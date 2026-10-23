//! M7-A: the §3.2 science economy — the data model and the lab's
//! data→science conversion, built as the `Research` slot of §2.6.5's
//! subsystem order.
//!
//! # The data inventory (§3.2: "data as an inventory, not a currency")
//!
//! An `Experiment` module run in a `(body, biome, situation)` context
//! yields `data_units` of a typed [`DataRecord`] — pooled on the
//! vessel as `Vessel::science.data`, keyed by *producing node* so the
//! pool partitions/merges with the part graph exactly like
//! `tank_contents` (jettison loses the node's records; breakup and
//! undock split them with the fragment; latch merge pools them).
//!
//! - **Soft cap** (diminishing repeats): the ledger
//!   `ScienceState.keys` tracks `collected` per full
//!   `(experiment, body, biome, situation)` key while the authored
//!   `cap_key` is looked up per `(experiment, body, situation)`
//!   ([`ExperimentDef::cap_for`]). §3.2 writes the yield rule as a
//!   rate `cap·exp(−k·collected/cap)·run_units`; we apply its exact
//!   integral — treating the key as a saturating store:
//!   `C' = C + (cap − C)·(1 − exp(−k·u/cap))`. For a run of `cap/k`
//!   units this reproduces the plan's quoted sequence exactly:
//!   63.2% / 86.5% / 95.0% of cap after runs 1–3. Data banked can
//!   never exceed `cap` — the asymptote is the soft cap.
//! - **Transmission vs recovery**: `TransmitData` banks
//!   `units·transmit_ratio` science and drains `ElectricCharge`
//!   (`transmit_ec_per_data_unit`, balance.ron); `RecoverData` banks
//!   100% of the recovered units — or `1 − ratio` when the key was
//!   previously transmitted ("a transmitted key can later be
//!   completed by recovery for the remainder") — and closes the key:
//!   no further runs, transmits, or projects on it. Recovery requires
//!   a `SurfaceLatched` residence (the physical sample must be down);
//!   the launch-site/recovery mechanics of the later "recovery"
//!   sub-phase will tighten where on a body it may happen.
//!
//! # The lab's two-regime closed form (§3.2, verbatim math)
//!
//! `rate(t) = R · min(1, D(t)/T)` science/s, `R = Σ skill ·
//! lab_multiplier`, `T = throughput_data_per_s`, `η` =
//! `lab_science_per_data` (science points per data unit). `dD = −dS/η`:
//!
//! - abundant (`D ≥ T`): `rate = R`, `D(t) = D0 − R·t/η` — linear;
//! - scarce (`D < T`): `D(t) = D0·exp(−R·t/(ηT))` — exponential, never
//!   reaching zero, so **exhaustion is `D < data_epsilon`** (1e-6)
//!   giving the finite event time `t = (ηT/R)·ln(D0/ε)` (plus the
//!   linear head `η(D0−T)/R` when `D0 > T`).
//!
//! # Event-driven, not interval-integrated
//!
//! The closed form makes the subsystem fully event-driven — the same
//! class the orbits occupy: a project's stamped state (`start_tick`,
//! `data_start`, frozen `R`/`T`) never changes inside an interval, and
//! `D(t)`/accrued science evaluate purely at any tick. Mutation happens
//! only at absolute-tick boundaries — `StartResearch` (escrow +
//! schedule) and the exhaustion pair — so the §2.6.1 ×1-vs-×10⁵
//! equivalence holds by construction and the `Research` arm of
//! `advance_interval_subsystems` is an honest no-op.
//!
//! At `StartResearch` the record's pool units are *escrowed* into the
//! project (`data_start`) and TWO same-tick events are scheduled at the
//! predicted exhaustion tick — `ResearchDataExhausted` (lower seq,
//! fires first: banks `η·D0` and consumes the record) then
//! `ResearchProjectComplete` (the player-facing completion
//! notification; both `Clamping` per §2.6.5/§3.2). A project rescheduled
//! or interrupted leaves orphan events that fire as no-ops — the
//! `TransferComplete` precedent.
//!
//! Interruption rules (documented v1 scope): jettison of the lab node
//! finalizes the project (science accrued to the tick banks, the
//! escrowed remainder leaves with the part); breakup/undock finalize a
//! project whose lab node leaves the vessel, its remaining `D(now)`
//! returning to the fragment's pool under the lab node; a latch merge
//! re-seats a secondary-vessel project onto the merged vessel
//! (`vessel → prim`, `lab_node → remap`) and it keeps running —
//! `merge_graphs` preserves primary node ids so primary projects need
//! no remap; vessel destruction drops its projects and their pending
//! events without accrual (the lab burned — work-in-progress is lost).
//!
//! # v1 simplifications (documented)
//!
//! - `RunExperiment` carries the caller-derived `(biome, situation)` —
//!   the sim does not load biome maps, and the situation boundary
//!   definitions (low/high splits) are presentation-side math. The
//!   command is §7.6-logged like `WatchTransferWindow`'s planner
//!   output, so this is exactly as replayable; the apply validates
//!   residence class + the module's `SituationMask` only.
//! - Researcher `skill` rides `StartResearch` (0..=5 per §3.5) — the
//!   real crew arena with per-member skills is a later M7 sub-phase;
//!   the command value is the logged truth until then.
//! - `RecoverData` requires `SurfaceLatched` — recovery *site*
//!   mechanics (which body/site credits funds too) are the recovery
//!   sub-phase's business.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use csp_core::{SimError, StateHasher};
use csp_data::{BiomeId, BodyId, ExperimentDef, ExperimentId, ResourceId, Situation};
use csp_phys::{LabSpec, PartNodeId};

use crate::transfers::CrewId;
use crate::{TickShadow, Vessel, VesselId, VesselState, World};

/// The ElectricCharge resource a transmission drains (§3.2). The
/// resource *name* is the resources.ron id — authored data could
/// rename it, in which case transmits simply find no charge aboard.
pub const ELECTRIC_CHARGE: &str = "electric_charge";

/// §2.2 sim-side research-project identity — monotonic `u64`, never
/// reused (`next_id` is serialized per §7.2). What a project's
/// `ResearchDataExhausted`/`ResearchProjectComplete` events name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ResearchId(pub u64);

/// The §3.2/§2.2 science key — `(experiment, body, biome, situation)`.
/// `Ord` derives on field order: the `BTreeMap` key order IS the
/// canonical enumeration (§2.2 determinism domain).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DataKey {
    /// The experiment kind that produced the data.
    pub experiment: ExperimentId,
    /// The body the context is on/above.
    pub body: BodyId,
    /// The biome — may be an empty id for bodies/contexts without
    /// surface biomes (space situations).
    pub biome: BiomeId,
    /// The §3.2 situation leg.
    pub situation: Situation,
}

/// The program-side per-key ledger — §3.2's "soft cap on *cumulative*
/// data per key" and "recovered-vs-transmitted is tracked per key".
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ScienceKeyState {
    /// Cumulative data collected at this key under the diminishing
    /// curve — bounded above by `cap_key` asymptotically.
    pub collected: f64,
    /// Whether any record at this key was ever transmitted (recovery
    /// then pays only the `1 − ratio` remainder).
    pub transmitted: bool,
    /// Recovery closes the key — no further collection or cashout.
    pub closed: bool,
}

/// A vessel's science bookkeeping — serialized + hashed with the
/// vessel, partitioned/merged with the part graph like
/// `tank_contents`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct VesselScience {
    /// Data units aboard: producing node → key → remaining units.
    /// Units a running project holds are escrowed OUT of this map
    /// (they live on the [`ResearchProject`]); a transmit/recover
    /// addresses the pool only.
    pub data: BTreeMap<PartNodeId, BTreeMap<DataKey, f64>>,
    /// `(node, experiment)` pairs already run — the
    /// `reusable: false` consumption marker. Rides the part.
    pub spent: BTreeSet<(PartNodeId, ExperimentId)>,
}

impl VesselScience {
    /// Total aboard units at `key` (Σ over producing nodes).
    pub fn units_at(&self, key: &DataKey) -> f64 {
        self.data
            .values()
            .map(|m| m.get(key).copied().unwrap_or(0.0))
            .sum()
    }
}

/// One assigned researcher — `(crew, skill-at-assignment)`. §3.5's
/// crew model gives `skill ∈ 0..=5`; until the M7 crew arena lands the
/// skill rides the `StartResearch` command (module doc).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResearcherAssign {
    /// The crew member aboard the vessel.
    pub crew: CrewId,
    /// Skill at assignment time — frozen into the project's `R`.
    pub skill: u8,
}

/// An in-flight lab conversion — the hashed+serialized process record,
/// the `transfers::ActiveTransfer` precedent applied to §3.2. `D(t)`
/// evaluates in closed form from `(data_start, start_tick)`; nothing in
/// the record changes inside an interval.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResearchProject {
    /// Its arena key — §2.2's arena-of-structs idiom.
    pub id: ResearchId,
    /// The vessel carrying the lab (and the data).
    pub vessel: VesselId,
    /// The lab part's graph node — interrupt bookkeeping keys on it.
    pub lab_node: PartNodeId,
    /// The record being converted.
    pub key: DataKey,
    /// Assigned researchers, frozen at start.
    pub researchers: Vec<ResearcherAssign>,
    /// `R = Σ skill · lab_multiplier`, frozen at start (science/s).
    pub rate_science_per_s: f64,
    /// `T = throughput_data_per_s`, frozen at start (data/s).
    pub throughput_data_per_s: f64,
    /// The drain tick the project started at.
    pub start_tick: u64,
    /// Escrowed data units at `start_tick` — `D0`.
    pub data_start: f64,
    /// The predicted exhaustion tick — the pending event pair's tick.
    pub exhaust_tick: u64,
}

/// §3.2's lab tunables — construction config (loaded from
/// `balance.ron` in real worlds; the `DEFAULT_*` mirrors otherwise),
/// **serialized and hashed** like `transfer_cfg`: every field changes
/// state evolution (the yield curve, the exhaustion tick, the transmit
/// cost/payout).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScienceConfig {
    /// `k` — the soft-cap constant (§3.2 `k ≈ 1.0`).
    pub softcap_k: f64,
    /// `data_epsilon` — `D < ε` is exhaustion (§3.2's 1e-6).
    pub data_epsilon: f64,
    /// `η` — science points banked per data unit consumed.
    pub science_per_data: f64,
    /// `ElectricCharge` units spent per transmitted data unit.
    pub transmit_ec_per_data_unit: f64,
}

/// `balance.ron` mirrors for data-less worlds.
pub const DEFAULT_SOFTCAP_K: f64 = 1.0;
/// See [`DEFAULT_SOFTCAP_K`].
pub const DEFAULT_DATA_EPSILON: f64 = 1e-6;
/// See [`DEFAULT_SOFTCAP_K`].
pub const DEFAULT_SCIENCE_PER_DATA: f64 = 1.0;
/// See [`DEFAULT_SOFTCAP_K`].
pub const DEFAULT_TRANSMIT_EC_PER_UNIT: f64 = 1.0;

/// The `DEFAULT_*` config — `World::new()`'s no-data fallback.
pub fn default_config() -> ScienceConfig {
    ScienceConfig {
        softcap_k: DEFAULT_SOFTCAP_K,
        data_epsilon: DEFAULT_DATA_EPSILON,
        science_per_data: DEFAULT_SCIENCE_PER_DATA,
        transmit_ec_per_data_unit: DEFAULT_TRANSMIT_EC_PER_UNIT,
    }
}

impl ScienceConfig {
    /// The `balance.ron` → config conversion (§8.3).
    pub fn from_balance(b: &csp_data::BalanceDef) -> Self {
        Self {
            softcap_k: b.science_softcap_k,
            data_epsilon: b.research_data_epsilon,
            science_per_data: b.lab_science_per_data,
            transmit_ec_per_data_unit: b.transmit_ec_per_data_unit,
        }
    }

    /// The snapshot-restore gate — corrupt config fails typed rather
    /// than silently diverging (a non-positive `k`/`ε`/`η` would make
    /// the closed form lie; a negative EC rate would mint charge).
    pub fn is_valid(&self) -> bool {
        self.softcap_k.is_finite()
            && self.softcap_k > 0.0
            && self.data_epsilon.is_finite()
            && self.data_epsilon > 0.0
            && self.science_per_data.is_finite()
            && self.science_per_data > 0.0
            && self.transmit_ec_per_data_unit.is_finite()
            && self.transmit_ec_per_data_unit >= 0.0
    }
}

/// The world's §3.2 state — everything the science model owns that is
/// not vessel-resident: the banked science total, the per-key ledger,
/// the project arena, and its next-id counter.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ScienceState {
    /// Banked science points — stamped at event/command boundaries
    /// only (see the module doc: stamped state is the anchor; the live
    /// total derives through [`World::science_at`]).
    pub banked: f64,
    /// The diminishing/closed ledger, one entry per key ever touched.
    pub keys: BTreeMap<DataKey, ScienceKeyState>,
    /// The live research projects (one per staffed lab assignment).
    pub projects: BTreeMap<ResearchId, ResearchProject>,
    /// §7.2 next-id counter — monotonic, never reused.
    pub next_id: u64,
}

// ---------------------------------------------------------------------------
// The §3.2 closed form — pure functions, libm only (§2.7).
// ---------------------------------------------------------------------------

/// `D(t)` for a project — the escrowed data remaining `t` seconds after
/// `data_start` was stamped. Two regimes per §3.2: linear to the `T`
/// boundary, then exponential decay.
pub fn data_at(d0: f64, rate_r: f64, throughput_t: f64, eta: f64, t_s: f64) -> f64 {
    if t_s <= 0.0 {
        return d0;
    }
    let t_boundary = eta * (d0 - throughput_t).max(0.0) / rate_r;
    if d0 <= throughput_t || t_s >= t_boundary {
        if d0 <= throughput_t {
            d0 * libm::exp(-rate_r * t_s / (eta * throughput_t))
        } else {
            // Past the boundary: exponential from T.
            throughput_t * libm::exp(-rate_r * (t_s - t_boundary) / (eta * throughput_t))
        }
    } else {
        d0 - rate_r * t_s / eta
    }
}

/// Science accrued by a project between `start` and `t` (seconds):
/// `η·(D0 − D(t))` — the integral of `R·min(1, D/T)`.
pub fn accrued(p: &ResearchProject, cfg: &ScienceConfig, t_s: f64) -> f64 {
    cfg.science_per_data
        * (p.data_start
            - data_at(
                p.data_start,
                p.rate_science_per_s,
                p.throughput_data_per_s,
                cfg.science_per_data,
                t_s,
            ))
}

/// Seconds from `d0` to `D = ε` — §3.2's finite event time:
/// `η(D0−T)/R + (ηT/R)·ln(T/ε)` when `D0 > T`, else `(ηT/R)·ln(D0/ε)`.
pub fn exhaust_seconds(d0: f64, rate_r: f64, throughput_t: f64, eta: f64, epsilon: f64) -> f64 {
    if d0 <= epsilon {
        return 0.0;
    }
    let tail = eta * throughput_t / rate_r;
    if d0 > throughput_t {
        eta * (d0 - throughput_t) / rate_r + tail * libm::log(throughput_t / epsilon)
    } else {
        tail * libm::log(d0 / epsilon)
    }
}

/// The predicted exhaustion tick for a project started at `start` —
/// ceiled to ticks, min `start + 1` (events sit strictly ahead of the
/// clock; §2.6.5's `schedule_event` invariant).
pub fn exhaust_tick(p: &ResearchProject, cfg: &ScienceConfig) -> u64 {
    let t = exhaust_seconds(
        p.data_start,
        p.rate_science_per_s,
        p.throughput_data_per_s,
        cfg.science_per_data,
        cfg.data_epsilon,
    );
    let ticks = (t * csp_core::TICK_HZ as f64)
        .ceil()
        .max(1.0)
        .min(u64::MAX as f64) as u64;
    p.start_tick.saturating_add(ticks).max(p.start_tick + 1)
}

// ---------------------------------------------------------------------------
// Hash walks (§7.1) — hand-written, ordering is the schema.
// ---------------------------------------------------------------------------

/// One [`DataKey`]'s field walk.
pub fn hash_data_key(h: &mut StateHasher, k: &DataKey) {
    h.str(k.experiment.as_str());
    h.str(k.body.as_str());
    h.str(k.biome.as_str());
    h.stable_enum(&k.situation);
}

/// The per-vessel science records — appended to `hash_vessel`.
pub fn hash_vessel_science(h: &mut StateHasher, s: &VesselScience) {
    h.btree_map(
        &s.data,
        |h, node| {
            h.u64(node.0);
        },
        |h, m| {
            h.btree_map(m, hash_data_key, |h, u| {
                h.f64(*u);
            });
        },
    );
    h.u32(s.spent.len() as u32);
    for (node, exp) in &s.spent {
        h.u64(node.0);
        h.str(exp.as_str());
    }
}

/// One [`ResearchProject`]'s field walk.
pub fn hash_project(h: &mut StateHasher, p: &ResearchProject) {
    h.u64(p.vessel.0);
    h.u64(p.lab_node.0);
    hash_data_key(h, &p.key);
    h.vec(&p.researchers, |h, r| {
        h.u64(r.crew.0);
        h.u8(r.skill);
    });
    h.f64(p.rate_science_per_s);
    h.f64(p.throughput_data_per_s);
    h.u64(p.start_tick);
    h.f64(p.data_start);
    h.u64(p.exhaust_tick);
}

/// The world-level [`ScienceState`] walk — appended to `state_hash`.
pub fn hash_science(h: &mut StateHasher, s: &ScienceState) {
    h.f64(s.banked);
    h.btree_map(&s.keys, hash_data_key, |h, k| {
        h.f64(k.collected);
        h.bool(k.transmitted);
        h.bool(k.closed);
    });
    h.u64(s.next_id);
    h.arena(s.projects.iter().map(|(id, p)| (id.0, p)), hash_project);
}

// ---------------------------------------------------------------------------
// Graph-surgery bookkeeping — the pool/spent partition rules shared by
// breakup, undock, and the latch merge (the `tank_contents` precedent).
// ---------------------------------------------------------------------------

/// Split `vessel`'s science records for the fragment: entries bound to
/// `frag_nodes` leave with it. Also finalizes each project whose lab
/// node departs — `science.banked` accrues to `tick` and the project's
/// remaining data lands in the fragment's pool under the lab node.
/// Returns `(fragment_science, finalized_project_ids)` — the caller
/// scrubs the finalized projects' pending event pair from the heap.
pub(crate) fn split_vessel_science(
    vessel: &mut Vessel,
    frag_nodes: &BTreeSet<PartNodeId>,
    science: &mut ScienceState,
    cfg: &ScienceConfig,
    tick: u64,
) -> (VesselScience, Vec<ResearchId>, f64) {
    let frag = VesselScience {
        data: {
            let mut out = BTreeMap::new();
            for (node, m) in &vessel.science.data {
                if frag_nodes.contains(node) {
                    out.insert(*node, m.clone());
                }
            }
            out
        },
        spent: vessel
            .science
            .spent
            .iter()
            .filter(|(n, _)| frag_nodes.contains(n))
            .cloned()
            .collect(),
    };
    vessel.science.data.retain(|id, _| !frag_nodes.contains(id));
    vessel
        .science
        .spent
        .retain(|(n, _)| !frag_nodes.contains(n));
    // Projects riding a departing lab finalize here: the lab part is
    // leaving, so the project ends — accrued science banks, and the
    // escrowed remainder returns to the pool *on the fragment* (the
    // data physically sits in the lab that left).
    let mut done = Vec::new();
    for p in science.projects.values() {
        if p.vessel == vessel.id && frag_nodes.contains(&p.lab_node) {
            done.push(p.id);
        }
    }
    let mut frag = frag;
    let mut banked_total = 0.0;
    for pid in &done {
        let (banked, remnant) = finalize_project(science, *pid, cfg, tick, |p| p.lab_node);
        banked_total += banked;
        if let Some(remnant) = remnant {
            *frag
                .data
                .entry(remnant.0)
                .or_default()
                .entry(remnant.1)
                .or_insert(0.0) += remnant.2;
        }
    }
    (frag, done, banked_total)
}

/// Finalize project `pid` at `tick`: bank accrued science and remove
/// the record. Returns `(banked, remnant)`: `banked` is the science
/// just banked (the caller credits the §3.5 milestone payout on it —
/// banking happens even when the remnant is empty); `remnant` is the
/// `(lab_node, key, remaining_units)` of the escrowed data still
/// unconverted — `None` when nothing meaningful is left (`D < ε` —
/// treat as exhausted, drop it) or the project is absent.
/// `lab_node`/`key` come straight off the project (`which` picks the
/// return site field — the lab node conventionally).
pub(crate) fn finalize_project(
    science: &mut ScienceState,
    pid: ResearchId,
    cfg: &ScienceConfig,
    tick: u64,
    which: impl Fn(&ResearchProject) -> PartNodeId,
) -> (f64, Option<(PartNodeId, DataKey, f64)>) {
    let Some(p) = science.projects.remove(&pid) else {
        return (0.0, None);
    };
    let elapsed = (tick.saturating_sub(p.start_tick)) as f64 / csp_core::TICK_HZ as f64;
    let banked = accrued(&p, cfg, elapsed);
    science.banked += banked;
    let remaining = data_at(
        p.data_start,
        p.rate_science_per_s,
        p.throughput_data_per_s,
        cfg.science_per_data,
        elapsed,
    );
    if remaining > cfg.data_epsilon {
        (banked, Some((which(&p), p.key.clone(), remaining)))
    } else {
        (banked, None)
    }
}

/// Merge the absorbed vessel's science records into the primary's
/// through the latch remap `map` — the `merge_vessel_records` mirror:
/// returns the merged [`VesselScience`] for the caller's
/// build-locals-then-commit, and re-seats the secondary's live
/// projects onto `prim` (their lab nodes keep their records under
/// remapped ids).
pub(crate) fn merge_vessel_science(
    primary: &Vessel,
    secondary: &Vessel,
    map: &impl Fn(PartNodeId) -> PartNodeId,
    projects: &mut BTreeMap<ResearchId, ResearchProject>,
) -> VesselScience {
    let mut data = primary.science.data.clone();
    for (node, m) in &secondary.science.data {
        let dst = data.entry(map(*node)).or_default();
        for (k, u) in m {
            *dst.entry(k.clone()).or_insert(0.0) += *u;
        }
    }
    let mut spent = primary.science.spent.clone();
    for (n, e) in &secondary.science.spent {
        spent.insert((map(*n), e.clone()));
    }
    for p in projects.values_mut() {
        if p.vessel == secondary.id {
            p.vessel = primary.id;
            p.lab_node = map(p.lab_node);
        }
    }
    VesselScience { data, spent }
}

/// Drop every project belonging to `vessel` and its pending events —
/// the vessel is being destroyed; work-in-progress dies with it (no
/// accrual — see the module doc). Shadows BOTH books itself, before
/// mutating either: a post-removal `Recoverable` must restore the
/// projects AND their pending event pair together — a capture taken
/// only after the scrub would leave restored projects with no pending
/// events (a zombie the restore gate's pair-check rejects).
pub(crate) fn drop_vessel_projects(
    science: &mut ScienceState,
    heap: &mut std::collections::BinaryHeap<crate::events::ScheduledEvent>,
    next_event_seq: u64,
    vessel: VesselId,
    shadow: &mut TickShadow,
) {
    if !science.projects.values().any(|p| p.vessel == vessel) {
        return;
    }
    shadow_science(science, &mut shadow.science);
    crate::shadow_events_split(heap, next_event_seq, shadow);
    science.projects.retain(|_, p| p.vessel != vessel);
    let kept: Vec<_> = std::mem::take(heap)
        .into_iter()
        .filter(|e| {
            !matches!(
                &e.kind,
                crate::EventKind::ResearchDataExhausted { project }
                | crate::EventKind::ResearchProjectComplete { project }
                    if !science.projects.contains_key(project)
            )
        })
        .collect();
    *heap = kept.into_iter().collect();
}

/// Lazy-shadow capture for the science book.
pub(crate) fn shadow_science(world_science: &ScienceState, slot: &mut Option<ScienceState>) {
    if slot.is_none() {
        *slot = Some(world_science.clone());
    }
}

// ---------------------------------------------------------------------------
// World impl — command applies, the event fires, observation.
// ---------------------------------------------------------------------------

impl World {
    /// The science book — the per-key ledger and live projects.
    pub fn science(&self) -> &ScienceState {
        &self.science
    }

    /// The live science total at `tick` — `banked` plus every running
    /// project's accrued-but-unbanked points. `tick` clamps at
    /// `clock`: it never looks ahead of stamped state.
    pub fn science_at(&self, tick: u64) -> f64 {
        let t = tick.min(self.clock.ticks());
        let mut total = self.science.banked;
        for p in self.science.projects.values() {
            let elapsed = t.saturating_sub(p.start_tick) as f64 / csp_core::TICK_HZ as f64;
            total += accrued(p, &self.science_cfg, elapsed);
        }
        total
    }

    /// Convenience observer at the current tick.
    pub fn science_now(&self) -> f64 {
        self.science_at(self.clock.ticks())
    }

    /// Set the lab tunables — the `set_transfer_config` pattern:
    /// construction config, applied before run start.
    pub fn set_science_config(&mut self, cfg: ScienceConfig) {
        self.science_cfg = cfg;
    }

    /// Install the loaded experiment defs — construction config (the
    /// `World::orbits` precedent: data-derived, handed in, never
    /// serialized or hashed; `WorldConfig` carries it so §7.6 replays
    /// rebuild it).
    pub fn set_experiments(&mut self, experiments: BTreeMap<ExperimentId, ExperimentDef>) {
        self.experiments = experiments;
    }

    /// The installed experiment-def table (construction config).
    pub fn experiments(&self) -> &BTreeMap<ExperimentId, ExperimentDef> {
        &self.experiments
    }

    /// The body the vessel's science context is on — `None` for a
    /// craft-less vessel (SpawnVessel) or no state at all.
    fn science_body(&self, v: &Vessel) -> Option<BodyId> {
        match &v.state {
            VesselState::OnRails(o) => Some(o.parent.clone()),
            VesselState::SurfaceLatched(p) => Some(p.body.clone()),
            VesselState::InBubble(_) => Some(self.physics.as_ref()?.dominant.clone()),
        }
    }

    /// `RunExperiment` — §3.2's data collection: validate the module,
    /// context, and key; apply the soft-cap yield; pool the record.
    /// The yield goes to `science.keys[key].collected` AND to the
    /// vessel's pool under `node`.
    #[allow(clippy::too_many_lines)]
    pub(crate) fn apply_run_experiment(
        &mut self,
        vessel: VesselId,
        node: PartNodeId,
        experiment: &ExperimentId,
        biome: &BiomeId,
        situation: Situation,
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        if !self.science_cfg.is_valid() {
            return Err(SimError::Recoverable("science: invalid config"));
        }
        let Some(v) = self.vessels.get(&vessel) else {
            return Err(SimError::Recoverable("experiment: unknown vessel"));
        };
        let Some(craft) = &v.craft else {
            return Err(SimError::Recoverable("experiment: vessel has no craft"));
        };
        let Some(n) = craft.graph.nodes.get(&node) else {
            return Err(SimError::Recoverable("experiment: unknown part node"));
        };
        let idx = usize::try_from(n.effective_index)
            .map_err(|_| SimError::Fatal("experiment: effective_index overflow"))?;
        let Some(part) = craft.parts.get(idx) else {
            return Err(SimError::Recoverable("experiment: node has no part"));
        };
        let Some(spec) = part.experiments.iter().find(|e| e.kind == *experiment) else {
            return Err(SimError::Recoverable("experiment: no such module on node"));
        };
        if !spec.situations.contains(situation) {
            return Err(SimError::Recoverable(
                "experiment: cannot run in this situation",
            ));
        }
        // Residence sanity: surface-side situations need a body-adjacent
        // residence (in-bubble or surface-latched — never pure rails);
        // space situations accept any residence.
        if situation.is_surface_side() && matches!(v.state, VesselState::OnRails(_)) {
            return Err(SimError::Recoverable(
                "experiment: situation claims surface context on rails",
            ));
        }
        let (data_units, reusable) = (spec.data_units, spec.reusable);
        if !reusable && v.science.spent.contains(&(node, experiment.clone())) {
            return Err(SimError::Recoverable("experiment: module already spent"));
        }
        let Some(def) = self.experiments.get(experiment) else {
            return Err(SimError::Recoverable("experiment: no def loaded for kind"));
        };
        let Some(body) = self.science_body(v) else {
            return Err(SimError::Recoverable("experiment: no body context"));
        };
        let key = DataKey {
            experiment: experiment.clone(),
            body: body.clone(),
            biome: biome.clone(),
            situation,
        };
        if self.science.keys.get(&key).is_some_and(|k| k.closed) {
            return Err(SimError::Recoverable("experiment: key closed by recovery"));
        }
        let Some(cap) = def.cap_for(&body, situation) else {
            return Err(SimError::Recoverable(
                "experiment: no cap authored for context",
            ));
        };
        let collected = self.science.keys.get(&key).map_or(0.0, |k| k.collected);
        // §3.2's diminishing-repeat soft cap — the exact integral of the
        // plan's rate form: `C' = C + (cap − C)·(1 − exp(−k·u/cap))`.
        let room = cap - collected;
        let yield_units = room * (-libm::expm1(-self.science_cfg.softcap_k * data_units / cap));
        if !(yield_units.is_finite() && yield_units > 0.0) {
            return Err(SimError::Recoverable(
                "experiment: key saturated — no yield",
            ));
        }
        self.shadow_science(shadow);
        self.shadow_vessels(shadow);
        self.science.keys.entry(key.clone()).or_default().collected += yield_units;
        let v = self
            .vessels
            .get_mut(&vessel)
            .ok_or(SimError::Fatal("experiment: vessel vanished"))?;
        *v.science
            .data
            .entry(node)
            .or_default()
            .entry(key)
            .or_insert(0.0) += yield_units;
        if !reusable {
            v.science.spent.insert((node, experiment.clone()));
        }
        Ok(())
    }

    /// `TransmitData` — bank `units·transmit_ratio`, drain
    /// `ElectricCharge`, drop the pooled units at `key`, flag the key
    /// transmitted (§3.2's "recovered-vs-transmitted is tracked per
    /// key").
    pub(crate) fn apply_transmit_data(
        &mut self,
        vessel: VesselId,
        key: &DataKey,
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        if !self.science_cfg.is_valid() {
            return Err(SimError::Recoverable("science: invalid config"));
        }
        let Some(v) = self.vessels.get(&vessel) else {
            return Err(SimError::Recoverable("transmit: unknown vessel"));
        };
        let units = v.science.units_at(key);
        if !(units.is_finite() && units > 0.0) {
            return Err(SimError::Recoverable("transmit: no data aboard at key"));
        }
        if self.science.keys.get(key).is_some_and(|k| k.closed) {
            return Err(SimError::Recoverable("transmit: key closed by recovery"));
        }
        let Some(def) = self.experiments.get(&key.experiment) else {
            return Err(SimError::Recoverable(
                "transmit: no def loaded for experiment",
            ));
        };
        let ratio = def.transmit_ratio;
        if !(ratio.is_finite() && ratio > 0.0 && ratio <= 1.0) {
            return Err(SimError::Recoverable(
                "transmit: def transmit_ratio out of band",
            ));
        }
        let ec = ResourceId::from(ELECTRIC_CHARGE);
        let cost = units * self.science_cfg.transmit_ec_per_data_unit;
        if crate::transfers::vessel_resource(v, &ec) < cost {
            return Err(SimError::Recoverable(
                "transmit: insufficient ElectricCharge",
            ));
        }
        self.shadow_science(shadow);
        self.shadow_vessels(shadow);
        let v = self
            .vessels
            .get_mut(&vessel)
            .ok_or(SimError::Fatal("transmit: vessel vanished"))?;
        let (_, mass_changed) = crate::transfers::debit(v, &ec, cost);
        crate::transfers::reweld_if_mass_changed(v, mass_changed)?;
        for m in v.science.data.values_mut() {
            m.remove(key);
        }
        v.science.data.retain(|_, m| !m.is_empty());
        let banked = units * ratio;
        self.science.banked += banked;
        self.science
            .keys
            .entry(key.clone())
            .or_default()
            .transmitted = true;
        // M7-C: banked science pays out at
        // `science_funds_per_point` — the milestone edge.
        self.credit_science_payout(banked, shadow);
        // M7-D: a transmit banks returned data — the survey-contract
        // trigger.
        self.contract_notify(&crate::contracts::ContractTrigger::DataBanked, shadow)
    }

    /// `RecoverData` — the physical-recovery cashout: 100% of the
    /// pooled units, or the `1 − ratio` remainder for a previously
    /// transmitted key; then the key closes (§3.2). Requires the vessel
    /// be `SurfaceLatched` — the sample is on the ground somewhere
    /// (site mechanics belong to the recovery sub-phase). The close is
    /// program-wide, so it also refuses while any other vessel still
    /// pools data at the key or a live project holds escrow on it —
    /// the "closed ⇒ no aboard data" invariant `restore_science_state`
    /// enforces.
    pub(crate) fn apply_recover_data(
        &mut self,
        vessel: VesselId,
        key: &DataKey,
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        if !self.science_cfg.is_valid() {
            return Err(SimError::Recoverable("science: invalid config"));
        }
        let Some(v) = self.vessels.get(&vessel) else {
            return Err(SimError::Recoverable("recover: unknown vessel"));
        };
        if !matches!(v.state, VesselState::SurfaceLatched(_)) {
            return Err(SimError::Recoverable("recover: vessel not on the surface"));
        }
        let units = v.science.units_at(key);
        if !(units.is_finite() && units > 0.0) {
            return Err(SimError::Recoverable("recover: no data aboard at key"));
        }
        let Some(def) = self.experiments.get(&key.experiment) else {
            return Err(SimError::Recoverable(
                "recover: no def loaded for experiment",
            ));
        };
        let prior = self.science.keys.get(key).copied().unwrap_or_default();
        if prior.closed {
            return Err(SimError::Recoverable("recover: key already closed"));
        }
        // Recovery closes the key *program-wide* (§3.2): the snapshot
        // restore gate fatals on pooled data under a closed ledger
        // entry, so the close is legal only while nothing elsewhere
        // still holds the key — pooled units aboard ANY other vessel,
        // or escrow inside a live project (a later finalize would
        // return its remnant to a pool the close just sealed). Reject
        // typed rather than silently strand: transmit the other
        // vessel's record, or finish the project, first.
        if self
            .vessels
            .iter()
            .any(|(id, other)| *id != vessel && other.science.units_at(key) > 0.0)
        {
            return Err(SimError::Recoverable(
                "recover: data at key aboard another vessel",
            ));
        }
        if self.science.projects.values().any(|p| p.key == *key) {
            return Err(SimError::Recoverable(
                "recover: a live project still holds the key",
            ));
        }
        let fraction = if prior.transmitted {
            (1.0 - def.transmit_ratio).max(0.0)
        } else {
            1.0
        };
        self.shadow_science(shadow);
        self.shadow_vessels(shadow);
        let v = self
            .vessels
            .get_mut(&vessel)
            .ok_or(SimError::Fatal("recover: vessel vanished"))?;
        for m in v.science.data.values_mut() {
            m.remove(key);
        }
        v.science.data.retain(|_, m| !m.is_empty());
        let banked = units * fraction;
        self.science.banked += banked;
        let entry = self.science.keys.entry(key.clone()).or_default();
        entry.closed = true;
        // M7-C: banked science pays out at
        // `science_funds_per_point` — the milestone edge.
        self.credit_science_payout(banked, shadow);
        // M7-D: a recovery closes+returns the key — the survey
        // trigger.
        self.contract_notify(&crate::contracts::ContractTrigger::DataBanked, shadow)?;
        Ok(())
    }

    /// `StartResearch` — create the project, escrow the pooled data at
    /// `key` into it, and schedule the exhaustion pair at the predicted
    /// tick (module doc). R/T/skill freeze here — the closed form runs
    /// off the frozen record only.
    #[allow(clippy::too_many_lines)]
    pub(crate) fn apply_start_research(
        &mut self,
        vessel: VesselId,
        lab_node: PartNodeId,
        key: &DataKey,
        researchers: &[ResearcherAssign],
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        if !self.science_cfg.is_valid() {
            return Err(SimError::Recoverable("science: invalid config"));
        }
        let Some(v) = self.vessels.get(&vessel) else {
            return Err(SimError::Recoverable("research: unknown vessel"));
        };
        let Some(craft) = &v.craft else {
            return Err(SimError::Recoverable("research: vessel has no craft"));
        };
        let Some(n) = craft.graph.nodes.get(&lab_node) else {
            return Err(SimError::Recoverable("research: unknown lab node"));
        };
        let idx = usize::try_from(n.effective_index)
            .map_err(|_| SimError::Fatal("research: effective_index overflow"))?;
        let Some(LabSpec {
            throughput_data_per_s: t,
            multiplier,
            researcher_slots,
        }) = craft.parts.get(idx).and_then(|p| p.lab)
        else {
            return Err(SimError::Recoverable("research: node carries no lab"));
        };
        if researchers.is_empty() {
            return Err(SimError::Recoverable("research: no researchers assigned"));
        }
        if researchers.len() as u32 > researcher_slots {
            return Err(SimError::Recoverable(
                "research: more researchers than slots",
            ));
        }
        let mut seen = BTreeSet::new();
        let mut skill_sum = 0.0f64;
        for r in researchers {
            if r.skill > 5 {
                return Err(SimError::Recoverable("research: skill outside 0..=5"));
            }
            if !seen.insert(r.crew) {
                return Err(SimError::Recoverable("research: duplicate researcher"));
            }
            if !v.crew.iter().any(|c| c.id == r.crew) {
                return Err(SimError::Recoverable("research: researcher not aboard"));
            }
            skill_sum += f64::from(r.skill);
        }
        let rate_r = skill_sum * multiplier;
        if !(rate_r.is_finite() && rate_r > 0.0) {
            return Err(SimError::Recoverable("research: zero effective rate"));
        }
        if self
            .science
            .projects
            .values()
            .any(|p| p.vessel == vessel && p.lab_node == lab_node)
        {
            return Err(SimError::Recoverable(
                "research: lab already running a project",
            ));
        }
        if self.science.keys.get(key).is_some_and(|k| k.closed) {
            return Err(SimError::Recoverable("research: key closed by recovery"));
        }
        let d0 = v.science.units_at(key);
        if !(d0.is_finite() && d0 > self.science_cfg.data_epsilon) {
            return Err(SimError::Recoverable("research: no usable data at key"));
        }
        if !(t.is_finite() && t > 0.0) {
            return Err(SimError::Recoverable(
                "research: lab throughput not positive/finite",
            ));
        }
        let now = self.clock.ticks();
        let id = ResearchId(self.science.next_id);
        let mut project = ResearchProject {
            id,
            vessel,
            lab_node,
            key: key.clone(),
            researchers: researchers.to_vec(),
            rate_science_per_s: rate_r,
            throughput_data_per_s: t,
            start_tick: now,
            data_start: d0,
            exhaust_tick: now + 1,
        };
        project.exhaust_tick = exhaust_tick(&project, &self.science_cfg);
        self.shadow_science(shadow);
        self.shadow_vessels(shadow);
        self.science.next_id = self
            .science
            .next_id
            .checked_add(1)
            .ok_or(SimError::Fatal("research id counter overflow"))?;
        // Escrow: the pooled units at `key` move into the project.
        let v = self
            .vessels
            .get_mut(&vessel)
            .ok_or(SimError::Fatal("research: vessel vanished"))?;
        for m in v.science.data.values_mut() {
            m.remove(key);
        }
        v.science.data.retain(|_, m| !m.is_empty());
        let tick = project.exhaust_tick;
        self.science.projects.insert(id, project);
        // The same-tick pair — Exhausted first (it does the conversion);
        // seq order is insertion order, so Complete fires second.
        self.schedule_event(
            crate::EventKind::ResearchDataExhausted { project: id },
            tick,
            shadow,
        )?;
        self.schedule_event(
            crate::EventKind::ResearchProjectComplete { project: id },
            tick,
            shadow,
        )?;
        Ok(())
    }

    /// `StopResearch` — accrue science to the drain tick, return the
    /// unconverted remainder to the pool under the lab node, drop the
    /// project and its pending events.
    pub(crate) fn apply_stop_research(
        &mut self,
        project: ResearchId,
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        if !self.science.projects.contains_key(&project) {
            return Err(SimError::Recoverable("research: unknown project"));
        }
        self.shadow_science(shadow);
        self.shadow_vessels(shadow);
        self.shadow_events(shadow);
        let tick = self.clock.ticks();
        let owner = self.science.projects.get(&project).map(|p| p.vessel);
        let (banked, remnant) =
            finalize_project(&mut self.science, project, &self.science_cfg, tick, |p| {
                p.lab_node
            });
        if let Some((node, key, units)) = remnant
            && let Some(v) = owner.and_then(|vid| self.vessels.get_mut(&vid))
        {
            // The pool is node-scoped: the remainder returns under the
            // lab's own node (it was holding the escrow).
            *v.science
                .data
                .entry(node)
                .or_default()
                .entry(key)
                .or_insert(0.0) += units;
        }
        // M7-C: the banked accrual pays out at
        // `science_funds_per_point` — the milestone edge.
        self.credit_science_payout(banked, shadow);
        // Scrub the pending pair — the project is gone by intent.
        let kept: Vec<_> = std::mem::take(&mut self.event_heap)
            .into_iter()
            .filter(|e| {
                !matches!(
                    &e.kind,
                    crate::EventKind::ResearchDataExhausted { project: p }
                    | crate::EventKind::ResearchProjectComplete { project: p }
                        if *p == project
                )
            })
            .collect();
        self.event_heap = kept.into_iter().collect();
        Ok(())
    }

    /// `ResearchDataExhausted` firing: bank `η·D0` (the residual is
    /// below epsilon — convert the whole escrow) and consume the
    /// project record. Missing project → no-op (orphan event).
    pub(crate) fn fire_research_exhausted(
        &mut self,
        project: ResearchId,
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        if !self.science.projects.contains_key(&project) {
            return Ok(());
        }
        self.shadow_science(shadow);
        let Some(p) = self.science.projects.remove(&project) else {
            return Ok(());
        };
        let gained = self.science_cfg.science_per_data * p.data_start;
        if !gained.is_finite() {
            return Err(SimError::Fatal("research: non-finite accrual"));
        }
        self.science.banked += gained;
        // M7-C: banked science pays out at
        // `science_funds_per_point` — the milestone edge.
        self.credit_science_payout(gained, shadow);
        Ok(())
    }

    /// `ResearchProjectComplete` firing: the player-facing completion
    /// notification — the event landing in `fired_events` IS the
    /// completion. Defensive: if the project somehow still exists
    /// (a degraded Exhausted firing), complete it identically.
    pub(crate) fn fire_research_complete(
        &mut self,
        project: ResearchId,
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        self.fire_research_exhausted(project, shadow)
    }

    /// Lazy-shadow capture for the science book (the
    /// `shadow_transfers` precedent — one clone per tick that mutates).
    pub(crate) fn shadow_science(&mut self, shadow: &mut TickShadow) {
        shadow_science(&self.science, &mut shadow.science);
    }

    /// Jettison/split bookkeeping: finalize every project ON `vessel`
    /// whose lab node is in `nodes` — accrue science to `tick`, drop
    /// the record and its pending events, and return `(remnants,
    /// banked_total)`: each `(lab_node, key, units)` remnant for the
    /// caller to place (or discard — jettison loses them: the
    /// escrowed data leaves with the part) plus the summed accrual so
    /// the caller can credit the §3.5 milestone payout. The vessel
    /// scope matters: `PartNodeId`s are only unique within one craft,
    /// so node `N` leaving vessel A must not touch another vessel's
    /// project that happens to share the id (the
    /// [`split_vessel_science`] `p.vessel == vessel` rule).
    pub(crate) fn finalize_projects_on_nodes(
        &mut self,
        vessel: VesselId,
        nodes: &BTreeSet<PartNodeId>,
        tick: u64,
    ) -> (Vec<(PartNodeId, DataKey, f64)>, f64) {
        let done: Vec<ResearchId> = self
            .science
            .projects
            .values()
            .filter(|p| p.vessel == vessel && nodes.contains(&p.lab_node))
            .map(|p| p.id)
            .collect();
        let mut out = Vec::new();
        let mut banked_total = 0.0;
        for pid in done {
            let (banked, remnant) =
                finalize_project(&mut self.science, pid, &self.science_cfg, tick, |p| {
                    p.lab_node
                });
            banked_total += banked;
            if let Some(remnant) = remnant {
                out.push(remnant);
            }
            let kept: Vec<_> = std::mem::take(&mut self.event_heap)
                .into_iter()
                .filter(|e| {
                    !matches!(
                        &e.kind,
                        crate::EventKind::ResearchDataExhausted { project: p }
                        | crate::EventKind::ResearchProjectComplete { project: p }
                            if *p == pid
                    )
                })
                .collect();
            self.event_heap = kept.into_iter().collect();
        }
        (out, banked_total)
    }
}

#[cfg(test)]
// Tests signal failure by panicking; §2.13's no-panic rule covers the
// shipping fallible paths.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    const EPS: f64 = DEFAULT_DATA_EPSILON;

    fn cfg() -> ScienceConfig {
        default_config()
    }

    fn project(d0: f64, r: f64, t: f64) -> ResearchProject {
        ResearchProject {
            id: ResearchId(0),
            vessel: VesselId(0),
            lab_node: PartNodeId(0),
            key: DataKey {
                experiment: ExperimentId::from("e"),
                body: BodyId::from("b"),
                biome: BiomeId::from(""),
                situation: Situation::SpaceLow,
            },
            researchers: vec![],
            rate_science_per_s: r,
            throughput_data_per_s: t,
            start_tick: 0,
            data_start: d0,
            exhaust_tick: 1,
        }
    }

    /// Abundant regime (D0 > T): linear drain — `D(t) = D0 − R·t/η`
    /// until the T boundary, then exponential.
    #[test]
    fn closed_form_plentiful_linear() {
        let c = cfg();
        // D0 = 10, T = 1, R = 2, η = 1: boundary at t_T = 4.5 s.
        let p = project(10.0, 2.0, 1.0);
        let d = |t_s| data_at(10.0, 2.0, 1.0, 1.0, t_s);
        assert_eq!(d(0.0), 10.0);
        assert!((d(2.0) - 6.0).abs() < 1e-12, "{}", d(2.0));
        assert!((d(4.5) - 1.0).abs() < 1e-12, "boundary {}", d(4.5));
        // Past the boundary: T·exp(−R(t−t_T)/(ηT)).
        assert!((d(4.5 + 1.0) - libm::exp(-2.0)).abs() < 1e-12, "{}", d(5.5));
        // Accrued science at the boundary is η·(D0 − T).
        assert!((accrued(&p, &c, 4.5) - 9.0).abs() < 1e-12);
    }

    /// Scarce regime (D0 < T): pure exponential `D0·exp(−Rt/(ηT))`.
    #[test]
    fn closed_form_scarce_exponential() {
        let d = |t_s| data_at(0.5, 2.0, 1.0, 1.0, t_s);
        assert!((d(1.0) - 0.5 * libm::exp(-2.0)).abs() < 1e-12);
        assert!((d(2.0) - 0.5 * libm::exp(-4.0)).abs() < 1e-12);
        // Exhaustion: t = (ηT/R)·ln(D0/ε).
        let t_e = exhaust_seconds(0.5, 2.0, 1.0, 1.0, EPS);
        assert!((t_e - 0.5 * libm::log(0.5 / EPS)).abs() < 1e-12, "{t_e}");
    }

    /// Mixed: D0 > T spends the linear head then the exponential tail.
    #[test]
    fn closed_form_exhaustion_two_regime() {
        let t_e = exhaust_seconds(10.0, 2.0, 1.0, 1.0, EPS);
        // η(D0−T)/R + (ηT/R)·ln(T/ε) = 4.5 + 0.5·ln(1/1e-6).
        assert!(
            (t_e - (4.5 + 0.5 * libm::log(1.0 / EPS))).abs() < 1e-12,
            "{t_e}"
        );
        // D at the event time is ≤ ε.
        assert!(data_at(10.0, 2.0, 1.0, 1.0, t_e) <= EPS * (1.0 + 1e-9));
    }

    /// The §3.2 diminishing sequence: with k=1 and runs of `cap/k`
    /// units the ledger's `collected` walks 63/86/95% of cap — the
    /// plan's quoted repeat returns.
    #[test]
    fn softcap_reproduces_plan_sequence() {
        let cap = 30.0;
        let k = 1.0;
        let run = cap / k; // the canonical run size
        let mut collected = 0.0;
        let mut acc = Vec::new();
        for _ in 0..3 {
            collected += (cap - collected) * (-libm::expm1(-k * run / cap));
            acc.push(collected / cap);
        }
        assert!(
            (acc[0] - 0.632_120_558_828_557_7).abs() < 1e-12,
            "{:?}",
            acc
        );
        assert!((acc[1] - 0.864_664_716_763_387_3).abs() < 1e-12);
        assert!((acc[2] - 0.950_212_931_632_136).abs() < 1e-12);
        // And the cumulative never exceeds cap.
        for _ in 0..20 {
            collected += (cap - collected) * (-libm::expm1(-k * run / cap));
        }
        assert!(collected < cap);
    }
}
