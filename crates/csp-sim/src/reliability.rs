//! M9b §3.4 — the reliability model: hazard accrual, the `PartFailure`
//! event, dormant hazard, failure modes, qualification accrual, and the
//! test-stand process. This module is the `Reliability` slot
//! (`Subsystem` index 8) of §2.6.5's constant order.
//!
//! # The model (§3.4 verbatim)
//!
//! `λ(t) = λ_base · Π(stress_i) · qual_factor(family)` while the part
//! is *operating*; `λ_base · dormant_stress · dormant_stress_scale`
//! while it is *dormant* (§3.4's verbatim dormant rate — no qual, no
//! infant, no stresses; `dormant_stress_scale` is the save's global
//! switch, `0` disables the mechanic). `Λ` (`PartUsage::
//! hazard_integral`) integrates λ over time; failure fires when `Λ`
//! crosses the pre-drawn threshold `θ`.
//!
//! - **Operating vs dormant.** A part is *operating* while its vessel
//!   is `InBubble` and — for an engine — burning (`throttle > 0`,
//!   not misfired, not `IgnitionFailure`-flagged); a non-engine part
//!   is operating whenever the vessel is physics-resident. Everything
//!   else is dormant: on rails, surface-latched, or an in-bubble
//!   engine that is not burning. The dormant classification at a
//!   failure decides the mode set — dormant failures draw only
//!   non-fatal modes (`FailureMode::is_fatal` is the filter).
//! - **The threshold is pre-drawn and never rerolled.**
//!   `θ₀ = −ln U₀`, `U₀ = rng(seed, {RELIABILITY, instance, 0})`,
//!   evaluated once at part creation (craft instantiation) and stored
//!   on the record. After the k-th failure the next threshold is
//!   `θₖ = θₖ₋₁ + (−ln Uₖ)` — the renewal reading of the exponential
//!   clock, counter `k` of the same stream. `k` is the failure
//!   ordinal ([`PartReliability::renewal_count`]), NOT the
//!   distinct-mode count: a repeat failure of an already-recorded
//!   mode still consumes the next draw. Revert-to-launch restores
//!   the snapshot that contains the record, so the engine still
//!   fails at the same `Λ` — a retry, not a reroll (§3.4).
//!   Revert is a *state-level* restore (`World::restore` of a §7.3
//!   snapshot); no UI prompt or revert-policy surface exists at M9b —
//!   the milestone's "revert policy in the UI" is deferred to the
//!   save/slot UI milestone, documented here so the gap is explicit.
//! - **Epoch anchoring (§2.6.1).** Rails/latched parts do not integrate
//!   per boundary — Λ at a tick is `hazard_integral + λ·(t − epoch)`,
//!   with the stamp refreshed only at absolute-tick transitions and
//!   events. In-bubble parts integrate per integer tick — the bubble
//!   already steps every integer tick warp-independently, so the tick
//!   stream is identical under every partition.
//! - **The published event.** `PartFailure { vessel, part, mode }` is
//!   a §2.6.5 `Silent` event. Dormant λ is *constant per part per
//!   save* — the crossing `Λ(t) = θ` solves in closed form for
//!   piecewise-constant λ: `tick = epoch + ceil((θ − Λ_epoch)/λ ·
//!   TICK_HZ)`, published by the subsystem slot (a pure function of
//!   `(clock, heap, state)`, the `publish_economy` precedent). In-bubble
//!   parts need no event: the bubble's integer-tick loop IS the
//!   mechanism §2.6.5's rule exists to protect — no warp can skip a
//!   tick the loop evaluates.
//! - **Ignition is a separate Bernoulli draw** per ignition index:
//!   `Uₖ = rng(seed, {IGNITION, instance, k})` against
//!   `p = 1 − exp(−λ_ign · ignition_hazard_s)` where `λ_ign` is the
//!   operating λ at the start (including `ExcessRestarts` and
//!   `UllageNegativeIgnition` stresses) and `ignition_hazard_s` is the
//!   characteristic ignition window (balance). A failed draw is a
//!   **misfire**: the start produces no thrust until the throttle
//!   returns to zero (which re-arms the next index). A part carrying
//!   the `IgnitionFailure` *mode* refuses every start.
//! - **Failure modes** map to real effects where the sim has the
//!   mechanism: `DegradedThrust` scales the part's `Thrust` sites by
//!   `degraded_thrust_factor`; `IgnitionFailure` refuses ignitions;
//!   `Leak` debits the part's tank contents at `leak_units_per_s`;
//!   `Explosion` destroys the part via the §2.4 thermal-hit machinery
//!   (unreachable dormant — the non-fatal filter). `StuckGimbal`,
//!   `SensorFault`, `StuckDecoupler`, and `PanelJam` record flags —
//!   the sim has no gimbal channel, sensor-fusion model, decouple
//!   command, or deployable state at v1, so their gameplay effect is
//!   honest absence (documented, surfaced in the notification log).
//!   `usage.gimbal_cycles` likewise never accrues — the field hashes
//!   anyway so enabling accrual later is a hash-schema change, not a
//!   silent format shift.
//! - **Qualification** accrues two ways into the M7-B
//!   [`TechState::credit_qualification`] point: flight units on burn
//!   completion (burn duration × a regime-weighted rate — atmospheric
//!   operation qualifies harder than vacuum, tunables), and bench
//!   units from `RunBenchTest` — the §3.4 test stand: funds +
//!   site-inventory resources + time, delivered by a
//!   `BenchTestComplete` event into `bench_units`. `qual_factor` is
//!   evaluated on the family's LIVE units at each λ evaluation (the
//!   resolver doc's step 7 — tests improve in-flight units).
//! - **Stress rules** evaluate live against the tick's regime inputs:
//!   `OverThrottle` vs the vessel throttle, `BurnBeyondRated` vs the
//!   running burn, `Overtemperature` vs `temp/max_temp_k`,
//!   `ExcessG` vs the tick's ‖Δv/dt‖ in g, `UllageNegativeIgnition`
//!   latched at a free-fall ignition on a `ullage_required` engine,
//!   `ExcessRestarts` vs the restart count. Infant mortality multiplies
//!   the operating λ by `1 + infant_mortality·exp(−operating_s/τ)`
//!   (`infant_tau_s` tunable) — decaying with *operating* time only.
//! - **The notification log** (`§7.2`'s "notification log (incl.
//!   recorded dormant failures)") is serialized but not hashed —
//!   recorded history, the ledger precedent.

use std::collections::{BTreeMap, VecDeque};

use serde::{Deserialize, Serialize};

use csp_core::{SimError, StateHasher, StreamKey, StreamTag, TICK_HZ, uniform};
use csp_data::{FailureMode, FamilyId, ResourceId, SiteId};
use csp_phys::{ForceSiteKind, PartNodeId, PartUsage, ReliabilitySpec};

// The stress evaluation lives in csp-phys, next to `ReliabilitySpec`:
// it is pure spec math and `csp-ui`'s VAB consumes the same
// `stress_breakdown` display table without ever seeing this crate
// (§2.1). Re-exported here so the `reliability::` paths the sim and
// its public surface already use keep resolving.
pub use csp_phys::{RegimeMetrics, stress_product};

use crate::events::EventKind;
use crate::{TickShadow, Vessel, VesselId, World};

/// §2.2 part-instance identity — a monotonic `u64` drawn from
/// [`World::next_part_instance_id`] at craft instantiation, never
/// reused. It is the KDF `entity` for `{RELIABILITY}`/`{IGNITION}`
/// draws and the `part_instance` a [`EventKind::PartFailure`] names.
/// Distinct from `PartNodeId` (a per-craft graph label): instance ids
/// are globally unique, so two vessels' node-0 parts can never draw
/// the same threshold.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PartInstanceId(pub u64);

/// The KDF counter the failure-MODE draw uses for the part's k-th
/// failure — the threshold draws take counters `0, 1, 2, …` of
/// `{RELIABILITY}`, so mode draws live in the high half of the same
/// stream (append-only `StreamTag`s admit no new tag without a
/// `KDF_VERSION` bump).
const MODE_COUNTER_BIT: u64 = 1 << 63;

/// §3.4 `PartUsage` plus the live bookkeeping the record needs —
/// serialized per instance, hashed (§7.2's "PartUsage per part
/// instance"). The stamped fields (`usage`, `theta`, `epoch_tick`,
/// `lambda_per_s`) are the authoritative hashed state;
/// `hazard_integral` inside `usage` is Λ *at `epoch_tick`*, evaluated
/// lazily past it by [`PartReliability::hazard_at`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PartReliability {
    /// The part-instance id — KDF entity, never reused.
    pub instance: PartInstanceId,
    /// Λ threshold for the NEXT failure — `θ₀ = −ln U₀` at creation;
    /// `θₖ = θₖ₋₁ + (−ln Uₖ)` after each failure (module doc).
    pub theta: f64,
    /// The §3.4 per-instance usage record (all fields stamped at
    /// `epoch_tick`; `hazard_integral` is the Λ anchor).
    /// `gimbal_cycles` stays `0` at v1 — honest non-accrual, see the
    /// module doc.
    pub usage: PartUsage,
    /// Applied failure modes — the permanent record. Distinct-mode
    /// set semantics: a repeat failure of an already-applied mode is
    /// recorded once (the mode's effect is a flag, not a stack).
    /// NOT the renewal index — see [`Self::renewal_count`].
    pub failures: Vec<FailureMode>,
    /// The failure ordinal — total failures applied, incremented on
    /// EVERY crossing whether or not the mode was new. This is the
    /// `{RELIABILITY}`/`{MODE_COUNTER_BIT}` stream counter `k`: the
    /// k-th failure draws `Uₖ` for the next threshold and for its
    /// mode pick. Using `failures.len()` here would stall the
    /// counter on a repeat mode — the same `U` would re-draw, and
    /// every later renewal would add the identical `−ln U`
    /// increment instead of a fresh draw down the stream.
    pub renewal_count: u64,
    /// The tick `usage`/`lambda_per_s` are stamped at — the epoch
    /// anchor for closed-form accrual (module doc).
    pub epoch_tick: u64,
    /// The accrual rate in force since `epoch_tick` (Λ/s).
    pub lambda_per_s: f64,
    /// Whether the current accrual is dormant-class (decides the
    /// non-fatal mode filter at a crossing).
    pub dormant_class: bool,
    /// The engine is burning (throttle up, started cleanly).
    pub burning: bool,
    /// Tick the current continuous burn started — the
    /// `BurnBeyondRated` metric's clock.
    pub burn_start_tick: u64,
    /// `UllageNegativeIgnition` latched for this burn — set at a
    /// free-fall ignition on a `ullage_required` engine, cleared at
    /// shutdown.
    pub ullage_stressed: bool,
    /// This start attempt failed its Bernoulli draw — no thrust until
    /// the throttle returns to zero (which re-arms the next index).
    pub misfire: bool,
    /// Thrust multiplier applied to the part's `Thrust` sites —
    /// `1.0` nominal, `0.0` while misfired or ignition-dead,
    /// `degraded_thrust_factor` on `DegradedThrust`.
    pub thrust_factor: f64,
    /// The pending dormant `PartFailure` tick this record published —
    /// `Some` = an event naming this instance exists in the heap
    /// (the dedup channel the publish slot reads; cleared wherever
    /// the record's binding or dormant class could change).
    pub pending_failure_tick: Option<u64>,
}

impl PartReliability {
    /// Λ at `tick` — the stamped integral plus the rate in force over
    /// the elapsed interval (§2.6.1's epoch-anchor).
    pub fn hazard_at(&self, tick: u64) -> f64 {
        let dt = tick.saturating_sub(self.epoch_tick) as f64 / TICK_HZ as f64;
        self.usage.hazard_integral + self.lambda_per_s * dt
    }

    /// Dormant seconds at `tick` (lazy — same anchor pattern).
    pub fn dormant_at(&self, tick: u64) -> f64 {
        let dt = tick.saturating_sub(self.epoch_tick) as f64 / TICK_HZ as f64;
        self.usage.dormant_s + if self.dormant_class { dt } else { 0.0 }
    }

    /// Operating seconds at `tick`.
    pub fn operating_at(&self, tick: u64) -> f64 {
        let dt = tick.saturating_sub(self.epoch_tick) as f64 / TICK_HZ as f64;
        self.usage.operating_s + if self.dormant_class { 0.0 } else { dt }
    }

    /// Fold the in-force accrual into the stamped fields at `tick` —
    /// the §2.6.1 restamp that every class/rate change performs.
    /// `dormant`/`burning`/`lambda` are the NEW values after the stamp.
    pub fn restamp(&mut self, tick: u64, lambda_per_s: f64, dormant: bool, burning: bool) {
        self.usage.hazard_integral = self.hazard_at(tick);
        if self.dormant_class {
            self.usage.dormant_s = self.dormant_at(tick);
        } else {
            self.usage.operating_s = self.operating_at(tick);
        }
        self.epoch_tick = tick;
        self.lambda_per_s = lambda_per_s;
        self.dormant_class = dormant;
        self.burning = burning;
    }

    /// Whether `mode` has been applied to this part.
    pub fn has_mode(&self, mode: FailureMode) -> bool {
        self.failures.contains(&mode)
    }
}

/// §3.4's reliability tunables — construction config, serialized and
/// hashed like `transfer_cfg` (they change state evolution).
/// `balance.ron` is the authored source; [`default_config`] is the
/// no-data mirror (the `transfers::default_config` precedent).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReliabilityConfig {
    /// The global dormant-hazard switch (§3.4: "`dormant_stress = 0`
    /// in `balance.ron` disables the mechanic entirely") — a
    /// multiplier on every spec's authored `dormant_stress`.
    pub dormant_stress_scale: f64,
    /// Characteristic ignition hazard window (s): the Bernoulli
    /// ignition-failure probability is `1 − exp(−λ_ign ·
    /// ignition_hazard_s)` (module doc).
    pub ignition_hazard_s: f64,
    /// Thrust multiplier a `DegradedThrust` failure applies.
    pub degraded_thrust_factor: f64,
    /// `Leak` mode: tank contents drained per second (units/s).
    pub leak_units_per_s: f64,
    /// Infant-mortality decay constant (s): the operating-hazard
    /// multiplier is `1 + infant_mortality·exp(−operating_s/τ)`.
    pub infant_tau_s: f64,
    /// Notification-log bound — oldest entries drop past the cap.
    pub notification_cap: usize,
    /// Flight-qualification credit per burn second in vacuum —
    /// §3.4's "weighted by regime coverage" (vacuum weight 1.0).
    pub flight_units_per_s_vacuum: f64,
    /// Flight-qualification credit per burn second in atmosphere —
    /// atmospheric operation qualifies harder (regime coverage).
    pub flight_units_per_s_atmosphere: f64,
    /// §3.4 test stand: funds charged per bench unit run.
    pub bench_funds_per_unit: f64,
    /// The site-inventory resource a bench run consumes.
    pub bench_resource: ResourceId,
    /// Units of `bench_resource` per bench unit.
    pub bench_resource_per_unit: f64,
    /// Sim-seconds one bench unit takes — the `ConstructionComplete`
    /// precedent's completion delay.
    pub bench_seconds_per_unit: f64,
}

/// The no-data mirror of `balance.ron`'s reliability block — worlds
/// built without a data set (unit tests, the wasm slice) run these
/// (the `transfers::default_config` precedent).
pub fn default_config() -> ReliabilityConfig {
    ReliabilityConfig {
        dormant_stress_scale: 1.0,
        ignition_hazard_s: 1.0,
        degraded_thrust_factor: 0.5,
        leak_units_per_s: 0.001,
        infant_tau_s: 3600.0,
        notification_cap: 256,
        flight_units_per_s_vacuum: 0.01,
        flight_units_per_s_atmosphere: 0.02,
        bench_funds_per_unit: 50.0,
        bench_resource: ResourceId::from("metal"),
        bench_resource_per_unit: 1.0,
        bench_seconds_per_unit: 60.0,
    }
}

impl ReliabilityConfig {
    /// `balance.ron`'s reliability block → construction config —
    /// the `TransferConfig::from_balance` precedent (the authored
    /// file is the source of truth; the mirror constants live in
    /// [`default_config`]).
    pub fn from_balance(b: &csp_data::BalanceDef) -> Self {
        Self {
            dormant_stress_scale: b.reliability_dormant_stress_scale,
            ignition_hazard_s: b.reliability_ignition_hazard_s,
            degraded_thrust_factor: b.reliability_degraded_thrust_factor,
            leak_units_per_s: b.reliability_leak_units_per_s,
            infant_tau_s: b.reliability_infant_tau_s,
            notification_cap: b.reliability_notification_cap as usize,
            flight_units_per_s_vacuum: b.reliability_flight_units_per_s_vacuum,
            flight_units_per_s_atmosphere: b.reliability_flight_units_per_s_atmosphere,
            bench_funds_per_unit: b.reliability_bench_funds_per_unit,
            bench_resource: b.reliability_bench_resource.clone(),
            bench_resource_per_unit: b.reliability_bench_resource_per_unit,
            bench_seconds_per_unit: b.reliability_bench_seconds_per_unit,
        }
    }

    /// Finite, in-range tunables — the restore/spawn config gate.
    pub fn is_valid(&self) -> bool {
        self.dormant_stress_scale.is_finite()
            && self.dormant_stress_scale >= 0.0
            && self.ignition_hazard_s.is_finite()
            && self.ignition_hazard_s >= 0.0
            && self.degraded_thrust_factor.is_finite()
            && self.degraded_thrust_factor >= 0.0
            && self.leak_units_per_s.is_finite()
            && self.leak_units_per_s >= 0.0
            && self.infant_tau_s.is_finite()
            && self.infant_tau_s > 0.0
            && self.flight_units_per_s_vacuum.is_finite()
            && self.flight_units_per_s_vacuum >= 0.0
            && self.flight_units_per_s_atmosphere.is_finite()
            && self.flight_units_per_s_atmosphere >= 0.0
            && self.bench_funds_per_unit.is_finite()
            && self.bench_funds_per_unit >= 0.0
            && self.bench_resource_per_unit.is_finite()
            && self.bench_resource_per_unit >= 0.0
            && self.bench_seconds_per_unit.is_finite()
            && self.bench_seconds_per_unit >= 0.0
    }
}

/// §2.2 bench-run identity — the test-stand arena key (the
/// `TransferId`/`BuildId` convention).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct BenchId(pub u64);

/// A live test-stand run — serialized and hashed sim state; pairs
/// with exactly one `BenchTestComplete` event at `complete_tick` (the
/// `ActiveTransfer`/`ActiveBuild` precedent).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ActiveBench {
    /// Its arena key, inline per §2.2.
    pub id: BenchId,
    /// The site running the stand.
    pub site: SiteId,
    /// The family under test.
    pub family: FamilyId,
    /// Bench units the run credits at completion.
    pub units: f64,
    /// The `BenchTestComplete` deadline.
    pub complete_tick: u64,
}

/// One notification-log entry — §7.2's "notification log (incl.
/// recorded dormant failures)". Serialized, never hashed (recorded
/// history, the ledger precedent), bounded by
/// [`ReliabilityConfig::notification_cap`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Notification {
    /// The tick the entry was recorded at.
    pub tick: u64,
    /// The vessel the entry concerns.
    pub vessel: VesselId,
    /// The part's graph node — `None` for vessel-level notices.
    pub node: Option<PartNodeId>,
    /// What happened.
    pub kind: NotificationKind,
}

/// What a [`Notification`] reports. Variants are append-only — the
/// ring is serialized (bincode positional discriminants), so new
/// kinds land at the end, never reordered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NotificationKind {
    /// A `Λ` crossing fired — `dormant` marks an unobserved (dormant-
    /// class) failure, the record §3.4 surfaces on next observation.
    PartFailed {
        /// The applied mode.
        mode: FailureMode,
        /// Whether the part was dormant-class at the crossing.
        dormant: bool,
    },
    /// An engine's start attempt failed its Bernoulli draw — the
    /// misfire clears when the throttle returns to zero.
    IgnitionMisfire,
    /// The vessel left the bubble for rails — stamped by
    /// `apply_to_rails_at`, so it covers the command path AND the
    /// automatic coasting-above-atmosphere conversion (the
    /// transition must not be silent).
    OnRails,
    /// A contract offer was accepted — `contract` is its arena id.
    /// A program-level entry (`vessel` is [`PROGRAM_VESSEL`]).
    ContractAccepted {
        /// The accepted contract's arena id.
        contract: u64,
    },
    /// A contract's completion predicate landed — the completion
    /// reward credited in the same boundary.
    ContractCompleted {
        /// The completed contract's arena id.
        contract: u64,
    },
    /// A contract expired at its `ContractDeadline` — the authored
    /// penalty charged through the funds clamp.
    ContractFailed {
        /// The expired contract's arena id.
        contract: u64,
    },
    /// Science was banked into the program ledger — a transmit, a
    /// data/vessel recovery, or a lab project's exhaustion.
    /// `micros` is science-points × 1e6 (the funds-micros
    /// convention; keeps the enum `Eq`).
    ScienceBanked {
        /// Points banked, in 1e-6 units.
        micros: u64,
    },
    /// §3.5's `StaffAttrition` consequence (C08-F3): an unpaid salary
    /// streak past the authored grace retired a roster member —
    /// `crew` is the roster arena id (the contract-id convention;
    /// `PROGRAM_VESSEL`-level entry).
    StaffAttrition {
        /// The `CrewId` that went `Retired`.
        crew: u64,
    },
    /// §3.5's `FacilityDormant` consequence (C08-F3): a funds charge
    /// clamped at zero suspended the program's facility surfaces —
    /// logged once per insolvency alongside the `Insolvent`/
    /// `FacilityDormant` event pair (`PROGRAM_VESSEL`-level entry).
    FacilityDormant,
    /// C21-F2's "berth blocked" reason (§3.5's build orders): the
    /// `ConstructionComplete` deadline arrived with the delivery site
    /// occupied, so the order re-queued as a `Silent`
    /// `ConstructionRetry` — one entry per blockage episode, not per
    /// retry (`PROGRAM_VESSEL`-level; `build` is the `BuildId`).
    BuildBerthBlocked {
        /// The deferred build order's arena id.
        build: u64,
    },
    /// C21-F2's delivery landing after a berth-blocked episode — the
    /// `ConstructionComplete` toast the retry path deliberately does
    /// not re-fire, logged once the craft materializes
    /// (`PROGRAM_VESSEL`-level; `build` is the `BuildId`).
    BuildDelivered {
        /// The delivered build order's arena id.
        build: u64,
    },
    /// C09-F6 §3.5.1's colony impact damage — a module marked inert
    /// by `DamageColonyModule`/`apply_surface_impacts`
    /// (`PROGRAM_VESSEL`-level; `colony`/`module` are the colony
    /// arena id and the `ModuleIndex` that broke).
    ColonyDamaged {
        /// The damaged colony's arena id.
        colony: u64,
        /// The module index marked inert.
        module: u64,
    },
}

/// `Notification::vessel` sentinel for program-level entries —
/// contract lifecycle and banked-science records name no vessel, and
/// the arena never mints `u64::MAX`, so the sentinel cannot collide
/// with a real `VesselId`.
pub const PROGRAM_VESSEL: VesselId = VesselId(u64::MAX);

/// A fresh record for one instantiated part — the §3.4 "drawn once at
/// part creation" point: `θ₀ = −ln U₀` from `{RELIABILITY, instance,
/// 0}`, stored on the record (reload cannot reroll).
pub fn fresh_record(save_seed: u64, instance: PartInstanceId, epoch_tick: u64) -> PartReliability {
    let u = uniform(
        save_seed,
        StreamKey {
            tag: StreamTag::Reliability,
            entity: instance.0,
            counter: 0,
        },
    );
    // U ∈ [0, 1): −ln U ∈ (0, +∞]. An exact U = 0 draw (p = 2⁻⁵³)
    // would store θ = +∞, which `hash_record`'s non-finite gate reads
    // as a permanent `Fatal` — the save could never hash again. Clamp
    // the draw to `MIN_POSITIVE` instead: identity for every u > 0,
    // and u = 0 yields the finite ~745 "never fails inside any
    // horizon" reading the physics paths already tolerate.
    let theta = -libm::log(u.max(f64::MIN_POSITIVE));
    PartReliability {
        instance,
        theta,
        usage: PartUsage::FRESH,
        failures: Vec::new(),
        renewal_count: 0,
        epoch_tick,
        lambda_per_s: 0.0,
        dormant_class: false,
        burning: false,
        burn_start_tick: epoch_tick,
        ullage_stressed: false,
        misfire: false,
        thrust_factor: 1.0,
        pending_failure_tick: None,
    }
}

/// Allocate per-node records for a fresh craft — one instance id per
/// graph node in ascending order (the deterministic allocation order),
/// θ pre-drawn per record. `specs` parallel to `craft.parts`; an empty
/// spec table means the craft carries no reliability model at all
/// (test/scenario crafts — the `part_costs`-is-zero precedent).
pub fn allocate_records(
    save_seed: u64,
    craft: &crate::CraftDef,
    epoch_tick: u64,
    next_id: &mut u64,
) -> Result<BTreeMap<PartNodeId, PartReliability>, SimError> {
    let mut out = BTreeMap::new();
    if craft.reliability.is_empty() {
        return Ok(out);
    }
    for id in craft.graph.nodes.keys() {
        let instance = PartInstanceId(*next_id);
        *next_id = next_id
            .checked_add(1)
            .ok_or(SimError::Fatal("part instance counter overflow"))?;
        out.insert(*id, fresh_record(save_seed, instance, epoch_tick));
    }
    Ok(out)
}

/// The operating λ at `metrics` — `λ_base · Π(stress) · qual ·
/// infant`. `op_s` is the part's current operating time (the infant
/// decay's argument).
pub fn operating_lambda(
    spec: &ReliabilitySpec,
    qual_factor: f64,
    m: &RegimeMetrics,
    op_s: f64,
    cfg: &ReliabilityConfig,
) -> f64 {
    let infant = 1.0 + spec.infant_mortality * libm::exp(-op_s / cfg.infant_tau_s);
    spec.base_hazard_per_s() * stress_product(spec, m) * qual_factor * infant
}

/// The mode draw — weighted pick over `modes` (already filtered:
/// dormant callers pass the non-fatal subset) at `failure_index`, the
/// ordinal of the failure being drawn — the record's
/// [`PartReliability::renewal_count`] before the firing increments it.
/// `U` rides `{RELIABILITY, instance, k | MODE_BIT}`, disjoint from
/// the threshold renewal stream.
pub fn draw_mode(
    save_seed: u64,
    instance: PartInstanceId,
    failure_index: u64,
    modes: &[(FailureMode, f64)],
) -> Option<FailureMode> {
    if modes.is_empty() {
        return None;
    }
    let u = uniform(
        save_seed,
        StreamKey {
            tag: StreamTag::Reliability,
            entity: instance.0,
            counter: MODE_COUNTER_BIT | failure_index,
        },
    );
    let total: f64 = modes.iter().map(|(_, w)| w).sum();
    if total <= 0.0 {
        return Some(modes[0].0);
    }
    let mut acc = 0.0;
    for (mode, w) in modes {
        acc += w / total;
        if u < acc {
            return Some(*mode);
        }
    }
    modes.last().map(|(m, _)| *m)
}

/// The modes a dormant failure may draw — §3.4's non-fatal filter.
pub fn dormant_modes(spec: &ReliabilitySpec) -> Vec<(FailureMode, f64)> {
    spec.failure_modes
        .iter()
        .filter(|(m, _)| !m.is_fatal())
        .copied()
        .collect()
}

/// The closed-form dormant crossing tick — `Λ(t) = θ` for the
/// constant dormant λ: `epoch + ceil((θ − Λ_epoch)/λ · TICK_HZ)`.
/// `None` when dormant accrual is off (the balance switch) or the
/// crossing has no finite time (θ = +∞ — the `U = 0` corner).
pub fn dormant_failure_tick(rec: &PartReliability, lambda_dormant: f64) -> Option<u64> {
    if lambda_dormant <= 0.0 || !rec.theta.is_finite() {
        return None;
    }
    let remain = rec.theta - rec.usage.hazard_integral;
    if remain <= 0.0 {
        // Already past the threshold — fire next tick (strictly
        // ahead of the clock, `schedule_event`'s invariant).
        return Some(rec.epoch_tick.saturating_add(1));
    }
    let ticks = libm::ceil(remain / lambda_dormant * TICK_HZ as f64) as u64;
    Some(rec.epoch_tick.saturating_add(ticks.max(1)))
}

/// The ignition Bernoulli draw — `Uₖ = rng(seed, {IGNITION,
/// instance, k})` vs `p = 1 − exp(−λ_ign · ignition_hazard_s)`.
/// `true` = the start succeeds.
pub fn ignition_succeeds(
    save_seed: u64,
    instance: PartInstanceId,
    ignition_index: u32,
    lambda_ign: f64,
    cfg: &ReliabilityConfig,
) -> bool {
    let u = uniform(
        save_seed,
        StreamKey {
            tag: StreamTag::Ignition,
            entity: instance.0,
            counter: u64::from(ignition_index),
        },
    );
    let p_fail = 1.0 - libm::exp(-lambda_ign.max(0.0) * cfg.ignition_hazard_s);
    u >= p_fail
}

// ---------------------------------------------------------------------------
// Hash walk (§7.1) — hand-written, ordering is the schema.
// ---------------------------------------------------------------------------

/// The per-record walk — appended inside the vessel's field walk.
pub fn hash_record(h: &mut StateHasher, r: &PartReliability) {
    h.u64(r.instance.0);
    h.f64(r.theta);
    h.f64(r.usage.operating_s);
    h.f64(r.usage.dormant_s);
    h.u64(u64::from(r.usage.ignitions));
    h.u64(u64::from(r.usage.gimbal_cycles));
    h.f64(r.usage.max_temp_k);
    h.f64(r.usage.max_axial_n);
    h.f64(r.usage.max_g);
    h.f64(r.usage.hazard_integral);
    h.vec(&r.failures, |h, m| {
        h.stable_enum(m);
    });
    h.u64(r.epoch_tick);
    h.f64(r.lambda_per_s);
    h.bool(r.dormant_class);
    h.bool(r.burning);
    h.u64(r.burn_start_tick);
    h.bool(r.ullage_stressed);
    h.bool(r.misfire);
    h.f64(r.thrust_factor);
    h.opt(&r.pending_failure_tick, |h, t| {
        h.u64(*t);
    });
    // Appended last per the record walk's append-only rule.
    h.u64(r.renewal_count);
}

/// The per-vessel table walk — `BTreeMap` order is canonical.
pub fn hash_vessel_reliability(h: &mut StateHasher, table: &BTreeMap<PartNodeId, PartReliability>) {
    h.btree_map(
        table,
        |h, id| {
            h.u64(id.0);
        },
        hash_record,
    );
}

/// The config walk — every number that changes state evolution is
/// hashed (the `economy_cfg` precedent); the `ResourceId` is a
/// string.
pub fn hash_config(h: &mut StateHasher, c: &ReliabilityConfig) {
    h.f64(c.dormant_stress_scale);
    h.f64(c.ignition_hazard_s);
    h.f64(c.degraded_thrust_factor);
    h.f64(c.leak_units_per_s);
    h.f64(c.infant_tau_s);
    h.u64(c.notification_cap as u64);
    h.f64(c.flight_units_per_s_vacuum);
    h.f64(c.flight_units_per_s_atmosphere);
    h.f64(c.bench_funds_per_unit);
    h.str(c.bench_resource.as_str());
    h.f64(c.bench_resource_per_unit);
    h.f64(c.bench_seconds_per_unit);
}

/// The bench-arena walk — the `hash_builds` precedent.
pub fn hash_benches(h: &mut StateHasher, runs: &BTreeMap<BenchId, ActiveBench>) {
    h.btree_map(
        runs,
        |h, id| {
            h.u64(id.0);
        },
        |h, b| {
            h.u64(b.id.0);
            h.str(b.site.as_str());
            h.str(b.family.as_str());
            h.f64(b.units);
            h.u64(b.complete_tick);
        },
    );
}

/// The `ReliabilitySpec` table inside `hash_craft` — the resolved
/// reliability half is craft identity like `part_costs`.
pub fn hash_spec(h: &mut StateHasher, s: &ReliabilitySpec) {
    h.str(s.family.as_str());
    h.f64(s.base_mtbf_s);
    h.f64(s.infant_mortality);
    h.f64(s.dormant_stress);
    h.vec(&s.regime_stress, |h, r| {
        h.stable_enum(&r.kind);
        h.f64(r.threshold);
        h.f64(r.multiplier);
    });
    h.vec(&s.failure_modes, |h, (m, w)| {
        h.stable_enum(m);
        h.f64(*w);
    });
    h.vec(&s.qualification_curve.knots, |h, k| {
        h.f64(k.qualification_units);
        h.f64(k.hazard_multiplier);
    });
    h.opt(&s.engine, |h, e| {
        h.f64(e.rated_burn_s);
        h.opt(&e.ignitions, |h, n| {
            h.u32(*n);
        });
        h.bool(e.ullage_required);
    });
}

// ---------------------------------------------------------------------------
// World impl — config, allocation hooks, the tick pass, the publish
// slot, the event firings, and the qualification writers.
// ---------------------------------------------------------------------------

/// Whether `craft.reliability` is positional over `craft.parts` — the
/// spawn/manufacture gates' "empty or covering" invariant. The M6-C
/// latch merge can produce a non-covering table (a specless half
/// concatenated with a spec'd one — the absorbed specs would bind at
/// the wrong `effective_index`), so every positional spec read must
/// gate on this: a non-parallel table resolves NO spec at all — the
/// part's record goes inert, never wrong.
pub(crate) fn specs_parallel(craft: &crate::CraftDef) -> bool {
    craft.reliability.len() == craft.parts.len()
}

/// The M7-B twin of [`specs_parallel`]: the `part_ids` identity
/// table covers `parts` exactly — the merge/split sites consult it
/// before carrying a positional table across a reindexed graph.
pub(crate) fn part_ids_parallel(craft: &crate::CraftDef) -> bool {
    craft.part_ids.len() == craft.parts.len()
}

impl World {
    /// The reliability tunables — construction config (`balance.ron`
    /// in real worlds; [`default_config`] otherwise).
    pub fn reliability_cfg(&self) -> &ReliabilityConfig {
        &self.reliability_cfg
    }

    /// `balance.ron` wiring — the `set_life_support_cfg` precedent:
    /// call before the first `step_frame`. This is a *construction-config*
    /// write, not a `SimCommand`: it never enters the §7.6 command log,
    /// so a post-construction call would diverge from any replay of that
    /// log. The config is serialized in the save and hashed by §7.1 —
    /// replay determinism is carried by the snapshot, not the log.
    pub fn set_reliability_config(&mut self, cfg: ReliabilityConfig) {
        self.reliability_cfg = cfg;
    }

    /// The notification log — §7.2's serialized history, bounded.
    pub fn notifications(&self) -> &VecDeque<Notification> {
        &self.notifications
    }

    /// Push a notification, bounded — the ledger precedent (history,
    /// never hashed).
    pub(crate) fn notify(&mut self, n: Notification, shadow: &mut TickShadow) {
        if shadow.notifications.is_none() {
            shadow.notifications = Some(self.notifications.clone());
        }
        self.notifications.push_back(n);
        while self.notifications.len() > self.reliability_cfg.notification_cap {
            self.notifications.pop_front();
        }
    }

    /// The spec for `node` — `craft.reliability[eff_idx]`, or `None`
    /// for crafts with no reliability model / unknown nodes. Gated
    /// on [`specs_parallel`]: a non-covering table resolves nothing.
    pub(crate) fn spec_for(vessel: &Vessel, node: PartNodeId) -> Option<&ReliabilitySpec> {
        let craft = vessel.craft.as_ref()?;
        if !specs_parallel(craft) {
            return None;
        }
        let idx = craft.graph.nodes.get(&node)?.effective_index as usize;
        craft.reliability.get(idx)
    }

    /// Residence hook — restamp every record at `tick` for the new
    /// class (§2.6.1's epoch re-anchor: fold the in-force accrual,
    /// install the new rate):
    ///
    /// - `to_dormant = true` (rails entry, surface latch): every part
    ///   goes dormant — `λ_base·dormant_stress·scale`, constant per
    ///   part per save, so a pending publish stays exact across a
    ///   dormant→dormant re-stamp (latched→rails keeps its events).
    ///   `pending_failure_tick` is NOT cleared here: the event names
    ///   this same `VesselId` and stays valid.
    /// - `to_dormant = false` (bubble entry, unlatch): pending
    ///   publishes die — the events are scrubbed as vessel-named on
    ///   the transition (`event_names_vessel`), so the flags clear;
    ///   the per-tick pass owns accrual now. Engines rest dormant
    ///   until they burn; other parts go operating under this tick's
    ///   ambient metrics.
    pub(crate) fn reliability_residence_change(
        &mut self,
        vessel: VesselId,
        tick: u64,
        to_dormant: bool,
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        let cfg = self.reliability_cfg.clone();
        let quals = self.tech.qualifications.clone();
        // Burn completions on a non-tick path — the residence restamp
        // retires `burning` without going through `bubble_part_tick`'s
        // shutdown branch, so the flight-qualification credit must be
        // emitted here or the whole burn's units are dropped (the
        // standard executor completion runs `apply_to_rails_at` → this
        // hook). `(family, burn_s, in_atmosphere)` — a to-dormant
        // transition is always vacuum-rated (the to-rails refusal
        // already fired for anything still inside the atmosphere).
        let mut burns: Vec<(FamilyId, f64, bool)> = Vec::new();
        let Some(v) = self.vessels.get_mut(&vessel) else {
            return Ok(());
        };
        let Some(craft) = v.craft.clone() else {
            return Ok(());
        };
        // A non-covering spec table (the latch merge's mismatched-
        // halves case) resolves no spec — records keep their
        // in-force rate rather than restamp against a wrong part.
        let specs_ok = specs_parallel(&craft);
        for (node, rec) in v.reliability.iter_mut() {
            let Some(spec) = craft
                .graph
                .nodes
                .get(node)
                .filter(|_| specs_ok)
                .and_then(|n| craft.reliability.get(n.effective_index as usize))
            else {
                continue;
            };
            if to_dormant {
                if rec.burning {
                    let burn_s = tick.saturating_sub(rec.burn_start_tick) as f64 / TICK_HZ as f64;
                    burns.push((spec.family.clone(), burn_s, false));
                }
                rec.restamp(
                    tick,
                    spec.dormant_hazard_per_s(cfg.dormant_stress_scale),
                    true,
                    false,
                );
                rec.misfire = false;
                rec.ullage_stressed = false;
                rec.thrust_factor = thrust_factor_for(rec, &cfg);
            } else {
                rec.pending_failure_tick = None;
                let engine = spec.engine.is_some();
                // Engines entering the bubble are not burning —
                // dormant-class until the tick pass lights them.
                // Non-engine parts go operating at ambient metrics
                // (the tick pass re-evaluates anyway).
                let (lambda, dormant) = if engine {
                    (spec.dormant_hazard_per_s(cfg.dormant_stress_scale), true)
                } else {
                    let temp_k = v.thermal.part_temps_k.get(node).copied().unwrap_or(0.0);
                    let max_k = craft
                        .parts
                        .get(
                            craft
                                .graph
                                .nodes
                                .get(node)
                                .map_or(0, |n| n.effective_index as usize),
                        )
                        .map_or(1.0, |p| p.thermal.max_temp_k.max(1.0));
                    let units = quals.get(&spec.family).map_or(0.0, |q| q.units());
                    let m = RegimeMetrics {
                        throttle: 0.0,
                        burn_s: 0.0,
                        temp_fraction: temp_k / max_k,
                        g_load: 0.0,
                        ullage_negative: false,
                        restarts: rec.usage.ignitions.saturating_sub(1) as f64,
                    };
                    (
                        operating_lambda(
                            spec,
                            csp_phys::qual_factor(&spec.qualification_curve, units),
                            &m,
                            rec.operating_at(tick),
                            &cfg,
                        ),
                        false,
                    )
                };
                rec.restamp(tick, lambda, dormant, false);
                rec.burning = false;
                rec.misfire = false;
                rec.ullage_stressed = false;
                rec.thrust_factor = thrust_factor_for(rec, &cfg);
            }
        }
        if !burns.is_empty() {
            self.shadow_tech(shadow);
            for (family, burn_s, in_atmo) in burns {
                let rate = if in_atmo {
                    self.reliability_cfg.flight_units_per_s_atmosphere
                } else {
                    self.reliability_cfg.flight_units_per_s_vacuum
                };
                self.tech
                    .credit_qualification(family, burn_s * rate, 0.0, 0)?;
            }
        }
        Ok(())
    }

    /// The `Reliability` slot (§2.6.5 index 8) — publish dormant
    /// `PartFailure`s. For every non-`InBubble` vessel's parts with a
    /// live dormant rate and no pending publish, solve the crossing
    /// `Λ(t) = θ` in closed form (constant λ) and schedule the event.
    /// A pure function of `(clock, heap, state)` — the
    /// `publish_economy` precedent: identical in every partition.
    pub(crate) fn publish_reliability(&mut self, shadow: &mut TickShadow) -> Result<(), SimError> {
        let cfg = self.reliability_cfg.clone();
        let clock = self.clock.ticks();
        // (vessel, node, instance, tick, mode) work list — collected
        // before the mutable pass so heap insertion order stays the
        // ascending (vessel, node) order `event_seq` depends on.
        let mut work: Vec<(VesselId, PartNodeId, PartInstanceId, u64, FailureMode)> = Vec::new();
        for (vid, v) in &self.vessels {
            if matches!(v.state, crate::VesselState::InBubble(_)) {
                continue;
            }
            for (node, rec) in &v.reliability {
                if rec.pending_failure_tick.is_some() {
                    continue;
                }
                let Some(spec) = Self::spec_for(v, *node) else {
                    continue;
                };
                let lambda = spec.dormant_hazard_per_s(cfg.dormant_stress_scale);
                let Some(tick) = dormant_failure_tick(rec, lambda) else {
                    continue;
                };
                let modes = dormant_modes(spec);
                // The mode index is the next failure's ordinal —
                // `renewal_count` before the firing increments it.
                let Some(mode) = draw_mode(self.save_seed, rec.instance, rec.renewal_count, &modes)
                else {
                    continue;
                };
                work.push((*vid, *node, rec.instance, tick.max(clock + 1), mode));
            }
        }
        if work.is_empty() {
            return Ok(());
        }
        self.shadow_vessels(shadow);
        for (vid, node, instance, tick, mode) in work {
            if let Some(rec) = self
                .vessels
                .get_mut(&vid)
                .and_then(|v| v.reliability.get_mut(&node))
            {
                rec.pending_failure_tick = Some(tick);
            }
            self.schedule_event(
                EventKind::PartFailure {
                    vessel: vid,
                    part: instance,
                    mode,
                },
                tick,
                shadow,
            )?;
        }
        Ok(())
    }

    /// `PartFailure` firing — the dormant clock crossed `θ`. The
    /// event is only meaningful for a dormant (non-`InBubble`)
    /// vessel: in-bubble crossings are the tick pass's (a stale
    /// event that survived an unscrubbed transition is a no-op —
    /// the `TransferComplete` precedent). The crossing is verified
    /// against the record's own accrual: a stale early fire (the
    /// record's rate moved since its publish — e.g. a dormant
    /// config swap) restamps and returns, leaving the pending flag
    /// clear for the publish slot to re-derive the true crossing.
    /// A fatal mode on a dormant record cannot legitimately exist —
    /// the publish draws the non-fatal subset — so a hand-crafted
    /// one is a stale no-op, never an explosion.
    pub(crate) fn fire_part_failure(
        &mut self,
        vessel: VesselId,
        part: PartInstanceId,
        mode: FailureMode,
        tick: u64,
        shadow: &mut TickShadow,
        degraded: &mut Vec<SimError>,
    ) -> Result<(), SimError> {
        let Some(v) = self.vessels.get(&vessel) else {
            return Ok(());
        };
        let Some(node) = v
            .reliability
            .iter()
            .find(|(_, r)| r.instance == part)
            .map(|(n, _)| *n)
        else {
            return Ok(());
        };
        if matches!(v.state, crate::VesselState::InBubble(_)) {
            // Stale event on a bubble vessel — the crossing belongs
            // to the tick pass, so no failure applies. But a record
            // whose `pending_failure_tick` still names this event
            // must not keep it: consumed here, the flag would orphan
            // — `publish_reliability` skips `is_some` records (and
            // `InBubble` vessels entirely), and the restore pairing
            // gate Fatals on a pending tick with no live event, so
            // the next save would brick its own reload (the
            // fatal-mode branch below's wedge, reached through the
            // other arm). Clear it like the stale-fire path — the
            // to-bubble transition already clears legit flags, so a
            // set one here is always leftover state.
            self.shadow_vessels(shadow);
            let v = self
                .vessels
                .get_mut(&vessel)
                .ok_or(SimError::Fatal("part failure: vessel vanished"))?;
            v.reliability
                .get_mut(&node)
                .ok_or(SimError::Fatal("part failure: record vanished"))?
                .pending_failure_tick = None;
            return Ok(());
        }
        if mode.is_fatal() {
            // A fatal mode on a dormant record is a crafted/stale
            // event — the publish only ever draws the non-fatal
            // subset. But the record's `pending_failure_tick` still
            // names this event: leaving it set would make
            // `publish_reliability` skip the record forever AND wedge
            // the next snapshot restore (the restore gate requires
            // every pending tick to pair with a live event). Clear it
            // like the stale-fire branch below so the publish slot
            // re-derives the true crossing.
            self.shadow_vessels(shadow);
            let v = self
                .vessels
                .get_mut(&vessel)
                .ok_or(SimError::Fatal("part failure: vessel vanished"))?;
            v.reliability
                .get_mut(&node)
                .ok_or(SimError::Fatal("part failure: record vanished"))?
                .pending_failure_tick = None;
            return Ok(());
        }
        let fire_tick = tick.min(self.clock.ticks());
        if self.vessels[&vessel].reliability[&node].hazard_at(fire_tick)
            < self.vessels[&vessel].reliability[&node].theta
        {
            // Stale early fire — restamp the dormant accrual (it is
            // real time under the new rate), clear the flag, and let
            // the publish slot re-derive the true crossing.
            let dormant_scale = self.reliability_cfg.dormant_stress_scale;
            self.shadow_vessels(shadow);
            let v = self
                .vessels
                .get_mut(&vessel)
                .ok_or(SimError::Fatal("part failure: vessel vanished"))?;
            let lambda = Self::spec_for(v, node)
                .map(|s| s.dormant_hazard_per_s(dormant_scale))
                .unwrap_or(0.0);
            let rec = v
                .reliability
                .get_mut(&node)
                .ok_or(SimError::Fatal("part failure: record vanished"))?;
            rec.pending_failure_tick = None;
            rec.restamp(fire_tick, lambda, true, false);
            return Ok(());
        }
        self.apply_failure_at(vessel, node, mode, fire_tick, true, shadow, degraded)
    }

    /// The shared failure application — dormant event firings and the
    /// in-bubble inline crossing both land here: restamp `Λ` at the
    /// crossing tick, record the mode, apply its effect, advance the
    /// renewal threshold `θₖ = θₖ₋₁ + (−ln Uₖ)`, credit the family's
    /// `observed_failures`, and log the notification.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn apply_failure_at(
        &mut self,
        vessel: VesselId,
        node: PartNodeId,
        mode: FailureMode,
        tick: u64,
        dormant: bool,
        shadow: &mut TickShadow,
        degraded: &mut Vec<SimError>,
    ) -> Result<(), SimError> {
        self.shadow_vessels(shadow);
        let family = Self::spec_for(
            self.vessels
                .get(&vessel)
                .ok_or(SimError::Fatal("part failure: vessel vanished"))?,
            node,
        )
        .map(|s| s.family.clone());
        {
            let v = self
                .vessels
                .get_mut(&vessel)
                .ok_or(SimError::Fatal("part failure: vessel vanished"))?;
            let rec = v
                .reliability
                .get_mut(&node)
                .ok_or(SimError::Fatal("part failure: record vanished"))?;
            // The firing consumed this record's publish — clear the
            // flag so the slot re-derives the renewal crossing (a
            // stale `Some` would wedge the renewal chain: the
            // publish's `is_some` skip leaves no event for the flag).
            rec.pending_failure_tick = None;
            let lambda = rec.lambda_per_s;
            let dormant_class = rec.dormant_class;
            let burning = rec.burning;
            rec.restamp(tick, lambda, dormant_class, burning);
            // The crossing CONSUMES Λ: the part's hazard clock
            // restarts at the crossed threshold, so the next leg is
            // exactly the drawn `−ln Uₖ` — the same convention
            // `apply_mode_to_record` uses in-bubble. This also keeps
            // a *late* fire sane: a publish gap can land the event
            // ticks past the true crossing, and keeping the overshoot
            // would queue a catch-up cascade of backlog renewals
            // (Λ ≫ θ) instead of one clean spaced renewal.
            rec.usage.hazard_integral = rec.theta;
            if !rec.failures.contains(&mode) {
                rec.failures.push(mode);
            }
            rec.renewal_count += 1;
            // The renewal threshold: θₖ = θₖ₋₁ + (−ln Uₖ), `k` the
            // failure ordinal — same stream, next counter (module
            // doc). `renewal_count`, not `failures.len()`: a repeat
            // mode must still advance the draw series.
            let u = uniform(
                self.save_seed,
                StreamKey {
                    tag: StreamTag::Reliability,
                    entity: rec.instance.0,
                    counter: rec.renewal_count,
                },
            );
            // Same U = 0 guard as `fresh_record` — a +inf θ here is
            // serialized and then poisons every state hash forever.
            rec.theta += -libm::log(u.max(f64::MIN_POSITIVE));
            match mode {
                FailureMode::DegradedThrust => {
                    rec.thrust_factor = self.reliability_cfg.degraded_thrust_factor;
                }
                FailureMode::IgnitionFailure => {
                    rec.thrust_factor = 0.0;
                    rec.burning = false;
                    rec.misfire = true;
                }
                _ => {}
            }
        }
        // §3.4: "failures add qualification data" — the family's
        // observed-failure count credits through the M7-B point.
        if let Some(f) = family {
            self.shadow_tech(shadow);
            self.tech.credit_qualification(f, 0.0, 0.0, 1)?;
        }
        self.notify(
            Notification {
                tick,
                vessel,
                node: Some(node),
                kind: NotificationKind::PartFailed { mode, dormant },
            },
            shadow,
        );
        // `Explosion` has no effect branch here by design: dormant
        // fires never carry it (the publish's non-fatal filter and
        // `fire_part_failure`'s guard), and the in-bubble caller
        // routes it into the same-tick thermal-hit destruction list
        // (§3.4: explosions are the thermal/structural mechanism).
        let _ = degraded;
        Ok(())
    }

    /// The `RunBenchTest` apply — §3.4's test-stand facility: charge
    /// `bench_funds_per_unit · units` and `bench_resource_per_unit ·
    /// units` of `bench_resource` out of the launch site's inventory
    /// now, then the `BenchTestComplete` event credits `bench_units`
    /// at `now + ceil(units · bench_seconds_per_unit · TICK_HZ)`
    /// (the `ManufactureCraft` precedent: reservation at apply,
    /// delivery at the event).
    pub(crate) fn apply_bench_test(
        &mut self,
        site: SiteId,
        family: FamilyId,
        units: f64,
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        let cfg = self.reliability_cfg.clone();
        if !cfg.is_valid() {
            return Err(SimError::Fatal("reliability: invalid config"));
        }
        if !(units.is_finite() && units > 0.0) {
            return Err(SimError::Recoverable("bench test: non-positive units"));
        }
        let Some(def) = self.site_defs.get(&site) else {
            return Err(SimError::Recoverable("bench test: unknown site"));
        };
        if !def.launch {
            return Err(SimError::Recoverable(
                "bench test: test stands live at launch sites",
            ));
        }
        // C08-F6/C33-F3 §3.3: the stand is a facility unlock — the
        // tree's `unlocks.facilities` finally gates something. A
        // treeless world runs ungated (the `check_part_gate`
        // precedent); a treed world needs `facility.test_stand`
        // researched (authored at tech.rocketry.3).
        if !self.facility_unlocked(&csp_data::FacilityId::from("facility.test_stand")) {
            return Err(SimError::Recoverable("bench test: test stand not unlocked"));
        }
        if !self.economy.sites.contains_key(&site) {
            return Err(SimError::Recoverable("bench test: site has no state row"));
        }
        let cost = crate::economy::to_micros(units * cfg.bench_funds_per_unit);
        if cost > self.economy.funds_micros {
            return Err(SimError::Recoverable("bench test: insufficient funds"));
        }
        let need = units * cfg.bench_resource_per_unit;
        if need > 0.0 {
            let have = self
                .economy
                .sites
                .get(&site)
                .and_then(|st| st.inventory.get(&cfg.bench_resource))
                .copied()
                .unwrap_or(0.0);
            if have < need {
                return Err(SimError::Recoverable(
                    "bench test: insufficient site inventory",
                ));
            }
        }
        // Commit — spend atomically, then arena + event.
        self.shadow_economy(shadow);
        if need > 0.0 {
            let st = self
                .economy
                .sites
                .get_mut(&site)
                .ok_or(SimError::Fatal("bench test: site row vanished"))?;
            *st.inventory
                .get_mut(&cfg.bench_resource)
                .ok_or(SimError::Fatal("bench test: inventory row vanished"))? -= need;
        }
        crate::economy::charge(
            &mut self.economy,
            self.clock.ticks(),
            cost,
            crate::economy::FundsReason::BenchTest,
        );
        if shadow.benches.is_none() {
            shadow.benches = Some(self.bench_runs.clone());
            shadow.next_bench_id = Some(self.next_bench_id);
        }
        let id = BenchId(self.next_bench_id);
        self.next_bench_id = self
            .next_bench_id
            .checked_add(1)
            .ok_or(SimError::Fatal("bench id overflow"))?;
        // The `exhaust_tick` convention: ceil to ticks, saturate at
        // `u64::MAX` — an over-horizon run completes effectively
        // never — and always land strictly ahead of the clock. An
        // enormous duration used to `Fatal` on `checked_add`; both
        // factors are gated finite above, so the product is finite
        // or `+inf` and the `.min` clamp is the honest answer.
        let span = (units * cfg.bench_seconds_per_unit * TICK_HZ as f64)
            .ceil()
            .min(u64::MAX as f64) as u64;
        let complete_tick = self
            .clock
            .ticks()
            .saturating_add(span)
            .max(self.clock.ticks().saturating_add(1));
        self.bench_runs.insert(
            id,
            ActiveBench {
                id,
                site,
                family,
                units,
                complete_tick,
            },
        );
        self.schedule_event(
            EventKind::BenchTestComplete { run: id },
            complete_tick,
            shadow,
        )
    }

    /// The installed §3.4 tunables — observation only (the
    /// `economy_config()` precedent): the UI's bench-economics rows
    /// and tests read it; writes are construction config
    /// (`set_reliability_config`).
    pub fn reliability_config(&self) -> &ReliabilityConfig {
        &self.reliability_cfg
    }

    /// The test-stand arena — observation only (the `builds()`
    /// precedent): the UI's in-flight bench rows and tests read it;
    /// mutation is the `apply_bench_test`/`fire_bench_complete`
    /// pair's job.
    pub fn bench_runs(&self) -> &BTreeMap<BenchId, ActiveBench> {
        &self.bench_runs
    }

    /// `BenchTestComplete` firing — the run's bench units credit the
    /// family's §3.4 qualification record. Total (the event-pair
    /// precedent): a vanished record is a no-op.
    pub(crate) fn fire_bench_complete(
        &mut self,
        run: BenchId,
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        if shadow.benches.is_none() {
            shadow.benches = Some(self.bench_runs.clone());
            shadow.next_bench_id = Some(self.next_bench_id);
        }
        let Some(b) = self.bench_runs.remove(&run) else {
            return Ok(());
        };
        self.shadow_tech(shadow);
        self.tech.credit_qualification(b.family, 0.0, b.units, 0)
    }
}

/// The thrust multiplier a record's failure flags imply — `0.0`
/// while misfired or ignition-dead, `degraded_thrust_factor` on
/// `DegradedThrust`, else `1.0`. Idempotent; the tick pass and the
/// transitions both install it.
fn thrust_factor_for(rec: &PartReliability, cfg: &ReliabilityConfig) -> f64 {
    if rec.misfire || rec.has_mode(FailureMode::IgnitionFailure) {
        0.0
    } else if rec.has_mode(FailureMode::DegradedThrust) {
        cfg.degraded_thrust_factor
    } else {
        1.0
    }
}

/// The record-local half of a failure application — push the mode,
/// advance the renewal threshold `θₖ = θₖ₋₁ + (−ln Uₖ)` (same
/// `{RELIABILITY}` stream, counter = the new failure ordinal), and
/// install the mode's live flags. The caller handles the Λ stamp,
/// the notification, the family's `observed_failures` credit, and
/// any `Explosion` routing (§3.4's thermal/structural mechanism —
/// never reachable dormant).
fn apply_mode_to_record(
    rec: &mut PartReliability,
    mode: FailureMode,
    save_seed: u64,
    cfg: &ReliabilityConfig,
) {
    if !rec.failures.contains(&mode) {
        rec.failures.push(mode);
    }
    // The crossing CONSUMES Λ — the same convention the dormant
    // firing's `apply_failure_at` uses: Λ stamps to the crossed θ
    // and the next leg is exactly the drawn `−ln Uₖ`. Keeping the
    // per-tick overshoot instead would let a large accrual stride
    // cascade repeated crossings while Λ ≥ θ.
    rec.usage.hazard_integral = rec.theta;
    rec.renewal_count += 1;
    // `renewal_count`, not `failures.len()` — the dedup'd mode list
    // stalls on repeats; the draw series must not. The `MIN_POSITIVE`
    // clamp is the `fresh_record` U = 0 guard (a +inf θ bricks the
    // state hash permanently).
    let u = uniform(
        save_seed,
        StreamKey {
            tag: StreamTag::Reliability,
            entity: rec.instance.0,
            counter: rec.renewal_count,
        },
    );
    rec.theta += -libm::log(u.max(f64::MIN_POSITIVE));
    if mode == FailureMode::IgnitionFailure {
        rec.burning = false;
        rec.misfire = true;
    }
    rec.thrust_factor = thrust_factor_for(rec, cfg);
}

/// What one bubble tick's reliability pass hands back — the work
/// that needs `self` (the notification log, the family's
/// qualification credit, the destruction list) collected during the
/// borrow-split vessel loop and applied once `self` is whole.
#[derive(Clone, Debug, Default)]
pub(crate) struct TickEffects {
    /// Failures applied this tick — `(node, mode, dormant_class)`
    /// for the notification and the family's `observed_failures`.
    pub failures: Vec<(PartNodeId, FailureMode, bool)>,
    /// Nodes an `Explosion` mode destroyed — the caller folds them
    /// into the same-tick thermal-hit list (§3.4's mechanism).
    pub explosions: Vec<PartNodeId>,
    /// Ignition misfires — `IgnitionMisfire` notifications.
    pub misfires: Vec<PartNodeId>,
    /// Burn completions — `(family, burn_s, in_atmosphere)` for the
    /// flight-qualification credit.
    pub burns: Vec<(FamilyId, f64, bool)>,
    /// A `Leak` debit moved part mass — the caller re-welds.
    pub mass_changed: bool,
}

/// The tick's regime inputs a [`bubble_part_tick`] call reads —
/// grouped so the signature stays readable under the borrow split
/// (the caller's `body` borrow coexists with these disjoint fields).
pub(crate) struct BubbleEnv<'a> {
    /// `vessel.thermal.part_temps_k` — post-`thermal_step` temps.
    pub temps: &'a BTreeMap<PartNodeId, f64>,
    /// `body.throttle` — the fraction that drove this tick's forces.
    pub throttle: f64,
    /// Sustained acceleration in g (`‖Δv/dt‖/G0`) — `ExcessG`.
    pub g_load: f64,
    /// Per-node external-force magnitude (N) — `max_axial_n`.
    pub axial: &'a BTreeMap<PartNodeId, f64>,
    /// A ground contact exists — ullage is settled on the surface.
    pub on_ground: bool,
    /// Inside the body's atmosphere — the flight-credit regime.
    pub in_atmosphere: bool,
}

/// One bubble tick of §3.4 for one vessel — the per-part usage
/// accrual, ignition bookkeeping, hazard integration, leak debit,
/// and the inline `Λ ≥ θ` crossing. Runs inside `step_bubble_
/// range`'s borrow-split loop: every write lands on the passed-in
/// disjoint fields (`craft`, `records`, `stores`), and the
/// `self`-level effects ride back in [`TickEffects`]. Called
/// post-step at tick `t` (the accrual covers `(t, t+1]`); the
/// crossing applies at `t + 1`.
///
/// Determinism: every input is the tick's sim state plus the save
/// seed — the bubble already steps every integer tick identically
/// under every warp partition, so the tick stream IS the §2.6.5
/// mechanism in-bubble failures need (the module doc's publish
/// rule).
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub(crate) fn bubble_part_tick(
    craft: &mut crate::CraftDef,
    records: &mut BTreeMap<PartNodeId, PartReliability>,
    stores: &mut BTreeMap<PartNodeId, BTreeMap<ResourceId, f64>>,
    env: &BubbleEnv<'_>,
    tick: u64,
    save_seed: u64,
    quals: &csp_phys::FamilyQualifications,
    cfg: &ReliabilityConfig,
) -> TickEffects {
    let dt = 1.0 / TICK_HZ as f64;
    let throttle = env.throttle;
    let g_load = env.g_load;
    let axial = env.axial;
    let mut out = TickEffects::default();
    // A non-covering spec table (the latch merge's mismatched-halves
    // case) resolves no spec — positional reads would bind the wrong
    // part's spec, so every record sits this pass out (inert, never
    // wrong).
    let specs_ok = specs_parallel(craft);
    for (node, rec) in records.iter_mut() {
        let Some(eff) = craft
            .graph
            .nodes
            .get(node)
            .map(|n| n.effective_index as usize)
        else {
            continue;
        };
        let Some(spec) = craft.reliability.get(eff).filter(|_| specs_ok) else {
            continue;
        };
        let engine = spec.engine.is_some();
        let temp_k = env.temps.get(node).copied().unwrap_or(0.0);
        let max_k = craft
            .parts
            .get(eff)
            .map_or(1.0, |p| p.thermal.max_temp_k.max(1.0));
        let axial_n = axial.get(node).copied().unwrap_or(0.0);
        // — Ignition (§3.4's Bernoulli draw, one per start index) —
        if engine
            && throttle > 0.0
            && !rec.burning
            && !rec.misfire
            && !rec.has_mode(FailureMode::IgnitionFailure)
        {
            rec.usage.ignitions = rec.usage.ignitions.saturating_add(1);
            let k = rec.usage.ignitions;
            let ullage_neg = spec.engine.is_some_and(|e| e.ullage_required) && !env.on_ground;
            let qual = quals.get(&spec.family).map_or(0.0, |q| q.units());
            let m = RegimeMetrics {
                throttle,
                burn_s: 0.0,
                temp_fraction: temp_k / max_k,
                g_load,
                ullage_negative: ullage_neg,
                restarts: k.saturating_sub(1) as f64,
            };
            let lambda_ign = operating_lambda(
                spec,
                csp_phys::qual_factor(&spec.qualification_curve, qual),
                &m,
                rec.usage.operating_s,
                cfg,
            );
            if ignition_succeeds(save_seed, rec.instance, k, lambda_ign, cfg) {
                rec.burning = true;
                rec.burn_start_tick = tick;
                rec.ullage_stressed = ullage_neg;
            } else {
                rec.misfire = true;
                out.misfires.push(*node);
            }
        }
        // — Shutdown: burn end credits flight qualification; a
        //   misfire re-arms when the throttle returns to zero —
        if engine && rec.burning && throttle <= 0.0 {
            let burn_s = tick.saturating_sub(rec.burn_start_tick) as f64 * dt;
            rec.burning = false;
            rec.ullage_stressed = false;
            out.burns
                .push((spec.family.clone(), burn_s, env.in_atmosphere));
        }
        if engine && rec.misfire && throttle <= 0.0 {
            rec.misfire = false;
        }
        let operating = !engine || rec.burning;
        rec.thrust_factor = thrust_factor_for(rec, cfg);
        // — The §3.4 usage record's peaks —
        rec.usage.max_temp_k = rec.usage.max_temp_k.max(temp_k);
        rec.usage.max_axial_n = rec.usage.max_axial_n.max(axial_n);
        rec.usage.max_g = rec.usage.max_g.max(g_load);
        // — λ + the per-tick Λ accrual (Λ += λ·dt) —
        let burn_s = if rec.burning {
            tick.saturating_sub(rec.burn_start_tick) as f64 * dt
        } else {
            0.0
        };
        let qual = quals.get(&spec.family).map_or(0.0, |q| q.units());
        let m = RegimeMetrics {
            throttle,
            burn_s,
            temp_fraction: temp_k / max_k,
            g_load,
            ullage_negative: rec.ullage_stressed,
            restarts: rec.usage.ignitions.saturating_sub(1) as f64,
        };
        let lambda = if operating {
            operating_lambda(
                spec,
                csp_phys::qual_factor(&spec.qualification_curve, qual),
                &m,
                rec.usage.operating_s,
                cfg,
            )
        } else {
            spec.dormant_hazard_per_s(cfg.dormant_stress_scale)
        };
        rec.usage.hazard_integral += lambda * dt;
        if operating {
            rec.usage.operating_s += dt;
        } else {
            rec.usage.dormant_s += dt;
        }
        rec.epoch_tick = tick + 1;
        rec.lambda_per_s = lambda;
        rec.dormant_class = !operating;
        // — `Leak`: drain the node's stores at the authored rate —
        if rec.has_mode(FailureMode::Leak)
            && let Some(part) = craft.parts.get_mut(eff)
            && let Some(stores) = stores.get_mut(node)
        {
            out.mass_changed |=
                crate::transfers::leak_part(part, stores, cfg.leak_units_per_s * dt);
        }
        // — The crossing: Λ ≥ θ at `tick + 1` —
        if rec.usage.hazard_integral >= rec.theta {
            let modes: Vec<(FailureMode, f64)> = if operating {
                spec.failure_modes.clone()
            } else {
                dormant_modes(spec)
            };
            // `renewal_count` is the next failure's ordinal — the
            // dedup'd `failures` list would repeat an index (and its
            // draw) on a second application of the same mode.
            if let Some(mode) = draw_mode(save_seed, rec.instance, rec.renewal_count, &modes) {
                // A mid-burn `IgnitionFailure` kills the burn on a path
                // the shutdown branch never sees — credit the partial
                // burn like the throttle-to-zero exit does (the crossing
                // applies at `tick + 1`, so the burn ran
                // `tick + 1 − burn_start` ticks).
                if mode == FailureMode::IgnitionFailure && rec.burning {
                    let burn_s = (tick + 1).saturating_sub(rec.burn_start_tick) as f64 * dt;
                    out.burns
                        .push((spec.family.clone(), burn_s, env.in_atmosphere));
                }
                apply_mode_to_record(rec, mode, save_seed, cfg);
                out.failures.push((*node, mode, !operating));
                if mode == FailureMode::Explosion {
                    out.explosions.push(*node);
                }
            }
        }
    }
    out
}

/// Scale one body's `Thrust` sites to each part's live
/// `thrust_factor` — applied once per tick before `backend.step`
/// from the CRAFT's baseline `rated_n` (idempotent and self-healing
/// across every re-weld: the body's sites are a derived view).
/// `site_start`/`site_len` come from `PartWeld`; the baseline
/// `rated_n` is scalar, identical in either frame. Field-level
/// arguments so the caller's `body` borrow coexists.
pub(crate) fn apply_thrust_factors(
    body: &mut csp_phys::WeldedBody,
    craft: &crate::CraftDef,
    records: &BTreeMap<PartNodeId, PartReliability>,
) {
    for (node, rec) in records {
        let Some(eff) = craft
            .graph
            .nodes
            .get(node)
            .map(|n| n.effective_index as usize)
        else {
            continue;
        };
        let Some(w) = body.part_index.get(node) else {
            continue;
        };
        let factor = rec.thrust_factor;
        let start = w.site_start as usize;
        let end = ((w.site_start + w.site_len) as usize).min(body.force_sites.len());
        for (j, site) in body.force_sites[start..end].iter_mut().enumerate() {
            if site.kind != ForceSiteKind::Thrust {
                continue;
            }
            if let Some(base) = craft.parts.get(eff).and_then(|p| p.force_sites.get(j)) {
                site.rated_n = base.rated_n * factor;
            }
        }
    }
}

/// The undock/breakup record split — the `split_vessel_records`
/// precedent: the fragment keeps its members' records (their pending
/// publishes named the old vessel, so the flags clear for the publish
/// slot to re-derive under the new binding); the retained side keeps
/// the rest untouched (its publishes stay valid — same `VesselId`).
pub(crate) fn split_records(
    vessel: &mut Vessel,
    frag_nodes: &std::collections::BTreeSet<PartNodeId>,
) -> BTreeMap<PartNodeId, PartReliability> {
    let frag: BTreeMap<PartNodeId, PartReliability> = vessel
        .reliability
        .iter()
        .filter(|(id, _)| frag_nodes.contains(id))
        .map(|(id, r)| {
            let mut r = r.clone();
            r.pending_failure_tick = None;
            (*id, r)
        })
        .collect();
    vessel.reliability.retain(|id, _| !frag_nodes.contains(id));
    // `BTreeMap::iter` yields `(&K, &V)` — the clone above is the
    // fragment half; the retained side needs no clearing.
    frag
}

#[cfg(test)]
// §2.13's no-panic rule covers shipping fallible paths; tests signal
// failure by panicking, so unwrap/expect stays legal inside test code.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use csp_data::{QualCurve, QualKnot, StressKind, StressRule};

    /// A bench spec with a controllable dormant rate — the test
    /// fixture's `base_mtbf_s`/`dormant_stress`/`failure_modes` are
    /// the only knobs the assertions read.
    fn spec(dormant: f64, modes: Vec<(FailureMode, f64)>) -> ReliabilitySpec {
        ReliabilitySpec {
            family: FamilyId::from("f"),
            base_mtbf_s: 1_000.0,
            infant_mortality: 0.5,
            dormant_stress: dormant,
            regime_stress: vec![StressRule {
                kind: StressKind::OverThrottle,
                threshold: 0.8,
                multiplier: 3.0,
            }],
            failure_modes: modes,
            qualification_curve: QualCurve {
                knots: vec![QualKnot {
                    qualification_units: 0.0,
                    hazard_multiplier: 1.0,
                }],
            },
            engine: None,
        }
    }

    #[test]
    fn threshold_draws_are_deterministic_and_distinct() {
        // §3.4's anti-save-scum core: the same (seed, instance) pair
        // draws the same θ forever; different instances draw
        // different θ — the pre-draw is what revert preserves.
        let a = fresh_record(42, PartInstanceId(7), 1_000);
        let b = fresh_record(42, PartInstanceId(7), 1_000);
        assert_eq!(a.theta, b.theta);
        assert_eq!(a.instance, b.instance);
        let c = fresh_record(42, PartInstanceId(8), 1_000);
        assert_ne!(a.theta, c.theta, "instances draw distinct θ");
        assert!(a.theta > 0.0);
    }

    #[test]
    fn restamp_accrues_hazard_at_the_in_force_rate() {
        // Λ accrues exactly λ·Δt while a class holds — the piecewise-
        // constant integration the closed-form solve re-derives.
        let mut r = fresh_record(7, PartInstanceId(1), 1_000);
        // λ = 0.5 per sim-second; two sim-seconds of dormant class
        // accrue exactly 1.0 of Λ.
        r.restamp(1_000, 0.5, true, false);
        let before = r.usage.hazard_integral;
        r.restamp(1_000 + 2 * TICK_HZ, 0.5, true, false);
        assert!((r.usage.hazard_integral - before - 1.0).abs() < 1e-9);
    }

    #[test]
    fn dormant_crossing_solves_closed_form_and_skips_disabled() {
        // θ − Λ_remaining over the constant λ gives the tick; a zero
        // dormant rate never crosses (the off-switch, §3.4).
        let mut r = fresh_record(7, PartInstanceId(2), 1_000);
        r.usage.hazard_integral = r.theta / 2.0;
        // λ = 0.5/s ⇒ the remaining θ/2 takes `ceil(θ·TICK_HZ)`
        // ticks (remaining_s × TICK_HZ, ceiled).
        let lambda = 0.5;
        r.restamp(2_000, lambda, true, false);
        let crossing = dormant_failure_tick(&r, lambda);
        let expected = 2_000 + libm::ceil(r.theta / 2.0 / lambda * TICK_HZ as f64) as u64;
        assert_eq!(crossing, Some(expected));
        assert_eq!(dormant_failure_tick(&r, 0.0), None);
    }

    #[test]
    fn dormant_modes_never_produce_fatal() {
        // §3.4: "Explosions are reserved for thermal/structural" —
        // the dormant publish draws from the non-fatal table only.
        let modes = spec(
            0.5,
            vec![(FailureMode::Explosion, 0.9), (FailureMode::Leak, 0.1)],
        );
        let non_fatal = dormant_modes(&modes);
        assert!(!non_fatal.iter().any(|(m, _)| m.is_fatal()));
        assert_eq!(non_fatal, vec![(FailureMode::Leak, 0.1)]);
        // An all-fatal table draws nothing dormant — the part is
        // invulnerable while racked.
        let all_fatal = spec(0.5, vec![(FailureMode::Explosion, 1.0)]);
        assert!(dormant_modes(&all_fatal).is_empty());
        assert_eq!(draw_mode(7, PartInstanceId(1), 0, &[]), None);
    }

    #[test]
    fn mode_draw_is_deterministic_and_weighted() {
        // The same (seed, instance, index) triple draws the same mode;
        // a single-entry table always selects its one mode.
        let modes = vec![(FailureMode::Leak, 0.99), (FailureMode::PanelJam, 0.01)];
        assert_eq!(
            draw_mode(7, PartInstanceId(3), 0, &modes),
            draw_mode(7, PartInstanceId(3), 0, &modes)
        );
        assert_eq!(
            draw_mode(7, PartInstanceId(3), 0, &[(FailureMode::Leak, 1.0)]),
            Some(FailureMode::Leak)
        );
    }

    #[test]
    fn ignition_draw_is_per_index_and_low_prob_lights() {
        // §3.4's "ignitions are deterministic per ignition index" —
        // the same (instance, k) draw replays identically; a healthy
        // part lights, a dead one does not.
        let cfg = default_config();
        // λ ≈ 0 ⇒ the hazard window never trips.
        assert!(ignition_succeeds(7, PartInstanceId(1), 1, 1e-12, &cfg));
        // λ huge ⇒ the same draw fails.
        assert!(!ignition_succeeds(7, PartInstanceId(1), 1, 1e30, &cfg));
        // Index-dependence: k=1 vs k=2 is a different draw stream.
        let mid = 1.0 / cfg.ignition_hazard_s; // P(fail) ≈ 63%
        let mut k1 = 0;
        let mut k2 = 0;
        for i in 0..40u64 {
            k1 += ignition_succeeds(7, PartInstanceId(i), 1, mid, &cfg) as u32;
            k2 += ignition_succeeds(7, PartInstanceId(i), 2, mid, &cfg) as u32;
        }
        // Both fire roughly half — but the per-index sequences differ
        // (the counter is in the key).
        assert_ne!(k1, k2);
    }

    #[test]
    fn qualification_drops_the_operating_hazard() {
        // §3.4's "qualified parts fail less" — a steeper curve at
        // high units cuts λ by the curve's factor.
        let mut s = spec(0.5, vec![(FailureMode::Leak, 1.0)]);
        s.qualification_curve = QualCurve {
            knots: vec![
                QualKnot {
                    qualification_units: 0.0,
                    hazard_multiplier: 1.0,
                },
                QualKnot {
                    qualification_units: 100.0,
                    hazard_multiplier: 0.1,
                },
            ],
        };
        let m = RegimeMetrics::default();
        let cfg = default_config();
        let fresh = operating_lambda(&s, 1.0, &m, 0.0, &cfg);
        let qualified = operating_lambda(&s, 0.1, &m, 0.0, &cfg);
        assert!((qualified * 10.0 - fresh).abs() / fresh < 1e-9);
        assert!(qualified < fresh);
    }

    #[test]
    fn regime_stress_multiplies_the_hazard() {
        // OverThrottle at 0.9 throttle triples λ over baseline.
        let s = spec(0.5, vec![(FailureMode::Leak, 1.0)]);
        let cfg = default_config();
        let m = RegimeMetrics {
            throttle: 0.9,
            ..RegimeMetrics::default()
        };
        let stressed = operating_lambda(&s, 1.0, &m, 0.0, &cfg);
        let calm = operating_lambda(&s, 1.0, &RegimeMetrics::default(), 0.0, &cfg);
        assert!((stressed / calm - 3.0).abs() < 1e-9);
    }

    #[test]
    fn renewal_threshold_extends_the_draw_series() {
        // After the first failure the next θ adds the {RELIABILITY,
        // instance, 1} draw — the renewal chain is deterministic and
        // strictly increasing.
        let mut r = fresh_record(7, PartInstanceId(9), 1_000);
        let theta1 = r.theta;
        apply_mode_to_record(&mut r, FailureMode::Leak, 7, &default_config());
        assert_eq!(r.failures, vec![FailureMode::Leak]);
        assert!(r.theta > theta1);
        let theta2 = r.theta;
        apply_mode_to_record(&mut r, FailureMode::SensorFault, 7, &default_config());
        assert!(r.theta > theta2);
    }

    #[test]
    fn infant_mortality_decays_over_operating_time() {
        // A fresh part carries extra hazard that fades — the
        // `exp(−t/τ)` wing over the flat λ_base.
        let s = spec(0.5, vec![(FailureMode::Leak, 1.0)]);
        let cfg = default_config();
        let m = RegimeMetrics::default();
        let young = operating_lambda(&s, 1.0, &m, 0.0, &cfg);
        let old = operating_lambda(&s, 1.0, &m, cfg.infant_tau_s * 100.0, &cfg);
        assert!(young > old);
        assert!((old - s.base_hazard_per_s()).abs() / s.base_hazard_per_s() < 0.01);
    }

    #[test]
    fn mission_confidence_is_exp_minus_expected_hazard() {
        // §3.4's formula — `exp(−Λ_expected)`: a partless craft is
        // certain; a burn segment accrues `λ·t` over each segment.
        let profile = csp_phys::MissionProfile::terrin_ascent();
        let quals = csp_phys::FamilyQualifications::default();
        assert_eq!(csp_phys::confidence(&[], &quals, &profile), 1.0);
        let s = spec(0.5, vec![(FailureMode::Leak, 1.0)]);
        let c = csp_phys::confidence(std::slice::from_ref(&s), &quals, &profile);
        let expected = libm::exp(-csp_phys::expected_hazard(&[s], &quals, &profile));
        assert!((c - expected).abs() < 1e-12);
        assert!(c < 1.0 && c > 0.0);
        // More qualification → less hazard → higher confidence —
        // the spec needs a descending curve for the units to bite.
        let mut quals = csp_phys::FamilyQualifications::default();
        quals.entry(FamilyId::from("f")).or_default().flight_units = 500.0;
        let mut curved = spec(0.5, vec![(FailureMode::Leak, 1.0)]);
        curved.qualification_curve = QualCurve {
            knots: vec![
                QualKnot {
                    qualification_units: 0.0,
                    hazard_multiplier: 1.0,
                },
                QualKnot {
                    qualification_units: 100.0,
                    hazard_multiplier: 0.1,
                },
            ],
        };
        assert!(csp_phys::confidence(&[curved], &quals, &profile) > c);
    }

    #[test]
    fn record_round_trips_through_clone_and_eq() {
        // The record is serialized state — a restore must read back
        // the identical record (the θ-preservation contract the
        // snapshot tests exercise end-to-end).
        let mut r = fresh_record(7, PartInstanceId(5), 1_000);
        r.usage.operating_s = 42.0;
        r.failures.push(FailureMode::Leak);
        r.misfire = true;
        assert_eq!(r, r.clone());
    }
}
