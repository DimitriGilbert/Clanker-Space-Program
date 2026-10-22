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
//! - **Kinds have M1 producers only.** The §2.6.5 member tables name ten
//!   `Clamping` and six `Silent` members; variants whose subsystems do
//!   not exist yet (`ManeuverNodeStart`, `ContractDeadline`,
//!   `ResearchProjectComplete`, `LifeSupportDepleted`, `ScriptWake`,
//!   `Insolvent`, `ConstructionComplete`, `ColonyStore*` ,
//!   `ReliabilityInspectionDue`, `PartFailure`, `TerminatorCrossing`)
//!   are deliberately absent — they land WITH their
//!   producing subsystems (no speculative filler). `SoiRescan` is the
//!   one kind the plan does not name: it is the Orbits subsystem's own
//!   publish deadline when the §2.6.6 scan finds no crossing inside the
//!   query horizon — the mechanism that keeps "publish your next event
//!   time" honest. Always `Silent`: a re-query must never clamp warp.
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
}

impl StableDiscriminant for EventKind {
    fn discriminant(&self) -> u16 {
        match self {
            Self::SoiTransition { .. } => 0,
            Self::SoiRescan { .. } => 1,
            Self::PlayerAlarm { .. } => 2,
            Self::DebrisDecay { .. } => 3,
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
        EventKind::SoiRescan { .. } | EventKind::DebrisDecay { .. } => EventClass::Silent,
        EventKind::PlayerAlarm { .. } => EventClass::Clamping,
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
