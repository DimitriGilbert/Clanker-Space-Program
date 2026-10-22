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
//! field order.
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

/// The `[0,1)` draw — the `csp_core::uniform` scaling applied to a
/// stream position (the KDF reseeds per offer, then the fixed draw
/// order consumes the stream).
fn next01(rng: &mut impl Rng) -> f64 {
    (rng.next_u64() >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
}

/// The weighted-pick uniform index over `len` candidates.
fn pick(rng: &mut impl Rng, len: usize) -> usize {
    (next01(rng) * len as f64) as usize
}

/// Draw the concrete params for a template — the fixed per-kind
/// draw order (module doc).
fn draw_params(
    rng: &mut impl Rng,
    template: &ContractTemplate,
) -> Result<ContractParams, SimError> {
    match &template.generate.space {
        ContractParamSpace::SatellitePlacement {
            bodies,
            periapsis_alt_m,
            apoapsis_alt_m,
        } => {
            let body = bodies
                .get(pick(rng, bodies.len()))
                .ok_or(SimError::Fatal("contract gen: empty body space"))?
                .clone();
            let rp = periapsis_alt_m.0 + next01(rng) * (periapsis_alt_m.1 - periapsis_alt_m.0);
            let mut ra = apoapsis_alt_m.0 + next01(rng) * (apoapsis_alt_m.1 - apoapsis_alt_m.0);
            if ra < rp {
                // The clamp the space doc names: a drawn cap under
                // the floor would be unsatisfiable.
                ra = rp;
            }
            Ok(ContractParams::SatelliteOrbit {
                body,
                min_periapsis_alt_m: rp,
                max_apoapsis_alt_m: ra,
            })
        }
        ContractParamSpace::SurveyRun {
            experiments,
            bodies,
            min_units,
        } => Ok(ContractParams::SurveyData {
            experiment: experiments
                .get(pick(rng, experiments.len()))
                .ok_or(SimError::Fatal("contract gen: empty experiment space"))?
                .clone(),
            body: bodies
                .get(pick(rng, bodies.len()))
                .ok_or(SimError::Fatal("contract gen: empty body space"))?
                .clone(),
            min_units: *min_units,
        }),
        ContractParamSpace::PartTest { modules, regime } => {
            let module = modules
                .get(pick(rng, modules.len()))
                .ok_or(SimError::Fatal("contract gen: empty module space"))?
                .clone();
            let regime = match regime {
                RegimeSpace::Orbit { bodies, min_alt_m } => {
                    let body = bodies
                        .get(pick(rng, bodies.len()))
                        .ok_or(SimError::Fatal("contract gen: empty body space"))?
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
                        .ok_or(SimError::Fatal("contract gen: empty body space"))?
                        .clone(),
                },
            };
            Ok(ContractParams::PartTest { module, regime })
        }
        ContractParamSpace::Rescue { bodies } => Ok(ContractParams::Rescue {
            body: bodies
                .get(pick(rng, bodies.len()))
                .ok_or(SimError::Fatal("contract gen: empty body space"))?
                .clone(),
        }),
        // M8-C §3.6: draw the body, the resource, then the delivery
        // amount uniform in the authored range.
        ContractParamSpace::ColonyResupply {
            bodies,
            resources,
            amount,
        } => Ok(ContractParams::ColonyResupply {
            body: bodies
                .get(pick(rng, bodies.len()))
                .ok_or(SimError::Fatal("contract gen: empty body space"))?
                .clone(),
            resource: resources
                .get(pick(rng, resources.len()))
                .ok_or(SimError::Fatal("contract gen: empty resource space"))?
                .clone(),
            amount: amount.0 + next01(rng) * (amount.1 - amount.0),
        }),
    }
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
    pub(crate) fn publish_contracts(&mut self, shadow: &mut TickShadow) -> Result<(), SimError> {
        if self.contract_defs.is_empty()
            || self.contracts.offers.len() >= self.contract_cfg.board_size as usize
        {
            return Ok(());
        }
        self.shadow_contracts(shadow);
        while self.contracts.offers.len() < self.contract_cfg.board_size as usize {
            let index = self.contracts.offer_index;
            let Some(offer) = self.generate_offer(index)? else {
                break;
            };
            self.contracts.offer_index = index
                .checked_add(1)
                .ok_or(SimError::Fatal("contract offer_index overflow"))?;
            self.contracts.offers.insert(index, offer);
        }
        Ok(())
    }

    /// `AcceptContract` — the §3.5 advance + the §3.6 re-check: the
    /// offer must still be on the board, its template must exist,
    /// and its prerequisites must still hold (facts may have moved
    /// since generation). On success the offer leaves the board,
    /// the advance credits, the deadline event arms when authored,
    /// and the completion predicate evaluates once — an already-
    /// satisfied contract completes at acceptance (facts-based
    /// semantics: the sim state is the truth).
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
        let rewards = template.rewards;
        let deadline = template.deadline_days.map(|d| {
            self.clock
                .ticks()
                .saturating_add((d * DAY_S * TICK_HZ as f64).ceil().max(1.0) as u64)
        });
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
                params: o.params,
                deadline_tick: deadline,
            },
        );
        self.credit_funds(
            to_micros(rewards.advance_funds),
            FundsReason::ContractAdvance,
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
            } => {
                // Cumulative *returned* data at matching keys —
                // transmitted or recovered (closed).
                let units: f64 = self
                    .science
                    .keys
                    .iter()
                    .filter(|(k, s)| {
                        k.experiment == *experiment
                            && k.body == *body
                            && (s.transmitted || s.closed)
                    })
                    .map(|(_, s)| s.collected)
                    .sum();
                units >= *min_units
            }
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
            self.shadow_events(shadow);
            let heap = std::mem::take(&mut self.event_heap);
            self.event_heap = heap
                .into_iter()
                .filter(|e| !matches!(&e.kind, EventKind::ContractDeadline { contract } if *contract == id))
                .collect();
        }
        self.credit_funds(to_micros(reward), FundsReason::ContractCompletion, shadow);
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
            return Err(SimError::Recoverable("contract deadline: tick mismatch"));
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
            w.economy().funds_micros,
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
            w.economy().funds_micros,
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
        assert_eq!(w.economy().funds_micros, 0);
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
            .insert(BodyId::from("rask"));
        let w2 = World::restore(test_orbits(), None, changed).unwrap();
        assert_ne!(w2.state_hash().unwrap(), base_hash);
        let mut changed = base;
        changed.contracts.offer_index += 1;
        let w2 = World::restore(test_orbits(), None, changed).unwrap();
        assert_ne!(w2.state_hash().unwrap(), base_hash);
    }
}
