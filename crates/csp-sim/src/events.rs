//! The §2.6.5 event queue's data types: the two event classes, the event
//! kinds their members fall into, the scheduled-event heap entry, and
//! the constant subsystem query/advance order array.
//!
//! Design rules carried here:
//!
//! - **Class is carried in the enum** (§2.6.5): `ScheduledEvent.class`
//!   is assigned at schedule time by [`class_of`] from the kind plus the
//!   tracked-vessel opt-in — the policy is not sprinkled across
//!   subsystems. The opt-in also re-writes pending entries' `class`
//!   (see `World`'s `SetVesselTracked` apply).
//! - **Kinds have producers only.** The §2.6.5 member tables name ten
//!   `Clamping` and six `Silent` members; variants whose subsystems do
//!   not exist yet (`ReliabilityInspectionDue`, `PartFailure`)
//!   are deliberately absent — they land WITH their
//!   producing subsystems (no speculative filler). M8-A landed the
//!   §3.5.1 colony kinds — `TerminatorCrossing`, `ColonyStore*`
//!   (`Silent`), `ColonyInspection`, `PowerLost`/`PowerRestored`, and
//!   `LifeSupportDepleted` (`Clamping`). `SoiRescan` is the
//!   one kind the plan does not name: it is the Orbits subsystem's own
//!   publish deadline when the §2.6.6 scan finds no crossing inside the
//!   query horizon — the mechanism that keeps "publish your next event
//!   time" honest. Always `Silent`: a re-query must never clamp warp.
//!   `TransferWindow` is the second plan-implied kind: the M5 exit row's
//!   "transfer-window events" (the §-compatibility table's "Kerbal
//!   Alarm Clock — transfer-window planner, falls out of the event
//!   queue"). It shares `PlayerAlarm`'s shape — player-requested,
//!   `Clamping`, mutates nothing — but carries a structured body pair
//!   so the notification surfaces can render it without label parsing.
//! - **Heap order is `(tick, seq)`.** `event_seq` is a serialized
//!   monotonic counter (§7.2) assigned at insertion — so ordering is
//!   total under tick ties, and because insertion order follows the
//!   subsystem order array, the counter is hash-relevant state.
//!
//! # The constant order array
//!
//! [`SUBSYSTEM_ORDER`] is the §2.6.5 query/advance order,
//! verbatim: `[Orbits, Vessels, Power, Colonies, LifeSupport, Research,
//! Manufacturing, Construction, Reliability, Contracts, Scripts,
//! Alarms]`. It exists NOW, while most of those subsystems are stubs,
//! because `event_seq` is assigned at insertion and therefore depends on
//! it — reordering it is a replay-breaking and save-breaking change that
//! requires a `HASH_SCHEMA_VERSION` bump.

use std::cmp::Ordering;
use std::collections::BTreeSet;

use csp_core::{StableDiscriminant, StateHasher};
use csp_data::BodyId;
use serde::{Deserialize, Serialize};

use crate::VesselId;
use crate::orbit::SoiDirection;

/// §2.6.5's constant subsystem query/advance order. The discriminants
/// are the slot indices — declared, append-only, never reordered.
#[repr(u16)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Subsystem {
    /// Orbits on rails — propagates all rails vessels (index 0).
    Orbits = 0,
    /// Vessel-level state (index 1).
    Vessels = 1,
    /// Power (index 2).
    Power = 2,
    /// Colonies (index 3).
    Colonies = 3,
    /// Life support (index 4).
    LifeSupport = 4,
    /// Research (index 5).
    Research = 5,
    /// Manufacturing (index 6).
    Manufacturing = 6,
    /// Construction (index 7).
    Construction = 7,
    /// Reliability (index 8).
    Reliability = 8,
    /// Contracts (index 9).
    Contracts = 9,
    /// Scripts (index 10).
    Scripts = 10,
    /// Alarms (index 11).
    Alarms = 11,
}

impl StableDiscriminant for Subsystem {
    fn discriminant(&self) -> u16 {
        *self as u16
    }
}

/// The §2.6.5 subsystem query/advance order — a CONSTANT ARRAY, verbatim:
/// `[Orbits, Vessels, Power, Colonies, LifeSupport, Research,
/// Manufacturing, Construction, Reliability, Contracts, Scripts, Alarms]`.
/// Reordering it reorders `event_seq` assignment → replay-breaking and
/// save-breaking → `HASH_SCHEMA_VERSION` bump (§2.6.5).
pub const SUBSYSTEM_ORDER: [Subsystem; 12] = [
    Subsystem::Orbits,
    Subsystem::Vessels,
    Subsystem::Power,
    Subsystem::Colonies,
    Subsystem::LifeSupport,
    Subsystem::Research,
    Subsystem::Manufacturing,
    Subsystem::Construction,
    Subsystem::Reliability,
    Subsystem::Contracts,
    Subsystem::Scripts,
    Subsystem::Alarms,
];

/// §2.6.5's two event classes — carried on the event so the policy is
/// not sprinkled across subsystems.
#[repr(u16)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventClass {
    /// Auto-demotes warp so the player cannot overshoot — surfaced to
    /// `csp-app`'s `WarpController` via `AdvanceReport::fired_events`.
    Clamping = 0,
    /// Processed inside `advance_to`, never touches warp — the class that
    /// keeps warp usable at scale (20 colonies + 500 on-rails objects
    /// must not leave warp perpetually clamped, §2.6.5).
    Silent = 1,
}

impl StableDiscriminant for EventClass {
    fn discriminant(&self) -> u16 {
        *self as u16
    }
}

/// What the event IS — the §2.6.5 member lists, restricted to kinds that
/// have an M1 producer (module docs). Discriminants are explicit, stable
/// `u16`s (§7.1 rule 7): append-only, never reordered.
#[repr(u16)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventKind {
    /// A vessel's §2.6.6 sphere-of-influence crossing. ONE kind carries
    /// both table members — focused/tracked vs other vessels — because
    /// the class is assigned per-object by the tracked opt-in
    /// ([`class_of`]), not by the variant.
    SoiTransition {
        /// The vessel crossing the boundary.
        vessel: VesselId,
        /// The body whose SOI is crossed (the new parent on `Enter`, the
        /// old parent on `Exit`).
        body: BodyId,
        /// `Enter` = into `body`'s SOI; `Exit` = out of it.
        direction: SoiDirection,
    } = 0,
    /// The Orbits subsystem's publish deadline for a vessel whose §2.6.6
    /// scan found no crossing inside the query horizon — fires `Silent`
    /// to re-run the query over the next horizon. The mechanism behind
    /// §2.6.5's "any subsystem with nonlinear or thresholded behaviour
    /// must publish its next event time": when the honest answer is "no
    /// event before H", `H` is itself the published deadline.
    SoiRescan {
        /// The vessel to re-scan.
        vessel: VesselId,
    } = 1,
    /// A player alarm (§2.6.5 `Clamping` member) — scheduled by
    /// `SimCommand::SetAlarm` so it is §7.6-logged and replayable. Firing
    /// mutates nothing; surfacing rides `AdvanceReport::fired_events`.
    PlayerAlarm {
        /// Free-form label for the notification surface.
        label: String,
    } = 2,
    /// §2.9's debris-decay removal (Silent member): a debris-flagged
    /// vessel whose rails periapsis dips below its parent's atmosphere
    /// top (or the surface, on an airless body) burns up / impacts at
    /// the periapsis tick. Scheduled by `ToRails` on debris vessels.
    DebrisDecay {
        /// The debris vessel to remove.
        vessel: VesselId,
    } = 3,
    /// §2.6.5 `Clamping` member + the M5 maneuver executor's trigger
    /// (module docs): a scheduled [`ManeuverNode`] reached its burn
    /// tick — firing materializes the vessel in the bubble at the node
    /// point and arms the executor. Always `Clamping` per the §2.6.5
    /// member table (unlike `SoiTransition` it is not tracked-gated —
    /// the player armed the node themselves, so warp must stop for it).
    /// Scheduled by `SimCommand::SetManeuver`; dropped alongside the
    /// node by `DeleteManeuver` and by the residence transitions that
    /// discard the scheduled intent.
    ManeuverNodeStart {
        /// The vessel whose node reached its burn tick.
        vessel: VesselId,
    } = 4,
    /// A predicted transfer window's optimal departure (M5
    /// "transfer-window events"): scheduled by
    /// `SimCommand::WatchTransferWindow` at the tick a
    /// `transfer::plan_transfer` run produced. Firing mutates
    /// nothing — surfacing rides `AdvanceReport::fired_events`, same
    /// contract as `PlayerAlarm`. Always `Clamping`: the player asked
    /// for the warning, so warp must demote rather than overshoot the
    /// window (not tracked-gated — the event names no vessel).
    TransferWindow {
        /// The body the transfer departs.
        origin: BodyId,
        /// The body it targets.
        destination: BodyId,
    } = 5,
    /// M6-D §3.5's `ResourceTransfer` completion — the process lands
    /// its cargo (resource units into the destination's tanks, or a
    /// crew member into a free seat). Scheduled by
    /// `SimCommand::TransferResource`/`TransferCrew` at the
    /// rate-computed tick; pairs with exactly one `ActiveTransfer`
    /// record (the restore coherence check, like the maneuver node's).
    /// Always `Clamping` — the player asked for the transfer, so warp
    /// demotes rather than streaming past its landing (§3.5's
    /// "publishes `TransferComplete` so it survives warp").
    TransferComplete {
        /// The transfer arena id the event completes.
        transfer: crate::transfers::TransferId,
    } = 6,
    /// §3.2's lab exhaustion (M7-A): the project's escrowed data has
    /// decayed under the `data_epsilon` floor — the firing banks the
    /// conversion and consumes the project record. Scheduled by
    /// `SimCommand::StartResearch` at the closed-form-predicted tick,
    /// paired with a same-tick `ResearchProjectComplete` (this one
    /// fires first — it was scheduled first, and `(tick, seq)` order
    /// is insertion order). Always `Clamping` (§3.2 marks both
    /// events clamping — the player sees the conversion land).
    ResearchDataExhausted {
        /// The research arena id the event exhausts.
        project: crate::research::ResearchId,
    } = 7,
    /// §3.2's lab completion (M7-A): the player-facing "research
    /// done" landing — fires immediately after the same-tick
    /// `ResearchDataExhausted` consumed the record (firing is the
    /// notification; an orphan — a stopped or destroyed project —
    /// fires as a no-op). Always `Clamping`.
    ResearchProjectComplete {
        /// The research arena id the event completes.
        project: crate::research::ResearchId,
    } = 8,
    /// §3.5's recurring salary drain (M7-C): fires once per
    /// `salary_period_ticks` on the absolute period grid, debits
    /// `Σ salary_per_s · period_s` over the `Active` crew roster,
    /// and re-arms itself one period further. Always `Silent` —
    /// routine bookkeeping must never demote warp (§2.6.5); the
    /// shortfall it can cause is surfaced by the separate
    /// `Insolvent` event instead.
    SalaryDue = 9,
    /// §3.5's bankruptcy notification (M7-C): a funds charge clamped
    /// at zero left an unpaid shortfall — scheduled at `clock + 1`
    /// by the charge path, once per insolvency (`economy.insolvent`
    /// gates re-scheduling until a credit clears it). Firing mutates
    /// nothing — surfacing rides `AdvanceReport::fired_events`, the
    /// `PlayerAlarm` contract. Always `Clamping` per the §2.6.5
    /// member table — warp demotes so the player sees the
    /// insolvency, never sleeps through it.
    Insolvent {
        /// The unpaid micro-funds the clamp could not cover.
        shortfall_micros: u64,
    } = 10,
    /// §3.6's contract expiry (M7-D; §2.6.5 `Clamping` member): a
    /// contract's `deadline_tick` arrived — the contract fails and
    /// its authored penalty charges through the funds clamp.
    /// Scheduled by `SimCommand::AcceptContract`; dropped by
    /// completion (the strict event pairing, like the maneuver
    /// node's). Always `Clamping` — the player must see a contract
    /// die, never sleep through it.
    ContractDeadline {
        /// The contract arena id the deadline expires.
        contract: crate::contracts::ContractId,
    } = 11,
    /// §3.5.1 rule 1's insolation step (M8-A; §2.6.5 `Silent`
    /// member): the colony's local sunrise/sunset — a closed form
    /// over body rotation and orbital phase. Firing stamps the new
    /// `Daylight` and re-solves the flow network.
    TerminatorCrossing {
        /// The colony crossing the terminator.
        colony: crate::ColonyId,
    } = 12,
    /// §3.5.1 rule 4's filling bound (M8-A; §2.6.5 `Silent` member):
    /// the colony's earliest store-bound crossing reached its tick —
    /// the resource's level clamped at capacity. Firing re-anchors
    /// the stores and re-solves; a stale firing (rates changed since
    /// it was scheduled) is an early recompute, never a wrong level.
    ColonyStoreFull {
        /// The colony whose store filled.
        colony: crate::ColonyId,
        /// The resource that reached capacity.
        resource: csp_data::ResourceId,
    } = 13,
    /// §3.5.1 rule 4's emptying bound (M8-A; §2.6.5 `Silent` member):
    /// same contract as `ColonyStoreFull`, at the zero bound.
    ColonyStoreEmpty {
        /// The colony whose store emptied.
        colony: crate::ColonyId,
        /// The resource that reached zero.
        resource: csp_data::ResourceId,
    } = 14,
    /// §3.5.1 rule 4's "next scheduled inspection" (M8-A): the
    /// colony's authored re-solve deadline — scheduled by
    /// `SimCommand::ScheduleColonyInspection` and by the `Colonies`
    /// slot's publish repair. Always `Silent`.
    ColonyInspection {
        /// The colony to re-solve.
        colony: crate::ColonyId,
    } = 15,
    /// §3.5.1 rule 5's power transition (M8-A): scheduled one tick
    /// out by the recompute when EC demand outruns production plus a
    /// dead battery — firing sets `Degraded{PowerLost}`. `Clamping`:
    /// a colony going dark is the rare transition the player must
    /// not sleep through (the `LifeSupportDepleted` precedent — a
    /// transition, not a periodic).
    PowerLost {
        /// The colony that lost power.
        colony: crate::ColonyId,
    } = 16,
    /// The inverse transition (M8-A): scheduled when a
    /// `Degraded{PowerLost}` colony's recompute finds EC demand
    /// served again — firing returns it to `Nominal`. `Clamping`,
    /// same contract as `PowerLost`.
    PowerRestored {
        /// The colony whose power returned.
        colony: crate::ColonyId,
    } = 17,
    /// §3.5.1 rule 5 + §2.6.5's `Clamping` member (M8-A): a
    /// life-support-required store is empty and under-granted with
    /// crew aboard — firing marks the crew `Lost` and drops the
    /// colony to `Dormant` (it schedules nothing ever again).
    LifeSupportDepleted {
        /// The colony whose life support ran out.
        colony: crate::ColonyId,
    } = 18,
    /// §3.5's build order delivery (M8-B): firing materializes the
    /// ordered craft as a surface-latched vessel at the ordering
    /// launch site — the `ManufactureCraft` order spent its BOM and
    /// funds at apply, so the firing is the only place the hull can
    /// appear. `Clamping`: a finished build is the member-table
    /// `ConstructionComplete` the player must not sleep through.
    ConstructionComplete {
        /// The build order completing.
        build: crate::manufacture::BuildId,
    } = 19,
    /// M8-C §3.5's periodic vessel life-support check — the
    /// `SalaryDue` precedent: one `Silent` event per
    /// `life_support_cfg.check_period_ticks` on the absolute grid,
    /// lazily published by the `LifeSupport` slot while any vessel
    /// carries crew and life support is enabled. Firing debits
    /// `rate · period` per crew member aboard each crewed vessel and
    /// schedules the `Clamping` depletion transitions the shortfalls
    /// earn. Always `Silent` — routine bookkeeping must never demote
    /// warp.
    LifeSupportCheck = 20,
    /// M8-C §3.5's vessel-side depletion transition — scheduled one
    /// tick out by a `LifeSupportCheck` firing when a crewed vessel
    /// cannot cover a required resource's debit; firing marks the
    /// vessel's crew `Lost`. `Clamping`: crew dying is the named
    /// §2.6.5 member the player must not sleep through (the colony's
    /// `LifeSupportDepleted` precedent). Names a vessel that may no
    /// longer exist — the `TransferComplete` precedent: a stale
    /// firing is a no-op, and the event is never scrubbed on vessel
    /// removal.
    VesselLifeSupportDepleted {
        /// The crewed vessel whose life support ran out.
        vessel: VesselId,
    } = 21,
    /// M9a §2.10's script wake — the `Scripts` subsystem's member of
    /// the §2.6.5 `Clamping` table. A script registered a
    /// closed-form-resolvable wake condition; firing runs the
    /// script's `on_tick` at this tick even under rails warp (the
    /// event IS the tick for a non-bubble script). `Clamping` —
    /// warp demotes so the player cannot overshoot the wake, the
    /// `ManeuverNodeStart` precedent. Pairs one-to-one with
    /// `ScriptState::wake_tick` (re-registration scrubs the pending
    /// event and reschedules); a stale firing — script unloaded,
    /// disabled, or `wake_tick` moved — is a tolerated no-op like
    /// `TransferComplete`.
    ScriptWake {
        /// The script arena id whose tick fires.
        script: crate::ScriptId,
    } = 22,
}

impl StableDiscriminant for EventKind {
    fn discriminant(&self) -> u16 {
        match self {
            Self::SoiTransition { .. } => 0,
            Self::SoiRescan { .. } => 1,
            Self::PlayerAlarm { .. } => 2,
            Self::DebrisDecay { .. } => 3,
            Self::ManeuverNodeStart { .. } => 4,
            Self::TransferWindow { .. } => 5,
            Self::TransferComplete { .. } => 6,
            Self::ResearchDataExhausted { .. } => 7,
            Self::ResearchProjectComplete { .. } => 8,
            Self::SalaryDue => 9,
            Self::Insolvent { .. } => 10,
            Self::ContractDeadline { .. } => 11,
            Self::TerminatorCrossing { .. } => 12,
            Self::ColonyStoreFull { .. } => 13,
            Self::ColonyStoreEmpty { .. } => 14,
            Self::ColonyInspection { .. } => 15,
            Self::PowerLost { .. } => 16,
            Self::PowerRestored { .. } => 17,
            Self::LifeSupportDepleted { .. } => 18,
            Self::ConstructionComplete { .. } => 19,
            Self::LifeSupportCheck => 20,
            Self::VesselLifeSupportDepleted { .. } => 21,
            Self::ScriptWake { .. } => 22,
        }
    }
}

/// §2.6.5's class assignment — the ONE place the rule lives: the
/// tracked-vessel opt-in promotes that object's `SoiTransition`
/// `Silent → Clamping`; `PlayerAlarm` is always `Clamping`; `SoiRescan`
/// is always `Silent` (a re-query must never clamp warp).
pub fn class_of(kind: &EventKind, tracked: &BTreeSet<VesselId>) -> EventClass {
    match kind {
        EventKind::SoiTransition { vessel, .. } => {
            if tracked.contains(vessel) {
                EventClass::Clamping
            } else {
                EventClass::Silent
            }
        }
        EventKind::SoiRescan { .. }
        | EventKind::DebrisDecay { .. }
        | EventKind::SalaryDue
        // §3.5.1's boundary deadlines — the §2.6.5 `Silent` members
        // (`TerminatorCrossing`, `ColonyStoreFull`/`Empty`) plus the
        // inspection re-solve, which is pure bookkeeping.
        | EventKind::TerminatorCrossing { .. }
        | EventKind::ColonyStoreFull { .. }
        | EventKind::ColonyStoreEmpty { .. }
        | EventKind::ColonyInspection { .. }
        // M8-C: the periodic vessel check is bookkeeping like
        // `SalaryDue` — never clamps warp.
        | EventKind::LifeSupportCheck => {
            EventClass::Silent
        }
        EventKind::ContractDeadline { .. }
        | EventKind::ConstructionComplete { .. }
        // §3.5.1's transition events — `LifeSupportDepleted` is a
        // named §2.6.5 `Clamping` member; `PowerLost`/`PowerRestored`
        // share its "a transition, not a periodic" contract.
        | EventKind::PowerLost { .. }
        | EventKind::PowerRestored { .. }
        | EventKind::LifeSupportDepleted { .. }
        // M8-C: the vessel-side depletion shares the colony's
        // "a transition, not a periodic" `Clamping` contract.
        | EventKind::VesselLifeSupportDepleted { .. }
        // M9a §2.10: the script wake is a named §2.6.5 `Clamping`
        // member — warp demotes so a sleeping script's wake tick is
        // never streamed past.
        | EventKind::ScriptWake { .. } => EventClass::Clamping,
        EventKind::PlayerAlarm { .. }
        | EventKind::ManeuverNodeStart { .. }
        | EventKind::TransferWindow { .. }
        | EventKind::TransferComplete { .. }
        | EventKind::ResearchDataExhausted { .. }
        | EventKind::ResearchProjectComplete { .. }
        | EventKind::Insolvent { .. } => EventClass::Clamping,
    }
}

/// One heap entry: §2.6.5's `ScheduledEvent` keyed by `(tick, seq)`.
/// `seq` is the serialized `event_seq` counter assigned at insertion —
/// it makes the ordering total under tick ties, and it is hashed state
/// (§7.2).
///
/// `Ord` is INVERTED on `(tick, seq)` so `std::collections::BinaryHeap`
/// — a max-heap — pops the EARLIEST `(tick, seq)` first.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduledEvent {
    /// The sim tick the event fires at. Invariant (enforced by
    /// `World::schedule_event`): strictly ahead of the clock.
    pub tick: u64,
    /// The insertion-order `event_seq` — the tie-break that makes heap
    /// ordering total.
    pub seq: u64,
    /// The §2.6.5 class — assigned at schedule (and re-assigned by the
    /// tracked opt-in), carried on the entry.
    pub class: EventClass,
    /// What the event is.
    pub kind: EventKind,
}

impl ScheduledEvent {
    /// The §7.1 field walk for one event — `World::state_hash` calls it
    /// under the heap's canonical `(tick, seq)` enumeration.
    pub fn hash_into(&self, h: &mut StateHasher) {
        h.u64(self.tick);
        h.u64(self.seq);
        h.stable_enum(&self.class);
        h.stable_enum(&self.kind);
        match &self.kind {
            EventKind::SoiTransition {
                vessel,
                body,
                direction,
            } => {
                h.u64(vessel.0);
                h.str(body.as_str());
                h.stable_enum(direction);
            }
            EventKind::SoiRescan { vessel } => {
                h.u64(vessel.0);
            }
            EventKind::PlayerAlarm { label } => {
                h.str(label);
            }
            EventKind::DebrisDecay { vessel } => {
                h.u64(vessel.0);
            }
            EventKind::ManeuverNodeStart { vessel } => {
                h.u64(vessel.0);
            }
            EventKind::TransferWindow {
                origin,
                destination,
            } => {
                h.str(origin.as_str());
                h.str(destination.as_str());
            }
            EventKind::TransferComplete { transfer } => {
                h.u64(transfer.0);
            }
            EventKind::ResearchDataExhausted { project }
            | EventKind::ResearchProjectComplete { project } => {
                h.u64(project.0);
            }
            EventKind::SalaryDue => {}
            EventKind::Insolvent { shortfall_micros } => {
                h.u64(*shortfall_micros);
            }
            EventKind::ContractDeadline { contract } => {
                h.u64(contract.0);
            }
            EventKind::TerminatorCrossing { colony }
            | EventKind::ColonyInspection { colony }
            | EventKind::PowerLost { colony }
            | EventKind::PowerRestored { colony }
            | EventKind::LifeSupportDepleted { colony } => {
                h.u64(colony.0);
            }
            EventKind::ColonyStoreFull { colony, resource }
            | EventKind::ColonyStoreEmpty { colony, resource } => {
                h.u64(colony.0);
                h.str(resource.as_str());
            }
            EventKind::ConstructionComplete { build } => {
                h.u64(build.0);
            }
            EventKind::LifeSupportCheck => {}
            EventKind::VesselLifeSupportDepleted { vessel } => {
                h.u64(vessel.0);
            }
            EventKind::ScriptWake { script } => {
                h.u64(script.0);
            }
        }
    }
}

impl Ord for ScheduledEvent {
    /// Inverted `(tick, seq)` compare — `BinaryHeap`'s top is the
    /// EARLIEST event.
    fn cmp(&self, other: &Self) -> Ordering {
        (other.tick, other.seq).cmp(&(self.tick, self.seq))
    }
}

impl PartialOrd for ScheduledEvent {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[cfg(test)]
// Tests signal failure by panicking; §2.13's no-panic rule covers the
// shipping fallible paths, not test code.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::collections::BinaryHeap;

    fn ev(tick: u64, seq: u64, kind: EventKind) -> ScheduledEvent {
        ScheduledEvent {
            tick,
            seq,
            class: EventClass::Silent,
            kind,
        }
    }

    /// The heap pops the earliest `(tick, seq)` — the §2.6.5 total order
    /// under tick ties is `event_seq` (insertion order).
    #[test]
    fn heap_pops_earliest_tick_then_seq() {
        let mut h = BinaryHeap::new();
        h.push(ev(
            5,
            2,
            EventKind::SoiRescan {
                vessel: VesselId(0),
            },
        ));
        h.push(ev(
            5,
            0,
            EventKind::SoiRescan {
                vessel: VesselId(0),
            },
        ));
        h.push(ev(
            3,
            9,
            EventKind::SoiRescan {
                vessel: VesselId(0),
            },
        ));
        h.push(ev(
            5,
            1,
            EventKind::SoiRescan {
                vessel: VesselId(0),
            },
        ));
        let order: Vec<(u64, u64)> = (0..4)
            .map(|_| h.pop().map(|e| (e.tick, e.seq)).unwrap())
            .collect();
        assert_eq!(order, [(3, 9), (5, 0), (5, 1), (5, 2)]);
    }

    /// §2.6.5's opt-in rule, at the one place the policy lives:
    /// tracked ⇒ Clamping, untracked ⇒ Silent; alarm ⇒ Clamping;
    /// rescan ⇒ Silent always.
    #[test]
    fn class_of_implements_the_opt_in_rule() {
        let mut tracked = BTreeSet::new();
        let soi = EventKind::SoiTransition {
            vessel: VesselId(7),
            body: BodyId::from("m"),
            direction: SoiDirection::Exit,
        };
        assert_eq!(class_of(&soi, &tracked), EventClass::Silent);
        tracked.insert(VesselId(7));
        assert_eq!(class_of(&soi, &tracked), EventClass::Clamping);
        assert_eq!(
            class_of(&EventKind::PlayerAlarm { label: "x".into() }, &tracked),
            EventClass::Clamping
        );
        // ManeuverNodeStart is Clamping unconditionally — not
        // tracked-gated (the player armed the node themselves).
        assert_eq!(
            class_of(
                &EventKind::ManeuverNodeStart {
                    vessel: VesselId(9)
                },
                &tracked
            ),
            EventClass::Clamping
        );
        assert_eq!(
            class_of(
                &EventKind::SoiRescan {
                    vessel: VesselId(7)
                },
                &tracked
            ),
            EventClass::Silent
        );
    }
}
