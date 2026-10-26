//! M7-D: the §3.6 contract system — the deterministic offer
//! generator, the fact-based prerequisites, the event-driven
//! completion evaluation, and the deadline failure path. This module
//! joins the `Contracts` slot of §2.6.5's subsystem order with the
//! program-economy book (it publishes after `publish_economy` inside
//! the slot).
//!
//! # Generation (§2.6.1 fixed arity)
//!
//! Offers draw from `{tag: CONTRACTS, entity: 0, counter:
//! offer_index}` — `ContractState::offer_index` is the serialized
//! monotonic counter §7.2 names ("contract `offer_index`"), so the
//! board is deterministic and not re-rollable by reload: index `i`
//! always draws the same template + params for a given
//! `(save_seed, eligible set)`. The board is lazily refilled by the
//! subsystem's publish step — `contract_offer_slots` entries — a pure
//! function of `(templates, facts, offer_index, save_seed)`, so the
//! same publish lands identically in every §2.6.1 partition. Draw
//! order is fixed: template (weighted pick over the prereq-eligible
//! set in ascending-id order), then the param draws in the authored
//! field order. A drawn satellite pair whose apoapsis cap lands under
//! its periapsis floor is ordered (the bands may overlap partially)
//! so every published offer stays satisfiable; a space that cannot
//! produce honest params at all — empty candidate lists, or a draw
//! that fails the restore gate's `params_ok` shape (finite
//! everywhere; `> 0` for the units/altitude/amount asks; `≥ 0` and
//! ordered for the satellite window) — is skipped: the draw is
//! deterministic per index, so the publish advances `offer_index`
//! past the poisoned draw, reports it on `degraded`, and moves on
//! (an all-poisoned install stops after `board_size` consecutive
//! skips and simply sits with a short board). Both are reachable
//! only through unvalidated `set_contracts` installs; the per-draw
//! gate is what keeps a poisoned space from publishing an offer the
//! snapshot restore would `Fatal` on — and the skip is what keeps it
//! from wedging the world.
//!
//! # Prerequisites — facts, not reputation
//!
//! §3.6 verbatim: "there is no reputation currency. Prerequisites
//! use concrete serialized facts." [`csp_data::ContractPrereq`]
//! evaluates against `TechState::researched`, the contract facts
//! record (completed-by-kind counts, the bodies-visited set), and
//! the crew roster's `Lost` count — checked at generation (offer
//! eligibility) and at `AcceptContract` (facts may have moved).
//!
//! # Completion — predicates on events, never polled
//!
//! §3.6: "completion is evaluated by the sim, as a predicate over
//! sim state checked on relevant events — never polled every tick."
//! The interval advance does NO contract work. Instead the sim calls
//! [`World::contract_notify`] at exactly the transitions a kind can
//! care about — orbit-achieved (`SpawnVessel`, `ToRails`, and the
//! §2.6.6 re-base, which covers `SoiTransition` and the residence
//! resolution), surface-latch, science banking, and vessel recovery
//! — and evaluates the subscribed contracts' predicates over current
//! state. `Rescue` is the one transition-keyed predicate (the
//! recovered vessel leaves the sim, so the event carries the body).
//! Because the predicates read current state, an already-satisfied
//! offer completes inside its own `AcceptContract` boundary —
//! documented on [`World::apply_accept_contract`].
//!
//! # Deadline
//!
//! `AcceptContract` schedules `EventKind::ContractDeadline`
//! (`Clamping`, §2.6.5) at `accept + deadline_days` when the
//! template authors one. Firing expires the contract: the record
//! leaves the board, `failed` counts, and `failure_penalty_funds`
//! is charged through the funds clamp (a drain, like salary —
//! §3.5's bankruptcy rule). Completion removes the pending event
//! so the restore pairing stays strict.
//!
//! # Serialized facts (§7.2 "completed-contract facts")
//!
//! [`ContractState::facts`] holds the prerequisite inputs the board
//! itself maintains: `completed`/`failed` counts per kind and the
//! `bodies_visited` set (stamped by the same orbit/latch triggers
//! the predicates ride). Hashed and serialized — a lost fact
//! silently re-locks or re-opens gated offers.

use std::collections::{BTreeMap, BTreeSet};

use rand_core::Rng;
use serde::{Deserialize, Serialize};

use csp_core::{SimError, StateHasher, StreamKey, StreamTag, TICK_HZ};
use csp_data::{
    BodyId, ContractKind, ContractParamSpace, ContractParams, ContractPrereq, ContractRegime,
    ContractTemplate, ContractTypeId, ModuleReq, RegimeSpace, ResourceId,
};

use crate::economy::{FundsReason, to_micros};
use crate::{EventKind, TickShadow, VesselId, World};

/// `balance.ron: contract_offer_slots` mirror for data-less worlds
/// — six offers on the board.
pub const DEFAULT_CONTRACT_BOARD_SIZE: u32 = 6;

/// The live-contract arena cap — bounds outstanding deadline/penalty
/// liability. `AcceptContract` rejects at the cap (Recoverable, like
/// every other cap); an instantly-completing record leaves `active`
/// in its own accept, so this does not throttle that path — the
/// board's own draw turnover does (see `apply_accept_contract`'s
/// doc).
pub const MAX_ACTIVE_CONTRACTS: usize = 32;

/// Sim-day in seconds — the deadline conversion.
const DAY_S: f64 = 86_400.0;

/// §2.2 sim-side contract identity — the `ContractDeadline` event's
/// arena key, monotonic, never reused (`next_contract_id` is
/// serialized per §7.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ContractId(pub u64);

/// One offer on the board — a drawn template + concrete params.
/// `offer` IS the `offer_index` that generated it: stable, unique,
/// never reused, and exactly what `AcceptContract` names.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContractOffer {
    /// The `offer_index` this drew at (the KDF counter — its id).
    pub index: u64,
    /// The template it drew.
    pub template: ContractTypeId,
    /// The concrete generated params.
    pub params: ContractParams,
}

/// An accepted contract — the live record the completion predicate
/// evaluates against.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ActiveContract {
    /// Its arena key.
    pub id: ContractId,
    /// The template it was accepted under.
    pub template: ContractTypeId,
    /// The drawn params — frozen at offer draw.
    pub params: ContractParams,
    /// The `ContractDeadline` event's tick, when the template
    /// authors a deadline. Pairs with exactly one pending event
    /// (the restore coherence check).
    pub deadline_tick: Option<u64>,
}

/// §7.2's "completed-contract facts" — the prerequisite inputs the
/// contract board maintains about the program's history.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ContractFacts {
    /// Contracts completed per kind — §3.6's "contracts completed
    /// by type".
    pub completed: BTreeMap<ContractKind, u32>,
    /// Contracts expired by deadline per kind — the failure half of
    /// the fact record.
    pub failed: BTreeMap<ContractKind, u32>,
    /// Bodies a vessel has orbited or surface-latched at — §3.6's
    /// "bodies visited". Stamped by the same triggers the
    /// completion predicates ride.
    pub bodies_visited: BTreeSet<BodyId>,
    /// M8-C §3.6's resupply fact — cumulative resource units a
    /// `TransferComplete` has landed in each colony's stores, keyed
    /// `(colony arena id, resource)`. Cumulative, not a level read:
    /// a colony that consumed what it was given still counts the
    /// delivery (the delivery is the contract's fact, not the
    /// inventory).
    pub colony_deliveries: BTreeMap<(u64, ResourceId), f64>,
}

/// The world's §3.6 contract book — everything the subsystem owns:
/// the offer board, the generator counter, the active-contract
/// arena and its next-id, and the fact record. Hashed + serialized.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ContractState {
    /// The live offer board, keyed by `offer_index` — deterministic
    /// and not re-rollable by reload (§2.6.1).
    pub offers: BTreeMap<u64, ContractOffer>,
    /// §7.2's serialized monotonic index — the `{CONTRACTS, 0,
    /// counter}` key's counter, one per generated offer.
    pub offer_index: u64,
    /// The accepted-contract arena, keyed by [`ContractId`].
    pub active: BTreeMap<ContractId, ActiveContract>,
    /// §7.2 next-id counter for the contract arena.
    pub next_contract_id: u64,
    /// The §3.6 fact record.
    pub facts: ContractFacts,
}

/// The §3.6 tunables — construction config (`balance.ron` in real
/// worlds; the `DEFAULT_*` mirrors otherwise), serialized + hashed
/// like `science_cfg` (the board size changes what generates).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContractConfig {
    /// The offer-board size the generator maintains.
    pub board_size: u32,
}

/// The `DEFAULT_*` config — `World::new()`'s no-data fallback.
pub fn default_config() -> ContractConfig {
    ContractConfig {
        board_size: DEFAULT_CONTRACT_BOARD_SIZE,
    }
}

impl ContractConfig {
    /// The `balance.ron` → config conversion (§8.3).
    pub fn from_balance(b: &csp_data::BalanceDef) -> Self {
        Self {
            board_size: b.contract_offer_slots,
        }
    }

    /// The snapshot-restore gate — a zero board would let a crafted
    /// snapshot pin the generator off.
    pub fn is_valid(&self) -> bool {
        self.board_size > 0
    }
}

/// A completion-relevant transition — the "relevant event" a
/// contract subscribes to (module doc). `OrbitAchieved` and
/// `SurfaceLatched` carry the vessel so the visit fact keys on it;
/// the predicates themselves read world state.
#[derive(Clone, Debug)]
pub(crate) enum ContractTrigger {
    /// A vessel's rails orbit was (re)stamped: spawn, `ToRails`,
    /// or an SOI re-base.
    OrbitAchieved(VesselId),
    /// A vessel latched onto a body's surface.
    SurfaceLatched(VesselId),
    /// Program science banked through transmit/recovery — the
    /// survey trigger.
    DataBanked,
    /// A `SurfaceLatched` vessel was recovered — carries the body
    /// it sat on (the vessel is gone; the event carries the fact).
    VesselRecovered(BodyId),
    /// M8-C §3.6: a `TransferComplete` landed resource units into a
    /// colony's stores — the `ColonyResupply` predicate's trigger.
    ColonyDelivery,
}

/// The `[0,1)` draw — `csp_core::uniform01` applied to a stream
/// position (the KDF reseeds per offer, then the fixed draw order
/// consumes the stream).
fn next01(rng: &mut impl Rng) -> f64 {
    csp_core::uniform01(rng)
}

/// The weighted-pick uniform index over `len` candidates.
fn pick(rng: &mut impl Rng, len: usize) -> usize {
    (next01(rng) * len as f64) as usize
}

/// The "honest params" predicate — the one shape BOTH sides of the
/// boundary enforce: [`draw_params`] runs it on the drawn values
/// before the params leave generation (a `Recoverable` publish
/// degradation), and `World::restore` runs it on every serialized
/// offer/active contract (a `Fatal` snapshot rejection — one
/// predicate, so generation can never publish what restore would
/// reject). Finite scalars throughout; `> 0` where the param is a
/// demand (survey units, regime altitude, resupply amount); `≥ 0`
/// and ordered for the satellite window.
pub(crate) fn params_ok(p: &ContractParams) -> bool {
    let ok = |v: f64| v.is_finite() && v >= 0.0;
    match p {
        ContractParams::SatelliteOrbit {
            min_periapsis_alt_m,
            max_apoapsis_alt_m,
            ..
        } => {
            ok(*min_periapsis_alt_m)
                && ok(*max_apoapsis_alt_m)
                && min_periapsis_alt_m <= max_apoapsis_alt_m
        }
        ContractParams::SurveyData { min_units, .. } => ok(*min_units) && *min_units > 0.0,
        ContractParams::PartTest { regime, .. } => match regime {
            ContractRegime::Orbit { min_alt_m, .. } => ok(*min_alt_m) && *min_alt_m > 0.0,
            ContractRegime::Landed { .. } => true,
        },
        ContractParams::Rescue { .. } => true,
        ContractParams::ColonyResupply { amount, .. } => ok(*amount) && *amount > 0.0,
    }
}

/// Draw the concrete params for a template — the fixed per-kind
/// draw order (module doc). A space that cannot produce honest
/// params — an empty candidate list, or a draw [`params_ok`]
/// rejects — is a `Recoverable` (construction-config defect:
/// `set_contracts` callers bypass the `csp-data` validators that
/// gate it), never a `Fatal`: the board publish degrades typed and
/// the world rolls back intact. Without the draw-time gate an
/// unvalidated install would publish the dishonest offer and
/// `World::restore` would `Fatal` on the snapshot (`snapshot:
/// offer with non-finite params`).
fn draw_params(
    rng: &mut impl Rng,
    template: &ContractTemplate,
) -> Result<ContractParams, SimError> {
    let params = match &template.generate.space {
        ContractParamSpace::SatellitePlacement {
            bodies,
            periapsis_alt_m,
            apoapsis_alt_m,
        } => {
            let body = bodies
                .get(pick(rng, bodies.len()))
                .ok_or(SimError::Recoverable("contract gen: empty body space"))?
                .clone();
            let mut rp = periapsis_alt_m.0 + next01(rng) * (periapsis_alt_m.1 - periapsis_alt_m.0);
            let mut ra = apoapsis_alt_m.0 + next01(rng) * (apoapsis_alt_m.1 - apoapsis_alt_m.0);
            if ra < rp {
                // The ordering rule the space doc names: a drawn cap
                // under the floor swaps into place, so the published
                // `[min_periapsis, max_apoapsis]` window is never
                // empty and the offer stays satisfiable. The `ra =
                // rp` clamp it replaces collapsed the window to a
                // single bit-exact altitude — completable only by a
                // circular orbit at exactly `e = 0`, `a − radius =
                // rp`, i.e. never in play.
                std::mem::swap(&mut rp, &mut ra);
            }
            ContractParams::SatelliteOrbit {
                body,
                min_periapsis_alt_m: rp,
                max_apoapsis_alt_m: ra,
            }
        }
        ContractParamSpace::SurveyRun {
            experiments,
            bodies,
            min_units,
        } => ContractParams::SurveyData {
            experiment: experiments
                .get(pick(rng, experiments.len()))
                .ok_or(SimError::Recoverable(
                    "contract gen: empty experiment space",
                ))?
                .clone(),
            body: bodies
                .get(pick(rng, bodies.len()))
                .ok_or(SimError::Recoverable("contract gen: empty body space"))?
                .clone(),
            min_units: *min_units,
        },
        ContractParamSpace::PartTest { modules, regime } => {
            let module = modules
                .get(pick(rng, modules.len()))
                .ok_or(SimError::Recoverable("contract gen: empty module space"))?
                .clone();
            let regime = match regime {
                RegimeSpace::Orbit { bodies, min_alt_m } => {
                    let body = bodies
                        .get(pick(rng, bodies.len()))
                        .ok_or(SimError::Recoverable("contract gen: empty body space"))?
                        .clone();
                    let alt = min_alt_m.0 + next01(rng) * (min_alt_m.1 - min_alt_m.0);
                    ContractRegime::Orbit {
                        body,
                        min_alt_m: alt,
                    }
                }
                RegimeSpace::Landed { bodies } => ContractRegime::Landed {
                    body: bodies
                        .get(pick(rng, bodies.len()))
                        .ok_or(SimError::Recoverable("contract gen: empty body space"))?
                        .clone(),
                },
            };
            ContractParams::PartTest { module, regime }
        }
        ContractParamSpace::Rescue { bodies } => ContractParams::Rescue {
            body: bodies
                .get(pick(rng, bodies.len()))
                .ok_or(SimError::Recoverable("contract gen: empty body space"))?
                .clone(),
        },
        // M8-C §3.6: draw the body, the resource, then the delivery
        // amount uniform in the authored range.
        ContractParamSpace::ColonyResupply {
            bodies,
            resources,
            amount,
        } => ContractParams::ColonyResupply {
            body: bodies
                .get(pick(rng, bodies.len()))
                .ok_or(SimError::Recoverable("contract gen: empty body space"))?
                .clone(),
            resource: resources
                .get(pick(rng, resources.len()))
                .ok_or(SimError::Recoverable("contract gen: empty resource space"))?
                .clone(),
            amount: amount.0 + next01(rng) * (amount.1 - amount.0),
        },
    };
    // Every drawn scalar — the authored `min_units` included — must
    // pass the restore gate's own predicate before the params leave
    // generation: a non-finite or non-positive band draw / ask off
    // an unvalidated install degrades the publish `Recoverable`
    // rather than printing an offer the snapshot restore fatals on.
    if !params_ok(&params) {
        return Err(SimError::Recoverable("contract gen: dishonest params"));
    }
    Ok(params)
}

/// One predicate of the fact set — `TechResearched` reads the tech
/// book, the rest read the contract facts / roster. The world-side
/// evaluator is [`World::contract_prereqs_met`].
fn prereq_met(
    p: &ContractPrereq,
    facts: &ContractFacts,
    researched: &BTreeSet<csp_data::TechNodeId>,
    crew_lost: u32,
) -> bool {
    match p {
        ContractPrereq::TechResearched(n) => researched.contains(n),
        ContractPrereq::ContractsCompleted { kind, count } => {
            facts.completed.get(kind).copied().unwrap_or(0) >= *count
        }
        ContractPrereq::BodyVisited(b) => facts.bodies_visited.contains(b),
        ContractPrereq::CrewLostAtMost(n) => crew_lost <= *n,
    }
}

/// Whether a vessel's craft carries `module` — the capability-keyed
/// check ([`ModuleReq`]'s doc: the weld record holds no part ids).
fn craft_has_module(vessel: &crate::Vessel, module: &ModuleReq) -> bool {
    let Some(craft) = &vessel.craft else {
        return false;
    };
    craft.parts.iter().any(|p| match module {
        ModuleReq::Lab => p.lab.is_some(),
        ModuleReq::DockPort => !p.dock_ports.is_empty(),
        ModuleReq::CrewSeat => p.crew_seats > 0,
        ModuleReq::Experiment(kind) => p.experiments.iter().any(|e| e.kind == *kind),
        ModuleReq::Tank => !p.tanks.is_empty(),
        ModuleReq::ThrustSite => p
            .force_sites
            .iter()
            .any(|s| s.kind == csp_phys::ForceSiteKind::Thrust),
    })
}

// ---------------------------------------------------------------------------
// Hash walk (§7.1) — hand-written, ordering is the schema.
// ---------------------------------------------------------------------------

fn hash_body(h: &mut StateHasher, b: &BodyId) {
    h.str(b.as_str());
}

fn hash_regime(h: &mut StateHasher, r: &ContractRegime) {
    match r {
        ContractRegime::Orbit { body, min_alt_m } => {
            h.u16(0);
            hash_body(h, body);
            h.f64(*min_alt_m);
        }
        ContractRegime::Landed { body } => {
            h.u16(1);
            hash_body(h, body);
        }
    }
}

fn hash_module(h: &mut StateHasher, m: &ModuleReq) {
    match m {
        ModuleReq::Lab => {
            h.u16(0);
        }
        ModuleReq::DockPort => {
            h.u16(1);
        }
        ModuleReq::CrewSeat => {
            h.u16(2);
        }
        ModuleReq::Experiment(k) => {
            h.u16(3);
            h.str(k.as_str());
        }
        ModuleReq::Tank => {
            h.u16(4);
        }
        ModuleReq::ThrustSite => {
            h.u16(5);
        }
    }
}

fn hash_params(h: &mut StateHasher, p: &ContractParams) {
    match p {
        ContractParams::SatelliteOrbit {
            body,
            min_periapsis_alt_m,
            max_apoapsis_alt_m,
        } => {
            h.u16(0);
            hash_body(h, body);
            h.f64(*min_periapsis_alt_m);
            h.f64(*max_apoapsis_alt_m);
        }
        ContractParams::SurveyData {
            experiment,
            body,
            min_units,
        } => {
            h.u16(1);
            h.str(experiment.as_str());
            hash_body(h, body);
            h.f64(*min_units);
        }
        ContractParams::PartTest { module, regime } => {
            h.u16(2);
            hash_module(h, module);
            hash_regime(h, regime);
        }
        ContractParams::Rescue { body } => {
            h.u16(3);
            hash_body(h, body);
        }
        ContractParams::ColonyResupply {
            body,
            resource,
            amount,
        } => {
            h.u16(4);
            hash_body(h, body);
            h.str(resource.as_str());
            h.f64(*amount);
        }
    }
}

/// The world-level [`ContractState`] walk — appended to
/// `state_hash` under `contract_cfg`.
pub fn hash_contracts(h: &mut StateHasher, c: &ContractState) {
    h.u64(c.offer_index);
    h.u32(c.offers.len() as u32);
    for (idx, o) in &c.offers {
        h.u64(*idx);
        h.u64(o.index);
        h.str(o.template.as_str());
        hash_params(h, &o.params);
    }
    h.u64(c.next_contract_id);
    h.arena(c.active.iter().map(|(id, a)| (id.0, a)), |h, a| {
        h.str(a.template.as_str());
        hash_params(h, &a.params);
        match a.deadline_tick {
            Some(t) => {
                h.u16(1);
                h.u64(t);
            }
            None => {
                h.u16(0);
            }
        }
    });
    // The fact record: completed/failed counts by kind (key order
    // is ContractKind's declared order — BTreeMap on the enum's
    // Ord), then the visit set.
    h.btree_map(
        &c.facts.completed,
        |h, k| {
            h.stable_enum(k);
        },
        |h, n| {
            h.u32(*n);
        },
    );
    h.btree_map(
        &c.facts.failed,
        |h, k| {
            h.stable_enum(k);
        },
        |h, n| {
            h.u32(*n);
        },
    );
    h.u32(c.facts.bodies_visited.len() as u32);
    for b in &c.facts.bodies_visited {
        hash_body(h, b);
    }
    // M8-C: the resupply delivery record — `(colony id, resource)`
    // → cumulative units, `BTreeMap` key order.
    h.btree_map(
        &c.facts.colony_deliveries,
        |h, (colony, resource)| {
            h.u64(*colony);
            h.str(resource.as_str());
        },
        |h, units| {
            h.f64(*units);
        },
    );
}

/// Lazy-shadow capture for the contract book (the
/// `shadow_economy` precedent).
pub(crate) fn shadow_contracts(world_c: &ContractState, slot: &mut Option<ContractState>) {
    if slot.is_none() {
        *slot = Some(world_c.clone());
    }
}

// ---------------------------------------------------------------------------
// World impl — generation, prereqs, commands, events, completion.
// ---------------------------------------------------------------------------

impl World {
    /// The contract book — observation only.
    pub fn contracts(&self) -> &ContractState {
        &self.contracts
    }

    /// The §2.7 save seed the run draws contract offers under.
    pub fn save_seed(&self) -> u64 {
        self.save_seed
    }

    /// The save seed — a construction-time write like
    /// [`Self::set_economy_config`]: call before the first
    /// `step_frame`. Serialized + hashed (the seed selects the
    /// offer draws).
    pub fn set_save_seed(&mut self, seed: u64) {
        self.save_seed = seed;
    }

    /// Install the contract templates — construction config (the
    /// `set_experiments` precedent: data-derived, handed in, never
    /// serialized or hashed; `WorldConfig` carries it).
    pub fn set_contracts(&mut self, defs: BTreeMap<ContractTypeId, ContractTemplate>) {
        self.contract_defs = defs;
    }

    /// The installed contract templates (construction config).
    pub fn contract_defs(&self) -> &BTreeMap<ContractTypeId, ContractTemplate> {
        &self.contract_defs
    }

    /// The contract tunables — the `set_economy_config` pattern.
    pub fn set_contract_config(&mut self, cfg: ContractConfig) {
        self.contract_cfg = cfg;
    }

    /// The configured contract tunables.
    pub fn contract_config(&self) -> ContractConfig {
        self.contract_cfg
    }

    /// The crew roster's `Lost` count — the `CrewLostAtMost`
    /// prerequisite's fact.
    fn crew_lost(&self) -> u32 {
        self.economy
            .crew
            .values()
            .filter(|c| c.status == crate::economy::CrewStatus::Lost)
            .count() as u32
    }

    /// The §3.6 fact-predicate evaluation — every entry of the
    /// template's `prerequisites` must hold.
    pub(crate) fn contract_prereqs_met(&self, template: &ContractTemplate) -> bool {
        let lost = self.crew_lost();
        template
            .prerequisites
            .iter()
            .all(|p| prereq_met(p, &self.contracts.facts, &self.tech.researched, lost))
    }

    /// The generator's weighted pick over the prereq-eligible
    /// templates, then the param draw — the
    /// `{CONTRACTS, 0, offer_index}` KDF seeding (§2.6.1). Returns
    /// `None` when nothing is eligible (the board just sits short).
    /// A picked template whose space cannot produce honest params
    /// degrades `Recoverable` out of the publish — see
    /// [`draw_params`].
    fn generate_offer(&self, index: u64) -> Result<Option<ContractOffer>, SimError> {
        let eligible: Vec<&ContractTemplate> = self
            .contract_defs
            .values()
            .filter(|t| t.generate.weight > 0.0 && self.contract_prereqs_met(t))
            .collect();
        if eligible.is_empty() {
            return Ok(None);
        }
        let mut rng = csp_core::stream(
            self.save_seed,
            StreamKey {
                tag: StreamTag::Contracts,
                entity: 0,
                counter: index,
            },
        );
        let total: f64 = eligible.iter().map(|t| t.generate.weight).sum();
        let mut u = next01(&mut rng) * total;
        let mut template = *eligible
            .last()
            .ok_or(SimError::Fatal("contract gen: eligible vanished"))?;
        for t in &eligible {
            u -= t.generate.weight;
            if u <= 0.0 {
                template = t;
                break;
            }
        }
        let params = draw_params(&mut rng, template)?;
        Ok(Some(ContractOffer {
            index,
            template: template.id.clone(),
            params,
        }))
    }

    /// The `Contracts` slot's offer publish — refill the board to
    /// `board_size` from successive `offer_index` counters (module
    /// doc). Pure function of `(defs, facts, offer_index, seed)` —
    /// the `publish_economy` lazily-published precedent.
    ///
    /// A poisoned draw (`generate_offer`'s `Recoverable` — an empty
    /// candidate space or a `params_ok` shape failure, reachable only
    /// through an unvalidated `set_contracts` install) is
    /// deterministic on `(seed, index)`: propagating it meant the
    /// same index re-drew and re-failed on EVERY later publish, and
    /// `step_frame` could never advance again — one bad template
    /// bricked the world. The draw is skipped instead: `offer_index`
    /// advances past it, the error rides the `degraded` channel, and
    /// the refill moves on. `board_size` CONSECUTIVE skips ends the
    /// pass — an all-poisoned install degrades to a short board, not
    /// a spin through `u64` counter space. `Fatal` still propagates:
    /// an impossible state is never a per-draw skip.
    pub(crate) fn publish_contracts(
        &mut self,
        shadow: &mut TickShadow,
        degraded: &mut Vec<SimError>,
    ) -> Result<(), SimError> {
        if self.contract_defs.is_empty()
            || self.contracts.offers.len() >= self.contract_cfg.board_size as usize
        {
            return Ok(());
        }
        self.shadow_contracts(shadow);
        let mut skips = 0u32;
        while self.contracts.offers.len() < self.contract_cfg.board_size as usize {
            let index = self.contracts.offer_index;
            match self.generate_offer(index) {
                Ok(Some(offer)) => {
                    self.contracts.offer_index = index
                        .checked_add(1)
                        .ok_or(SimError::Fatal("contract offer_index overflow"))?;
                    self.contracts.offers.insert(index, offer);
                    skips = 0;
                }
                Ok(None) => break,
                Err(e @ SimError::Fatal(_)) => return Err(e),
                Err(e) => {
                    degraded.push(e);
                    self.contracts.offer_index = index
                        .checked_add(1)
                        .ok_or(SimError::Fatal("contract offer_index overflow"))?;
                    skips += 1;
                    if skips >= self.contract_cfg.board_size {
                        break;
                    }
                }
            }
        }
        Ok(())
    }

    /// `AcceptContract` — the §3.5 advance + the §3.6 re-check: the
    /// offer must still be on the board, its template must exist,
    /// its prerequisites must still hold (facts may have moved
    /// since generation), and the active arena must not be at
    /// [`MAX_ACTIVE_CONTRACTS`]. On success the offer leaves the
    /// board, the advance credits, the deadline event arms when
    /// authored, and the completion predicate evaluates once — an
    /// already-satisfied contract completes at acceptance
    /// (facts-based semantics: the sim state is the truth).
    ///
    /// # Instant completion — v1 semantics, bounded
    ///
    /// The predicate reads CURRENT state, so an already-satisfied
    /// offer completes inside its own accept boundary — advance and
    /// completion credit together, and no deadline is armed. For
    /// the state-reading kinds (`SatelliteOrbit`, `PartTest`) that
    /// counts evidence predating acceptance: a vessel already in
    /// the regime satisfies a fresh accept. `Rescue` cannot
    /// instant-complete — its predicate is transition-keyed, so
    /// `trigger: None` yields `false`. This is the intended §3.6
    /// reading (prerequisites — not accept order — do the gating: a
    /// program that already flew the profile owns the fact).
    ///
    /// The cumulative-fact kinds are different: `SurveyData` and
    /// `ColonyResupply` are re-based to the *delta since accept* at
    /// signing — the ask becomes `baseline + amount` over the
    /// monotone fact counters, so pre-acceptance deliveries and
    /// returns can never satisfy them. Without the baseline, one
    /// landed resupply (or one banked survey) would instantly
    /// satisfy every future offer of that kind drawn under its
    /// threshold — an unbounded payout loop, one command per
    /// payout, on an inexhaustible `offer_index` supply.
    ///
    /// Residual bound, stated honestly: a state-predicate offer
    /// (`SatelliteOrbit`/`PartTest`) satisfied by pre-acceptance
    /// evidence still pays at accept, and [`MAX_ACTIVE_CONTRACTS`]
    /// does not throttle it (an instantly-completed record leaves
    /// `active`). What bounds the loop is the board itself: offers
    /// are drawn against the *live* predicate, so unsatisfiable
    /// draws accumulate on the board and throttle turnover — the
    /// faucet self-starves rather than paying at a fixed rate
    /// forever. The cap's real job is the outstanding deadline/
    /// penalty liability, not the instant-complete path.
    pub(crate) fn apply_accept_contract(
        &mut self,
        offer: u64,
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        let Some(o) = self.contracts.offers.get(&offer).cloned() else {
            return Err(SimError::Recoverable("accept: no such offer"));
        };
        let Some(template) = self.contract_defs.get(&o.template) else {
            return Err(SimError::Recoverable("accept: template not installed"));
        };
        if !self.contract_prereqs_met(template) {
            return Err(SimError::Recoverable("accept: prerequisites not met"));
        }
        // The active-arena cap: an unbounded `active` is unbounded
        // outstanding deadline/penalty liability — reject like every
        // other cap. An instantly-completing record leaves `active`
        // in this same apply, so this bounds live contracts, not the
        // (separately-handled) payout path.
        if self.contracts.active.len() >= MAX_ACTIVE_CONTRACTS {
            return Err(SimError::Recoverable("accept: active contract cap"));
        }
        // Re-base the cumulative-fact kinds to the delta since this
        // accept: the recorded ask becomes `baseline + amount` over
        // the monotone counters, so only post-acceptance work can
        // satisfy them (the doc above — the unbounded re-pay faucet
        // is closed). The saturating fold keeps `params_ok`'s finite
        // `> 0` shape on the re-based ask.
        let mut params = o.params.clone();
        match &mut params {
            ContractParams::ColonyResupply {
                body,
                resource,
                amount,
            } => {
                let baseline: f64 = self
                    .colonies
                    .values()
                    .filter(|c| c.body == *body)
                    .map(|c| {
                        self.contracts
                            .facts
                            .colony_deliveries
                            .get(&(c.id.0, resource.clone()))
                            .copied()
                            .unwrap_or(0.0)
                    })
                    .sum();
                *amount = (*amount + baseline).min(f64::MAX);
            }
            ContractParams::SurveyData {
                experiment,
                body,
                min_units,
            } => {
                *min_units =
                    (*min_units + self.survey_returned_units(experiment, body)).min(f64::MAX);
            }
            _ => {}
        }
        let rewards = template.rewards;
        let deadline = match template.deadline_days {
            Some(d) => {
                // The data validator gates `deadline_days` finite
                // and `> 0`, but `set_contracts` callers bypass it —
                // a NaN would slip `f64::max` and arm at +1 tick, a
                // `+inf`/non-positive day count likewise. Reject
                // typed rather than arm a lie.
                if !(d.is_finite() && d > 0.0) {
                    return Err(SimError::Recoverable(
                        "accept: non-finite or non-positive deadline_days",
                    ));
                }
                Some(
                    self.clock.ticks().saturating_add(
                        (d * DAY_S * TICK_HZ as f64)
                            .ceil()
                            .max(1.0)
                            .min(u64::MAX as f64) as u64,
                    ),
                )
            }
            None => None,
        };
        self.shadow_contracts(shadow);
        self.contracts.offers.remove(&offer);
        let id = ContractId(self.contracts.next_contract_id);
        self.contracts.next_contract_id = self
            .contracts
            .next_contract_id
            .checked_add(1)
            .ok_or(SimError::Fatal("contract id counter overflow"))?;
        self.contracts.active.insert(
            id,
            ActiveContract {
                id,
                template: o.template,
                params,
                deadline_tick: deadline,
            },
        );
        self.credit_funds(
            to_micros(rewards.advance_funds),
            FundsReason::ContractAdvance,
            shadow,
        );
        self.notify(
            crate::reliability::Notification {
                tick: self.clock.ticks(),
                vessel: crate::reliability::PROGRAM_VESSEL,
                node: None,
                kind: crate::reliability::NotificationKind::ContractAccepted { contract: id.0 },
            },
            shadow,
        );
        // The already-satisfied case: complete at acceptance rather
        // than arming a deadline for a done contract.
        if self.objective_met(&self.contracts.active[&id].params, None) {
            self.complete_contract(id, shadow)?;
        } else if let Some(tick) = deadline {
            self.schedule_event(EventKind::ContractDeadline { contract: id }, tick, shadow)?;
        }
        Ok(())
    }

    /// The completion predicate — evaluated on triggers (and once at
    /// acceptance with `trigger: None`). Returns `true` when the
    /// params' claim holds in current state. `Rescue` reads the
    /// recovery event's body (the vessel is gone — no state answers
    /// it), so `None`/any other trigger yields `false` for it.
    fn objective_met(&self, params: &ContractParams, trigger: Option<&ContractTrigger>) -> bool {
        match params {
            ContractParams::SatelliteOrbit {
                body,
                min_periapsis_alt_m,
                max_apoapsis_alt_m,
            } => self.vessels.values().any(|v| {
                if v.debris_since.is_some() {
                    return false;
                }
                let Some(o) = v.orbit() else { return false };
                if o.parent != *body || o.elements.eccentricity >= 1.0 {
                    return false;
                }
                let radius = self.orbits.body_radius(body).unwrap_or(0.0);
                let a = o.elements.semi_major_axis_m;
                let e = o.elements.eccentricity;
                let rp = a * (1.0 - e);
                let ra = a * (1.0 + e);
                rp - radius >= *min_periapsis_alt_m && ra - radius <= *max_apoapsis_alt_m
            }),
            ContractParams::SurveyData {
                experiment,
                body,
                min_units,
            } => self.survey_returned_units(experiment, body) >= *min_units,
            ContractParams::PartTest { module, regime } => self.vessels.values().any(|v| {
                if v.debris_since.is_some() || !craft_has_module(v, module) {
                    return false;
                }
                match regime {
                    ContractRegime::Orbit { body, min_alt_m } => {
                        let Some(o) = v.orbit() else { return false };
                        if o.parent != *body || o.elements.eccentricity >= 1.0 {
                            return false;
                        }
                        let radius = self.orbits.body_radius(body).unwrap_or(0.0);
                        let rp = o.elements.semi_major_axis_m * (1.0 - o.elements.eccentricity);
                        rp - radius >= *min_alt_m
                    }
                    ContractRegime::Landed { body } => match &v.state {
                        crate::VesselState::SurfaceLatched(p) => p.body == *body,
                        _ => false,
                    },
                }
            }),
            ContractParams::Rescue { body } => {
                matches!(trigger, Some(ContractTrigger::VesselRecovered(b)) if b == body)
            }
            // M8-C §3.6: cumulative deliveries into the stores of
            // every colony on `body` reach `amount`. The fact reads
            // the arena ids, the colony arena supplies the body
            // binding — a delivery to a colony on another body does
            // not count, and a decommissioned colony's deliveries
            // still do (they happened).
            ContractParams::ColonyResupply {
                body,
                resource,
                amount,
            } => {
                let delivered: f64 = self
                    .colonies
                    .values()
                    .filter(|c| c.body == *body)
                    .map(|c| {
                        self.contracts
                            .facts
                            .colony_deliveries
                            .get(&(c.id.0, resource.clone()))
                            .copied()
                            .unwrap_or(0.0)
                    })
                    .sum();
                delivered >= *amount
            }
        }
    }

    /// The `SurveyData` fact — cumulative *returned* units at keys
    /// matching `(experiment, body)` (the
    /// `ContractParams::SurveyData` accounting): `collected` is a
    /// cumulative-ever counter, so a key's returned share is
    /// `collected` minus what is still outstanding (pooled aboard
    /// any vessel, escrowed inside a live lab project). Only keys
    /// that produced a return — transmitted at least once, or
    /// closed by recovery — count at all; units lost with a vessel
    /// are not distinguished (the ledger keeps no per-key loss
    /// record). A `closed` key holds no outstanding units by
    /// invariant, so the formula reduces to `collected` for it.
    fn survey_returned_units(&self, experiment: &csp_data::ExperimentId, body: &BodyId) -> f64 {
        let cfg = &self.science_cfg;
        let clock = self.clock.ticks();
        self.science
            .keys
            .iter()
            .filter(|(k, s)| {
                k.experiment == *experiment && k.body == *body && (s.transmitted || s.closed)
            })
            .map(|(k, s)| {
                let pooled: f64 = self.vessels.values().map(|v| v.science.units_at(k)).sum();
                let escrowed: f64 = self
                    .science
                    .projects
                    .values()
                    .filter(|p| p.key == *k)
                    .map(|p| {
                        crate::research::data_at(
                            p.data_start,
                            p.rate_science_per_s,
                            p.throughput_data_per_s,
                            cfg.science_per_data,
                            clock.saturating_sub(p.start_tick) as f64 / TICK_HZ as f64,
                        )
                    })
                    .sum();
                (s.collected - pooled - escrowed).max(0.0)
            })
            .sum()
    }

    /// The event-driven completion pass — §3.6's "predicate over
    /// sim state checked on relevant events, never polled". Called
    /// at the transitions that can satisfy a kind: orbit-achieved,
    /// surface-latch, science banking, vessel recovery. Also stamps
    /// the fact record's `bodies_visited`.
    ///
    /// Ascending-`ContractId` evaluation order; a completed contract
    /// leaves the arena and credits its completion reward in the
    /// same boundary.
    pub(crate) fn contract_notify(
        &mut self,
        trigger: &ContractTrigger,
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        // The §3.6 visit fact rides the same transitions the
        // predicates do — stamped before evaluation so a
        // BodyVisited prereq sees the freshest fact.
        match trigger {
            ContractTrigger::OrbitAchieved(v) => {
                if let Some(body) = self
                    .vessels
                    .get(v)
                    .and_then(|v| v.orbit())
                    .map(|o| o.parent.clone())
                {
                    self.shadow_contracts(shadow);
                    self.contracts.facts.bodies_visited.insert(body);
                }
            }
            ContractTrigger::SurfaceLatched(v) => {
                if let Some(body) = self.vessels.get(v).and_then(|v| match &v.state {
                    crate::VesselState::SurfaceLatched(p) => Some(p.body.clone()),
                    _ => None,
                }) {
                    self.shadow_contracts(shadow);
                    self.contracts.facts.bodies_visited.insert(body);
                }
            }
            _ => {}
        }
        let done: Vec<ContractId> = self
            .contracts
            .active
            .iter()
            .filter(|(_, a)| self.objective_met(&a.params, Some(trigger)))
            .map(|(id, _)| *id)
            .collect();
        for id in done {
            self.complete_contract(id, shadow)?;
        }
        Ok(())
    }

    /// One contract's completion — the completion reward credits,
    /// the record leaves the arena, the fact count stamps, and its
    /// pending `ContractDeadline` (if any) drops so the event
    /// pairing stays strict.
    fn complete_contract(
        &mut self,
        id: ContractId,
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        let Some(c) = self.contracts.active.get(&id).cloned() else {
            return Ok(());
        };
        let reward = self
            .contract_defs
            .get(&c.template)
            .map(|t| t.rewards.completion_funds)
            .unwrap_or(0.0);
        self.shadow_contracts(shadow);
        self.contracts.active.remove(&id);
        *self
            .contracts
            .facts
            .completed
            .entry(c.params.kind())
            .or_insert(0) += 1;
        // A pending deadline is dead with the contract — drop it so
        // the restore pairing holds in both directions.
        if c.deadline_tick.is_some() {
            self.shadow_events_whole(shadow);
            let heap = std::mem::take(&mut self.event_heap);
            self.event_heap = heap
                .into_iter()
                .filter(|e| !matches!(&e.kind, EventKind::ContractDeadline { contract } if *contract == id))
                .collect();
        }
        self.credit_funds(to_micros(reward), FundsReason::ContractCompletion, shadow);
        self.notify(
            crate::reliability::Notification {
                tick: self.clock.ticks(),
                vessel: crate::reliability::PROGRAM_VESSEL,
                node: None,
                kind: crate::reliability::NotificationKind::ContractCompleted { contract: id.0 },
            },
            shadow,
        );
        Ok(())
    }

    /// `ContractDeadline` firing — the contract expires: the record
    /// leaves the arena, `failed` counts, and the authored penalty
    /// charges through the funds clamp (a drain — §3.5's bankruptcy
    /// rule, never a rejection). Idempotent: a deadline naming a
    /// non-active contract is a no-op (the completion path already
    /// drops pending events; the tolerance is defensive).
    pub(crate) fn fire_contract_deadline(
        &mut self,
        id: ContractId,
        event_tick: u64,
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        let Some(c) = self.contracts.active.get(&id).cloned() else {
            return Ok(());
        };
        if c.deadline_tick != Some(event_tick) {
            // The event tick IS the scheduled deadline — a record
            // disagreeing with it breaks the pairing invariant the
            // restore gate enforces (`snapshot: contract / deadline
            // event pairing mismatch`), and no legit path moves one
            // without the other. Same severity as that sibling
            // check: `Fatal`, never a degradation.
            return Err(SimError::Fatal("contract deadline: tick mismatch"));
        }
        let penalty = self
            .contract_defs
            .get(&c.template)
            .map(|t| t.rewards.failure_penalty_funds)
            .unwrap_or(0.0);
        self.shadow_contracts(shadow);
        self.contracts.active.remove(&id);
        *self
            .contracts
            .facts
            .failed
            .entry(c.params.kind())
            .or_insert(0) += 1;
        self.charge_funds(to_micros(penalty), FundsReason::ContractPenalty, shadow)?;
        self.notify(
            crate::reliability::Notification {
                tick: self.clock.ticks(),
                vessel: crate::reliability::PROGRAM_VESSEL,
                node: None,
                kind: crate::reliability::NotificationKind::ContractFailed { contract: id.0 },
            },
            shadow,
        );
        Ok(())
    }
}

#[cfg(test)]
// §2.13's no-panic rule covers shipping fallible paths; tests signal
// failure by panicking, so unwrap/expect stays legal inside test code.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::collections::BTreeMap;

    use csp_data::{
        BodyId, ContractGenerator, ContractKind, ContractParamSpace, ContractParams,
        ContractTemplate, KeplerElements,
    };

    use super::*;
    use crate::{SimCommand, World};

    /// One template for tests — the satellite contract over a
    /// synthetic body set.
    fn template(id: &str, kind: ContractKind, prereqs: Vec<ContractPrereq>) -> ContractTemplate {
        let space = match kind {
            ContractKind::SatellitePlacement => ContractParamSpace::SatellitePlacement {
                bodies: vec![BodyId::from("terrin")],
                periapsis_alt_m: (100_000.0, 200_000.0),
                apoapsis_alt_m: (100_000.0, 300_000.0),
            },
            ContractKind::SurveyRun => ContractParamSpace::SurveyRun {
                experiments: vec![csp_data::ExperimentId::from("exp.materials_study")],
                bodies: vec![BodyId::from("terrin")],
                min_units: 1.0,
            },
            ContractKind::PartTest => ContractParamSpace::PartTest {
                modules: vec![ModuleReq::CrewSeat],
                regime: RegimeSpace::Orbit {
                    bodies: vec![BodyId::from("terrin")],
                    min_alt_m: (100_000.0, 200_000.0),
                },
            },
            ContractKind::Rescue => ContractParamSpace::Rescue {
                bodies: vec![BodyId::from("terrin")],
            },
            ContractKind::ColonyResupply => ContractParamSpace::ColonyResupply {
                bodies: vec![BodyId::from("terrin")],
                resources: vec![ResourceId::from("ore")],
                amount: (10.0, 10.0),
            },
        };
        ContractTemplate {
            schema_version: 1,
            id: ContractTypeId::from(id),
            display_name: csp_data::LocKey::from("x"),
            description: csp_data::LocKey::from("x"),
            kind,
            generate: ContractGenerator { weight: 1.0, space },
            prerequisites: prereqs,
            rewards: csp_data::ContractRewards {
                advance_funds: 2.0,
                completion_funds: 10.0,
                failure_penalty_funds: 4.0,
            },
            deadline_days: Some(1.0),
        }
    }

    /// sol → terrin — a two-body model with a known 600 km datum.
    fn test_orbits() -> crate::orbit::PatchedConics {
        let mut bodies = BTreeMap::new();
        bodies.insert(
            BodyId::from("sol"),
            crate::orbit::BodyOrbit {
                mu_m3_per_s2: 1.0e18,
                soi_m: None,
                parent: None,
                elements: None,
            },
        );
        bodies.insert(
            BodyId::from("terrin"),
            crate::orbit::BodyOrbit {
                mu_m3_per_s2: 3.5316e12,
                soi_m: Some(1.0e8),
                parent: Some(BodyId::from("sol")),
                elements: Some(KeplerElements {
                    semi_major_axis_m: 1.0e10,
                    eccentricity: 0.0,
                    inclination_rad: 0.0,
                    ascending_node_rad: 0.0,
                    arg_periapsis_rad: 0.0,
                    mean_anomaly_epoch_rad: 0.0,
                    period_s: 1.0,
                }),
            },
        );
        let mut m = crate::orbit::PatchedConics::new(bodies);
        m.set_body_radius(&BodyId::from("terrin"), 600_000.0);
        m
    }

    /// A world with the two-body orbit model + one template.
    fn world_with(t: ContractTemplate) -> World {
        let mut w = World::with_system(test_orbits());
        w.set_contracts(BTreeMap::from([(t.id.clone(), t)]).into_iter().collect());
        w
    }

    /// Step one more interval pass — every pass re-publishes the
    /// board until it holds `board_size` offers.
    fn fill_board(w: &mut World) {
        w.step_frame(w.clock().ticks() + csp_core::TICK_HZ).unwrap();
    }

    /// A bound Terrin orbit with `a = radius + alt` — inside every
    /// generated band (radius 600 000, apsis 100–300 km).
    fn terrin_orbit(alt_m: f64) -> KeplerElements {
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

    /// Same seed → same offers; different seed → different draw.
    #[test]
    fn generation_is_deterministic_by_seed() {
        let mut a = world_with(template(
            "contract.satellite",
            ContractKind::SatellitePlacement,
            vec![],
        ));
        a.set_save_seed(7);
        fill_board(&mut a);
        let mut b = world_with(template(
            "contract.satellite",
            ContractKind::SatellitePlacement,
            vec![],
        ));
        b.set_save_seed(7);
        fill_board(&mut b);
        assert_eq!(a.contracts().offers, b.contracts().offers);
        assert_eq!(a.state_hash().unwrap(), b.state_hash().unwrap());
        assert_eq!(a.contracts().offers.len(), 6);
        assert_eq!(a.contracts().offer_index, 6);
        // Every stored offer names its own index.
        for (idx, o) in &a.contracts().offers {
            assert_eq!(*idx, o.index);
        }
        let mut c = world_with(template(
            "contract.satellite",
            ContractKind::SatellitePlacement,
            vec![],
        ));
        c.set_save_seed(8);
        fill_board(&mut c);
        assert_ne!(a.contracts().offers, c.contracts().offers);
    }

    /// An unmet prerequisite keeps a template off the board; meeting
    /// it (here the tech fact flips on) admits it.
    #[test]
    fn prereqs_gate_the_board() {
        let mut w = world_with(template(
            "contract.satellite",
            ContractKind::SatellitePlacement,
            vec![ContractPrereq::TechResearched(csp_data::TechNodeId::from(
                "tech.start",
            ))],
        ));
        fill_board(&mut w);
        assert!(w.contracts().offers.is_empty());
        w.tech
            .researched
            .insert(csp_data::TechNodeId::from("tech.start"));
        fill_board(&mut w);
        assert_eq!(w.contracts().offers.len(), 6);
        // And the prereq re-check gates acceptance: a fresh world
        // without the fact rejects a crafted accept.
        let mut w2 = world_with(template(
            "contract.satellite",
            ContractKind::SatellitePlacement,
            vec![ContractPrereq::TechResearched(csp_data::TechNodeId::from(
                "tech.start",
            ))],
        ));
        // Craft an offer directly (bypassing the generation gate —
        // acceptance must re-check on its own).
        w2.contracts.offers.insert(
            0,
            ContractOffer {
                index: 0,
                template: ContractTypeId::from("contract.satellite"),
                params: ContractParams::Rescue {
                    body: BodyId::from("terrin"),
                },
            },
        );
        w2.enqueue(
            crate::SourceClass::Ui,
            0,
            SimCommand::AcceptContract { offer: 0 },
        );
        let report = w2.step_frame(2 * csp_core::TICK_HZ).unwrap();
        assert!(w2.contracts().active.is_empty());
        assert_eq!(report.rejected.len(), 1);
    }

    /// Accept → advance credits; the contract enters the arena;
    /// the deadline event arms.
    #[test]
    fn accept_pays_advance_and_arms_deadline() {
        let mut w = world_with(template(
            "contract.satellite",
            ContractKind::SatellitePlacement,
            vec![],
        ));
        w.economy.funds_micros = 0;
        fill_board(&mut w);
        w.enqueue(
            crate::SourceClass::Ui,
            0,
            SimCommand::AcceptContract { offer: 0 },
        );
        w.step_frame(2 * csp_core::TICK_HZ).unwrap();
        assert_eq!(w.contracts().active.len(), 1);
        assert_eq!(
            w.economy.funds_micros,
            crate::economy::to_micros(2.0),
            "advance_funds 2.0 credited"
        );
        let c = w.contracts().active.values().next().unwrap();
        let deadline = c.deadline_tick.unwrap();
        assert_eq!(deadline, csp_core::TICK_HZ + 86_400 * csp_core::TICK_HZ);
        assert_eq!(
            w.economy().ledger.iter().last().unwrap().reason,
            FundsReason::ContractAdvance,
        );
    }

    /// The satellite completion: spawn a vessel into a matching
    /// orbit — the orbit-achieved trigger completes the contract
    /// and pays the completion reward, all inside the command
    /// boundary (event-driven, never polled).
    #[test]
    fn satellite_completes_on_orbit_achieved() {
        let mut w = world_with(template(
            "contract.satellite",
            ContractKind::SatellitePlacement,
            vec![],
        ));
        w.economy.funds_micros = 0;
        fill_board(&mut w);
        w.enqueue(
            crate::SourceClass::Ui,
            0,
            SimCommand::AcceptContract { offer: 0 },
        );
        w.step_frame(2 * csp_core::TICK_HZ).unwrap();
        // Long quiet advance: the active contract does NOT complete
        // by polling — nothing orbits yet.
        w.step_frame(60 * csp_core::TICK_HZ).unwrap();
        assert_eq!(w.contracts().active.len(), 1);
        // Every generated Terrin band sits inside 100–300 km: the
        // drawn orbit's peri/apo caps fit a 200 km circular orbit
        // iff rp ≥ min and ra ≤ max — draw-independent since the
        // generator clamps ra ≥ rp but the bands can still exclude
        // a fixed altitude. Spawn until one matches? No — read the
        // drawn band and spawn inside it.
        let c = w.contracts().active.values().next().unwrap();
        let ContractParams::SatelliteOrbit {
            min_periapsis_alt_m,
            max_apoapsis_alt_m,
            ..
        } = &c.params
        else {
            panic!("wrong params")
        };
        let alt = (min_periapsis_alt_m + max_apoapsis_alt_m) / 2.0;
        w.enqueue(
            crate::SourceClass::Ui,
            0,
            SimCommand::SpawnVessel {
                parent: BodyId::from("terrin"),
                elements: terrin_orbit(alt),
            },
        );
        w.step_frame(60 * csp_core::TICK_HZ + 1).unwrap();
        assert!(w.contracts().active.is_empty(), "contract completed");
        assert_eq!(
            w.contracts().facts.completed[&ContractKind::SatellitePlacement],
            1
        );
        assert_eq!(
            w.economy.funds_micros,
            crate::economy::to_micros(2.0 + 10.0)
        );
        assert!(
            w.contracts()
                .facts
                .bodies_visited
                .contains(&BodyId::from("terrin"))
        );
    }

    /// Deadline expiry: advance past `deadline_days` — the
    /// `ContractDeadline` fires, the record leaves the arena, the
    /// penalty charges (the clamp path), `failed` counts.
    #[test]
    fn deadline_expiry_fails_the_contract() {
        let mut w = world_with(template(
            "contract.satellite",
            ContractKind::SatellitePlacement,
            vec![],
        ));
        w.economy.funds_micros = 0;
        fill_board(&mut w);
        w.enqueue(
            crate::SourceClass::Ui,
            0,
            SimCommand::AcceptContract { offer: 0 },
        );
        w.step_frame(2 * csp_core::TICK_HZ).unwrap();
        // `MAX_TICKS_PER_FRAME` bounds one call — step until the
        // deadline passes.
        let mut saw_deadline = false;
        while w.clock().ticks() < 2 * 86_400 * csp_core::TICK_HZ {
            let report = w.step_frame(2 * 86_400 * csp_core::TICK_HZ).unwrap();
            saw_deadline |= report
                .fired_events
                .iter()
                .any(|e| matches!(e.kind, EventKind::ContractDeadline { .. }));
        }
        assert!(saw_deadline, "the ContractDeadline event fired");
        assert!(w.contracts().active.is_empty());
        assert_eq!(
            w.contracts().facts.failed[&ContractKind::SatellitePlacement],
            1
        );

        // Advance 2.0 in, penalty 4.0 out → the drain clamps at 0
        // and marks insolvency (§3.5's rule for drains).
        assert_eq!(w.economy.funds_micros, 0);
        assert!(w.economy().insolvent);
    }

    /// Snapshot/restore round-trip + the coherence gate: a crafted
    /// snapshot with a deadline pairing mismatch fatals.
    #[test]
    fn snapshot_round_trip_and_restore_gate() {
        let mut w = world_with(template(
            "contract.satellite",
            ContractKind::SatellitePlacement,
            vec![],
        ));
        fill_board(&mut w);
        w.enqueue(
            crate::SourceClass::Ui,
            0,
            SimCommand::AcceptContract { offer: 0 },
        );
        w.step_frame(2 * csp_core::TICK_HZ).unwrap();
        let snap = w.snapshot().unwrap();
        let mut w2 = World::restore(test_orbits(), None, snap.clone()).unwrap();
        w2.set_contracts(w.contract_defs().clone());
        assert_eq!(w.state_hash().unwrap(), w2.state_hash().unwrap());
        // The pairing gate: a deadline event pointing at a tick the
        // contract doesn't agree with fatals.
        let mut bad = snap.clone();
        bad.contracts
            .active
            .values_mut()
            .next()
            .unwrap()
            .deadline_tick = bad
            .contracts
            .active
            .values()
            .next()
            .unwrap()
            .deadline_tick
            .map(|t| t + 1);
        assert!(World::restore(test_orbits(), None, bad).is_err());
        // And a dangling offer index (≥ counter) fatals.
        let mut bad2 = snap;
        bad2.contracts.offer_index = 0;
        assert!(World::restore(test_orbits(), None, bad2).is_err());
    }

    /// Hash coverage: mutating any contract field changes the
    /// canonical hash.
    #[test]
    fn hash_covers_contract_state() {
        let mut w = world_with(template(
            "contract.satellite",
            ContractKind::SatellitePlacement,
            vec![],
        ));
        fill_board(&mut w);
        let base = w.snapshot().unwrap();
        let base_hash = w.state_hash().unwrap();
        let mut changed = base.clone();
        changed.save_seed += 1;
        let w2 = World::restore(test_orbits(), None, changed).unwrap();
        assert_ne!(w2.state_hash().unwrap(), base_hash);
        let mut changed = base.clone();
        changed
            .contracts
            .facts
            .bodies_visited
            .insert(BodyId::from("sol"));
        let w2 = World::restore(test_orbits(), None, changed).unwrap();
        assert_ne!(w2.state_hash().unwrap(), base_hash);
        let mut changed = base;
        changed.contracts.offer_index += 1;
        let w2 = World::restore(test_orbits(), None, changed).unwrap();
        assert_ne!(w2.state_hash().unwrap(), base_hash);
    }

    /// A satellite template whose bands overlap — `lo(apo) <
    /// lo(peri)` — or even sit fully inverted still generates
    /// satisfiable offers: the generator orders a crossed draw, so
    /// every published window is non-empty and a circular orbit at
    /// its low edge completes the contract.
    #[test]
    fn overlapping_bands_still_yield_satisfiable_offers() {
        let mut t = template(
            "contract.satellite",
            ContractKind::SatellitePlacement,
            vec![],
        );
        t.generate.space = ContractParamSpace::SatellitePlacement {
            bodies: vec![BodyId::from("terrin")],
            periapsis_alt_m: (100_000.0, 200_000.0),
            apoapsis_alt_m: (50_000.0, 150_000.0),
        };
        let mut w = world_with(t);
        w.economy.funds_micros = 0;
        fill_board(&mut w);
        assert_eq!(w.contracts().offers.len(), 6);
        for o in w.contracts().offers.values() {
            let ContractParams::SatelliteOrbit {
                min_periapsis_alt_m,
                max_apoapsis_alt_m,
                ..
            } = &o.params
            else {
                panic!("wrong params")
            };
            assert!(
                max_apoapsis_alt_m >= min_periapsis_alt_m,
                "the published window is never empty"
            );
        }
        // Satisfiable in play, not just non-empty: accept the first
        // offer and park a circular orbit at the window's low edge.
        let ContractParams::SatelliteOrbit {
            min_periapsis_alt_m,
            ..
        } = &w.contracts().offers[&0].params
        else {
            panic!("wrong params")
        };
        let alt = *min_periapsis_alt_m;
        w.enqueue(
            crate::SourceClass::Ui,
            0,
            SimCommand::AcceptContract { offer: 0 },
        );
        w.enqueue(
            crate::SourceClass::Ui,
            0,
            SimCommand::SpawnVessel {
                parent: BodyId::from("terrin"),
                elements: terrin_orbit(alt),
            },
        );
        w.step_frame(2 * csp_core::TICK_HZ).unwrap();
        assert!(
            w.contracts().active.is_empty(),
            "the contract completed on the low-edge circular orbit"
        );
        assert_eq!(
            w.contracts().facts.completed[&ContractKind::SatellitePlacement],
            1
        );
    }

    /// C6-F1 institutionalized: a poisoned draw (empty `bodies`
    /// space installed through `set_contracts`, which bypasses the
    /// data validators) used to re-fire forever — the failing index
    /// was re-drawn identically every publish, `step_frame` returned
    /// `Recoverable` every tick, and the world bricked permanently
    /// while the rollback kept it clean. Now the publish skips the
    /// poisoned index: the error rides `degraded`, `offer_index`
    /// advances past it, the board sits short, and the world keeps
    /// stepping.
    #[test]
    fn poisoned_draw_skips_instead_of_wedging() {
        let mut t = template(
            "contract.satellite",
            ContractKind::SatellitePlacement,
            vec![],
        );
        t.generate.space = ContractParamSpace::SatellitePlacement {
            bodies: vec![],
            periapsis_alt_m: (100_000.0, 200_000.0),
            apoapsis_alt_m: (150_000.0, 300_000.0),
        };
        let mut w = world_with(t);
        let rep = w.step_frame(csp_core::TICK_HZ).unwrap();
        assert!(
            rep.degraded
                .iter()
                .any(|e| matches!(e, SimError::Recoverable(m) if m.contains("empty body space"))),
            "the poisoned draw rode the degraded channel: {:?}",
            rep.degraded
        );
        let idx_after_first = w.contracts().offer_index;
        assert!(
            idx_after_first >= DEFAULT_CONTRACT_BOARD_SIZE as u64,
            "the publish skipped a full board's worth of indices: {idx_after_first}"
        );
        // And the world stays live — repeated frames don't re-wedge.
        w.step_frame(2 * csp_core::TICK_HZ).unwrap();
        assert!(w.clock().ticks() == 2 * csp_core::TICK_HZ);
    }

    /// C6-F2/C7-F4 institutionalized: the cumulative-fact kinds
    /// re-base to the delta since accept — pre-acceptance survey
    /// returns can no longer instantly satisfy a freshly-signed
    /// `SurveyRun` contract (the "re-pay the same returned units
    /// forever" faucet is closed). The recorded ask lands at
    /// `baseline + min_units`.
    #[test]
    fn survey_accept_baselines_past_returns() {
        let mut w = world_with(template("contract.survey", ContractKind::SurveyRun, vec![]));
        // 40 units already returned for the experiment/body the
        // template draws on (the `exp.materials_study`/`terrin`
        // candidate the space offers): a transmitted key with
        // nothing pooled aboard counts as returned.
        w.science.keys.insert(
            crate::research::DataKey {
                experiment: csp_data::ExperimentId::from("exp.materials_study"),
                body: BodyId::from("terrin"),
                biome: csp_data::BiomeId::from("bio.x"),
                situation: csp_data::Situation::SpaceLow,
            },
            crate::research::ScienceKeyState {
                collected: 40.0,
                transmitted: true,
                closed: false,
            },
        );
        fill_board(&mut w);
        let start = w.economy.funds_micros;
        w.enqueue(
            crate::SourceClass::Ui,
            0,
            SimCommand::AcceptContract { offer: 0 },
        );
        w.step_frame(2 * csp_core::TICK_HZ).unwrap();
        // Not completed — only the advance was paid.
        assert_eq!(
            w.contracts().active.len(),
            1,
            "pre-acceptance returns are baseline"
        );
        assert!(w.contracts().facts.completed.is_empty());
        let active = w.contracts().active.values().next().unwrap();
        let ContractParams::SurveyData { min_units, .. } = active.params else {
            panic!("wrong params")
        };
        assert!(
            min_units > 40.0,
            "the ask re-based past the 40 already returned: {min_units}"
        );
        // Advance was credited; the completion reward was not.
        assert!(w.economy.funds_micros > start);
    }

    /// The active-arena cap: `MAX_ACTIVE_CONTRACTS` live contracts is
    /// the bound; the next accept refuses `Recoverable` rather than
    /// grow unbounded deadline/penalty liability.
    #[test]
    fn active_contract_cap_rejects() {
        let w = world_with(template(
            "contract.satellite",
            ContractKind::SatellitePlacement,
            vec![],
        ));
        let mut snap = w.snapshot().unwrap();
        for i in 0..crate::contracts::MAX_ACTIVE_CONTRACTS {
            snap.contracts.active.insert(
                crate::ContractId(i as u64),
                crate::ActiveContract {
                    id: crate::ContractId(i as u64),
                    template: ContractTypeId::from("contract.satellite"),
                    params: ContractParams::SatelliteOrbit {
                        body: BodyId::from("terrin"),
                        min_periapsis_alt_m: 1.0,
                        max_apoapsis_alt_m: 2.0,
                    },
                    deadline_tick: None,
                },
            );
        }
        snap.contracts.next_contract_id = crate::contracts::MAX_ACTIVE_CONTRACTS as u64;
        let mut w = World::restore(test_orbits(), None, snap).unwrap();
        fill_board(&mut w);
        w.enqueue(
            crate::SourceClass::Ui,
            0,
            SimCommand::AcceptContract { offer: 0 },
        );
        let rep = w.step_frame(w.clock().ticks() + csp_core::TICK_HZ).unwrap();
        assert_eq!(
            rep.rejected.len(),
            1,
            "the capped accept was rejected: {rep:?}"
        );
        assert_eq!(
            w.contracts().active.len(),
            crate::contracts::MAX_ACTIVE_CONTRACTS,
            "the cap held"
        );
        // (The consumed offer is the accept path's long-standing
        // contract — `offers.remove` precedes the checks; a rejected
        // accept does not restore it.)
    }

    /// A `deadline_days` that is not finite-and-positive (NaN, ±inf,
    /// zero, negative) is a construction-config defect: acceptance
    /// rejects `Recoverable` rather than arming a deadline at +1
    /// tick or `u64::MAX`.
    #[test]
    fn non_finite_deadline_days_rejects_at_accept() {
        for d in [f64::NAN, f64::INFINITY, 0.0, -2.0] {
            let mut t = template(
                "contract.satellite",
                ContractKind::SatellitePlacement,
                vec![],
            );
            t.deadline_days = Some(d);
            let mut w = world_with(t);
            fill_board(&mut w);
            assert_eq!(w.contracts().offers.len(), 6);
            w.enqueue(
                crate::SourceClass::Ui,
                0,
                SimCommand::AcceptContract { offer: 0 },
            );
            let report = w.step_frame(2 * csp_core::TICK_HZ).unwrap();
            assert_eq!(report.rejected.len(), 1, "deadline_days {d} rejected");
            assert!(w.contracts().active.is_empty());
        }
    }

    /// A live `ContractDeadline` firing whose tick disagrees with
    /// the record's `deadline_tick` is the same pairing violation
    /// the restore gate fatals on — `Fatal`, not a degradation.
    #[test]
    fn deadline_tick_mismatch_is_fatal() {
        let mut w = world_with(template(
            "contract.satellite",
            ContractKind::SatellitePlacement,
            vec![],
        ));
        w.contracts.active.insert(
            ContractId(0),
            ActiveContract {
                id: ContractId(0),
                template: ContractTypeId::from("contract.satellite"),
                params: ContractParams::Rescue {
                    body: BodyId::from("terrin"),
                },
                deadline_tick: Some(1_000),
            },
        );
        let mut shadow = crate::TickShadow::capture(&w);
        let err = w
            .fire_contract_deadline(ContractId(0), 999, &mut shadow)
            .unwrap_err();
        assert!(matches!(err, SimError::Fatal(_)), "got {err}");
        // The matching tick, by contrast, expires it normally.
        let mut shadow = crate::TickShadow::capture(&w);
        w.fire_contract_deadline(ContractId(0), 1_000, &mut shadow)
            .unwrap();
        assert!(w.contracts().active.is_empty());
    }

    /// The survey predicate counts units *returned*, not merely
    /// collected: pooled-aboard units at a transmitted key do not
    /// count until they actually come home.
    #[test]
    fn survey_counts_returned_not_collected() {
        let key = crate::research::DataKey {
            experiment: csp_data::ExperimentId::from("exp.materials_study"),
            body: BodyId::from("terrin"),
            biome: csp_data::BiomeId::from("bio.x"),
            situation: csp_data::Situation::SpaceLow,
        };
        let survey_world = |min_units: f64| {
            let mut t = template("contract.survey", ContractKind::SurveyRun, vec![]);
            if let ContractParamSpace::SurveyRun {
                min_units: slot, ..
            } = &mut t.generate.space
            {
                *slot = min_units;
            }
            let mut w = world_with(t);
            // The key saw a return (transmitted) after collecting
            // 100 units; 60 units sit pooled aboard a vessel —
            // outstanding, not returned.
            w.science.keys.insert(
                key.clone(),
                crate::research::ScienceKeyState {
                    collected: 100.0,
                    transmitted: true,
                    closed: false,
                },
            );
            fill_board(&mut w);
            w.enqueue(
                crate::SourceClass::Ui,
                0,
                SimCommand::SpawnVessel {
                    parent: BodyId::from("terrin"),
                    elements: terrin_orbit(200_000.0),
                },
            );
            w.step_frame(2 * csp_core::TICK_HZ).unwrap();
            let vid = *w.vessels.keys().next().unwrap();
            w.vessels
                .get_mut(&vid)
                .unwrap()
                .science
                .data
                .entry(csp_phys::PartNodeId(0))
                .or_default()
                .insert(key.clone(), 60.0);
            w
        };
        // 100 collected − 60 still aboard = 40 returned. Under the
        // accept-time baseline (the C6-F2 faucet fix) neither ask
        // instant-completes — the 40 returned units are the *start*
        // line — and the recorded ask lands at `40 + min_units`: the
        // 50-ask contract records 90, the 40-ask contract 80.
        let mut w = survey_world(50.0);
        w.enqueue(
            crate::SourceClass::Ui,
            0,
            SimCommand::AcceptContract { offer: 0 },
        );
        w.step_frame(3 * csp_core::TICK_HZ).unwrap();
        let a = w.contracts().active.values().next().unwrap();
        let ContractParams::SurveyData { min_units, .. } = a.params else {
            panic!("wrong params")
        };
        assert!(
            (min_units - 90.0).abs() < 1e-9,
            "50-ask re-based over the 40 returned: {min_units}"
        );
        let mut w = survey_world(40.0);
        w.enqueue(
            crate::SourceClass::Ui,
            0,
            SimCommand::AcceptContract { offer: 0 },
        );
        w.step_frame(3 * csp_core::TICK_HZ).unwrap();
        let a = w.contracts().active.values().next().unwrap();
        let ContractParams::SurveyData { min_units, .. } = a.params else {
            panic!("wrong params")
        };
        assert!(
            (min_units - 80.0).abs() < 1e-9,
            "40-ask re-based over the 40 returned: {min_units}"
        );
        assert!(
            w.contracts().facts.completed.is_empty(),
            "pre-acceptance returns never complete the ask"
        );
    }

    /// The `params_ok` draw gate covers EVERY drawn param, not just
    /// the satellite bands: a poisoned space — a NaN/≤0 ask or a
    /// band that draws one, reachable only through unvalidated
    /// `set_contracts` installs — degrades the publish `Recoverable`
    /// rather than printing an offer `World::restore` would `Fatal`
    /// on (`snapshot: offer with non-finite params`).
    #[test]
    fn dishonest_param_spaces_reject_recoverable() {
        let spaces = [
            // A NaN authored survey ask.
            (
                ContractKind::SurveyRun,
                ContractParamSpace::SurveyRun {
                    experiments: vec![csp_data::ExperimentId::from("exp.materials_study")],
                    bodies: vec![BodyId::from("terrin")],
                    min_units: f64::NAN,
                },
            ),
            // A zero survey ask.
            (
                ContractKind::SurveyRun,
                ContractParamSpace::SurveyRun {
                    experiments: vec![csp_data::ExperimentId::from("exp.materials_study")],
                    bodies: vec![BodyId::from("terrin")],
                    min_units: 0.0,
                },
            ),
            // A fully-below-datum satellite band.
            (
                ContractKind::SatellitePlacement,
                ContractParamSpace::SatellitePlacement {
                    bodies: vec![BodyId::from("terrin")],
                    periapsis_alt_m: (-200_000.0, -100_000.0),
                    apoapsis_alt_m: (-150_000.0, -50_000.0),
                },
            ),
            // A part-test orbit band whose draw NaNs out (`-inf`
            // edge through the `lo + u·(hi − lo)` scale).
            (
                ContractKind::PartTest,
                ContractParamSpace::PartTest {
                    modules: vec![ModuleReq::CrewSeat],
                    regime: RegimeSpace::Orbit {
                        bodies: vec![BodyId::from("terrin")],
                        min_alt_m: (f64::NEG_INFINITY, 0.0),
                    },
                },
            ),
            // A zero-ask resupply band.
            (
                ContractKind::ColonyResupply,
                ContractParamSpace::ColonyResupply {
                    bodies: vec![BodyId::from("terrin")],
                    resources: vec![ResourceId::from("ore")],
                    amount: (0.0, 0.0),
                },
            ),
        ];
        for (kind, space) in spaces {
            let mut t = template("contract.x", kind, vec![]);
            t.generate.space = space;
            let mut w = world_with(t);
            // The poisoned draw is skipped (C6-F1): it rides
            // `degraded`, the board sits short, and the frame
            // completes — a publish that Err'd here bricked the world.
            let rep = w.step_frame(csp_core::TICK_HZ).unwrap();
            assert!(
                rep.degraded.iter().any(
                    |e| matches!(e, SimError::Recoverable(m) if m.contains("dishonest params"))
                ),
                "{kind:?} poisoned space degraded, got {:?}",
                rep.degraded
            );
            assert!(w.contracts().offers.is_empty());
        }
    }

    /// The survey predicate's OTHER outstanding arm: units escrowed
    /// inside a live lab project are subtracted through the `data_at`
    /// live projection, same as pooled-aboard units — a key that
    /// collected 100 units with 60 still escrowed has returned 40.
    #[test]
    fn survey_subtracts_escrowed_project_units() {
        let key = crate::research::DataKey {
            experiment: csp_data::ExperimentId::from("exp.materials_study"),
            body: BodyId::from("terrin"),
            biome: csp_data::BiomeId::from("bio.x"),
            situation: csp_data::Situation::SpaceLow,
        };
        let survey_world = |min_units: f64| {
            let mut t = template("contract.survey", ContractKind::SurveyRun, vec![]);
            if let ContractParamSpace::SurveyRun {
                min_units: slot, ..
            } = &mut t.generate.space
            {
                *slot = min_units;
            }
            let mut w = world_with(t);
            // The key saw a return (transmitted) after collecting
            // 100 units; a live lab project escrows 60 of them.
            // `start_tick` sits ahead of the clock so the
            // `saturating_sub` elapsed is 0 — the closed form
            // escrows the full `data_start` at whatever tick the
            // predicate evaluates, keeping the ask deterministic.
            w.science.keys.insert(
                key.clone(),
                crate::research::ScienceKeyState {
                    collected: 100.0,
                    transmitted: true,
                    closed: false,
                },
            );
            w.science.projects.insert(
                crate::research::ResearchId(0),
                crate::research::ResearchProject {
                    id: crate::research::ResearchId(0),
                    vessel: VesselId(0),
                    lab_node: csp_phys::PartNodeId(0),
                    key: key.clone(),
                    researchers: vec![],
                    rate_science_per_s: 1.0,
                    throughput_data_per_s: 10.0,
                    start_tick: u64::MAX,
                    data_start: 60.0,
                    exhaust_tick: u64::MAX,
                },
            );
            fill_board(&mut w);
            w
        };
        // 100 collected − 60 escrowed = 40 returned. Under the
        // accept-time baseline (the C6-F2 faucet fix) neither ask
        // instant-completes — the returned units are the start line —
        // and the recorded ask re-bases to `40 + min_units`.
        let mut w = survey_world(50.0);
        w.enqueue(
            crate::SourceClass::Ui,
            0,
            SimCommand::AcceptContract { offer: 0 },
        );
        w.step_frame(3 * csp_core::TICK_HZ).unwrap();
        assert_eq!(
            w.contracts().active.len(),
            1,
            "escrowed units are not returned"
        );
        let mut w = survey_world(40.0);
        w.enqueue(
            crate::SourceClass::Ui,
            0,
            SimCommand::AcceptContract { offer: 0 },
        );
        w.step_frame(3 * csp_core::TICK_HZ).unwrap();
        let a = w.contracts().active.values().next().unwrap();
        let ContractParams::SurveyData { min_units, .. } = a.params else {
            panic!("wrong params")
        };
        assert!(
            (min_units - 80.0).abs() < 1e-9,
            "40-ask re-based over the 40 returned: {min_units}"
        );
        assert!(w.contracts().facts.completed.is_empty());
    }
}
