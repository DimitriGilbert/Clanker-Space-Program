#![forbid(unsafe_code)]

//! Headless authoritative simulation (PLAN §2.2).
//!
//! Plain owned data — no ECS, no wgpu, no winit, no rand facade, no
//! `std::time`. The ONLY way state changes is `step_frame` plus stamped
//! `SimCommand`s (§2.2.1): commands enqueue freely, but they take effect
//! only when a tick boundary drains them — and a boundary exists only
//! where the clock actually advances, so a zero-advance call drains
//! nothing. The drain is what stamps the `(tick, source_class, source_id,
//! source_seq)` `CommandKey` that the §7.6 command log is built from.
//!
//! M1-B wires the §2.6.2 advance function to its real shape: each loop
//! iteration drains the command queue, computes
//! `t_next = min(effective_target, event_heap.peek_tick(), tick budget)`,
//! advances the interval subsystems in the §2.6.5 constant order array,
//! sets `clock = t_next`, and fires every event due at `t_next` — firing
//! may insert new events. The physics bubble is EMPTY at M1, so the
//! per-tick integration between `now` and `t_next` has nothing to drive:
//! the interval is a jump (see [`World::advance_interval_subsystems`]).
//!
//! # Partition independence (§2.6.1 — the rule the milestone exists for)
//!
//! A rails [`Vessel`]'s `OrbitState` is **epoch-anchored**: its stored
//! value does not change as the clock advances. Re-stamping
//! `mean_anomaly_epoch_rad += n·Δt` at every interval boundary would make
//! the hashed elements depend on how the interval was partitioned —
//! `fl(fl(M + n·Δt₁) + n·Δt₂)` is not bitwise `fl(M + n·(Δt₁+Δt₂))` — and
//! the §2.6.1 ×1-vs-×10⁵ equivalence test would fail by last-ulp drift.
//! Instead the elements stay stamped at spawn/last-rebase and
//! [`OrbitModel::state_at`] evaluates the closed form at any tick from
//! that fixed epoch. Vessel state mutates ONLY at events (the §2.6.6 SOI
//! re-base) and commands (spawn) — both at absolute ticks, identical in
//! every partition. The bubble mask of the equivalence test is identity
//! at M1: no vessel can be bubble-resident yet ([`World::state_hash_masked`]).
//!
//! # Rails-warp refusal seam (§2.6.2)
//!
//! Rails warp is allowed only when no bubble-resident vessel is under
//! thrust, in unsettled contact, or in atmosphere. At M1 there is no
//! bubble, no throttle, and no atmosphere integration, so
//! [`World::rails_warp_refusal`] has nothing to check — but the predicate
//! exists, is the documented seam future refusal cases slot into, and is
//! called every frame by `csp-app`'s warp wiring where it can be.

use std::collections::{BTreeMap, BTreeSet, BinaryHeap, VecDeque};

use csp_core::{SimError, SimTime, StableDiscriminant, StateHasher};
use csp_data::{BodyId, KeplerElements};
use serde::{Deserialize, Serialize};

/// The M1 orbit layer (PLAN §2.8): patched-conics [`orbit::OrbitModel`]
/// with the universal-variable solver and the §2.6.6 SOI-transition scan.
pub mod orbit;

/// The §2.6.5 event-queue data types: classes, kinds, heap entries, and
/// the constant subsystem order array.
pub mod events;

pub use events::{EventClass, EventKind, SUBSYSTEM_ORDER, ScheduledEvent, Subsystem};
use orbit::{OrbitModel, OrbitState, PatchedConics};

/// The §2.6.2 per-call tick budget — the `bubble_tick_budget_limit` slot
/// in the advance loop. At M1 the bubble is empty so the budget bounds
/// the analytic jump: 64 ticks/s × 1080 sim-seconds (18 sim-minutes) per
/// call, matching §2.6.2's ×10⁵ frame coverage. A `step_frame` whose
/// target lies further ahead returns a [`AdvanceReport::partial`] report
/// and the caller re-issues the same target — the budget bounds one
/// call, never the run.
pub const MAX_TICKS_PER_FRAME: u64 = 69_120;

/// §2.6.5's default event budget — the cap on `events_processed` per
/// `step_frame` call. This value **mirrors**
/// `data/balance.ron: advance_to_max_events_per_frame` (2000 — the
/// tunable source of truth, §8.3's "ALL tuning lives here") for worlds
/// built without a loaded data set — `World::new()`'s empty-system
/// worlds and unit tests. Worlds built from a real data set take the
/// loaded value through [`World::with_config`]; a `csp-bench` test pins
/// this mirror to the file so the two can never drift silently.
pub const DEFAULT_MAX_EVENTS_PER_FRAME: u64 = 2_000;

/// §2.6.5's hard per-batch ceiling: one tick's due-event batch fires
/// atomically in chunks of at most this many events per loop iteration —
/// the tail *defers to the next iteration* (the clock does not advance
/// while a deferred batch drains; see [`World::step_frame`]). Atomicity
/// against the events-per-frame budget is unchanged: the budget is checked
/// between iterations, never mid-batch — this ceiling bounds the atomic
/// unit itself so a degenerate same-tick pile-up can neither run
/// unbounded inside one iteration nor wedge the frame.
///
/// The bound is sized so a normal sim never defers (the §2.6.5 pathological
/// density scenario is ~500 objects — a few hundred same-tick events at
/// most), yet each flush iteration does ≤1024 pop/fire units of work. Note
/// the batch ceiling does NOT guarantee `partial`: a batch in
/// (1024, `event_budget`] defers its tail without tripping the per-frame
/// event budget — `partial` and the warp demotion come only from exceeding
/// the *frame* budget (§2.6.5). The ceiling bounds atomic work per
/// iteration; it never drops or re-ticks events.
pub const MAX_EVENTS_PER_BATCH: u64 = 1_024;

/// §2.13's documented fallback for a `Recoverable` event application: the
/// failed event is rolled back and dropped, the error rides
/// [`AdvanceReport::degraded`], and a bare `SoiRescan` is re-published at
/// `clock + DEGRADED_RESCAN_TICKS` so the Orbits subsystem keeps a live
/// publish deadline (the next scan re-examines the full window, so a
/// bounded retry cadence loses no information — it only re-asks later).
/// 4096 ticks = 64 sim-seconds at 64 Hz: often enough that a transient
/// solver hiccup heals quickly, rare enough that a permanently broken
/// orbit cannot storm the event budget.
pub const DEGRADED_RESCAN_TICKS: u64 = 4_096;

/// §2.2 sim-side vessel identity: a monotonic `u64`, never reused
/// (`next_vessel_id` is serialized per §7.2). Iteration that affects
/// state is ascending-ID — the `BTreeMap` arena's key order IS that
/// order. Distinct from `csp-data`'s authored string IDs (§3.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct VesselId(pub u64);

/// An on-rails vessel — the ONLY vessel form at M1: no bubble residence,
/// no parts, no throttle (and therefore nothing for the §2.6.2
/// rails-refusal predicate to read — see its doc).
///
/// `orbit` is epoch-anchored closed-form state (module docs): unchanged
/// by interval advancement, mutated only by `SpawnVessel` and the §2.6.6
/// SOI re-base at `SoiTransition` events — partition-independent by
/// construction.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Vessel {
    /// Its arena key — kept inline per §2.2's arena-of-structs idiom.
    pub id: VesselId,
    /// Elements + the sim-time they are stamped at.
    pub orbit: OrbitState,
}

/// Source of a `SimCommand` — fixed ordering class for total intra-tick
/// order (PLAN §2.2.1). Ordered: Script < Autopilot < Ui < External.
///
/// Discriminants are append-only and stable: they are what the
/// `command_log` serde stream (§7.6) and the canonical hash (§7.1 rule 7)
/// encode, so renumbering is a deliberate golden-breaking change.
#[repr(u16)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum SourceClass {
    /// Scripting host commands.
    Script = 0,
    /// Autopilot / AI commands.
    Autopilot = 1,
    /// Interactive UI commands.
    Ui = 2,
    /// External interface (MCP, server) commands.
    External = 3,
}

impl StableDiscriminant for SourceClass {
    fn discriminant(&self) -> u16 {
        *self as u16
    }
}

/// Total ordering key for commands within a tick (PLAN §2.2.1), stamped at
/// *dequeue* time inside `step_frame` — never at production time, so an
/// early queue entry can never smuggle an early tick into the log.
///
/// `source_seq` is a per-`(source_class, source_id)` counter; within one
/// source the log order is production order, which is what lets replay
/// restore `source_seq` counters and reproduce every recorded stamp.
///
/// Ordering is the declaration order: `tick`, then `source_class`, then
/// `source_id`, then `source_seq` — the drain order `step_frame` produces.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CommandKey {
    /// The drain-boundary tick at which the command was applied.
    pub tick: u64,
    /// The command's ordering class (§2.2.1).
    pub source_class: SourceClass,
    /// Which producer of that class issued the command.
    pub source_id: u32,
    /// Per-source production-order counter, stamped at dequeue.
    pub source_seq: u64,
}

/// A mutation request — the only path into state (PLAN §2.2).
///
/// Variant order is append-only and stable: the `command_log` serde stream
/// encodes the variant index, so reordering variants is a deliberate
/// golden-breaking change. `SetAlarm` carries a `String`, so the enum is
/// no longer `Copy` — replay clones entries out of the log.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum SimCommand {
    /// Store a new warp ladder rung (§2.6.3). Stored state, in the hash;
    /// interpreting it as a multiplier is presentation's job.
    SetWarp(u32),
    /// Ask the sim to advance to `tick` — a *logged* advance target
    /// (§2.6.3: warp is issued as commands, so the §7.6 log is
    /// self-contained). Effective per-call target is `max(step target,
    /// every AdvanceTo arg drained at that boundary)`.
    AdvanceTo(u64),
    /// Spawn an on-rails vessel. The elements' `mean_anomaly_epoch_rad`
    /// refers to the **drain tick** (the spawn instant — §2.8's
    /// elements+epoch convention). The apply validates that `parent`
    /// exists in the orbit model and that the elements solve at the
    /// spawn tick, allocates the next monotonic [`VesselId`], and
    /// schedules the vessel's first §2.6.6 SOI query. A failure is a
    /// command error: rejected onto [`AdvanceReport::rejected`]
    /// (§2.2.1's "rejected on the originating channel"), its effects
    /// rolled back — the boundary continues.
    SpawnVessel {
        /// The frame the elements are relative to.
        parent: BodyId,
        /// Keplerian elements stamped at the drain tick.
        elements: KeplerElements,
    },
    /// §2.6.5's per-object opt-in ("track this vessel"): promote the
    /// vessel's events `Silent → Clamping` — including its already-
    /// queued `SoiTransition` — or demote back on `false`. Unknown ids
    /// are a command error (Recoverable), never a silent no-op.
    SetVesselTracked {
        /// The vessel to (un)track.
        vessel: VesselId,
        /// `true` = its events clamp warp; `false` = Silent.
        tracked: bool,
    },
    /// Schedule a `PlayerAlarm` (§2.6.5 Clamping member) at `tick` — a
    /// command so the alarm is §7.6-logged and replayable. A `tick` at
    /// or before the drain boundary clamps to the next tick: an alarm
    /// may not rewrite the past, and scheduled events must always sit
    /// strictly ahead of the clock.
    SetAlarm {
        /// The tick the alarm fires at.
        tick: u64,
        /// Free-form label carried to `AdvanceReport::fired_events`.
        label: String,
    },
}

/// What `step_frame` did (§2.6.2): where the clock landed, what the
/// call's drains applied, how many events it fired — and which — plus
/// whether the call ran out of budget before reaching the effective
/// target.
#[derive(Clone, Debug, PartialEq)]
pub struct AdvanceReport {
    /// The tick the clock reached after this call.
    pub reached_tick: u64,
    /// Commands this call's boundary drains stamped and applied — 0 on a
    /// zero-advance call, which is not a tick boundary at all (§2.2.1).
    pub commands_applied: u64,
    /// Events this call fired (§2.6.2's `events_processed`), bounded per
    /// call by `World`'s event budget (§2.6.5's 2000-events-per-frame
    /// back-pressure, batch-atomic — see [`World::fire_due_events`]).
    pub events_processed: u64,
    /// Every event this call fired, in `(tick, seq)` order — the
    /// [`EventClass::Clamping`] entries are what `csp-app`'s
    /// `WarpController` demotes on (§2.6.5); the whole list is reported
    /// so a UI/log layer can surface notifications without re-querying
    /// sim state. A degraded event still appears here (it *was* processed
    /// at that tick — its failed application is what `degraded` records).
    pub fired_events: Vec<ScheduledEvent>,
    /// §2.2.1's "rejected on the originating channel": commands whose
    /// apply failed non-`Fatal` (bad spawn parent, unknown vessel in
    /// `SetVesselTracked`, unsolvable elements). The channel IS this
    /// report — the rejection is reported with the `CommandKey` the
    /// command *would* have been stamped with. A rejected command applies
    /// nothing: its partial effects, `source_seq` consumption, and log
    /// entry are all rolled back, so post-frame state is identical to the
    /// command never having existed (the boundary still consumed it — a
    /// rejected command is not re-queued).
    pub rejected: Vec<(CommandKey, SimError)>,
    /// §2.13's Recoverable channel for the event/interval side: an event
    /// whose firing failed non-`Fatally` (e.g. a degenerate SOI event that
    /// slipped the post-rebase suppression, a scan that exhausted its
    /// sample budget) is rolled back, dropped, reported here — and, for
    /// vessel-carrying kinds, a fresh `SoiRescan` is re-published
    /// [`DEGRADED_RESCAN_TICKS`] out so the subsystem keeps a publish
    /// deadline. Interval-advance solvability failures also report here
    /// (the vessel keeps its last valid state — epoch-anchored, nothing
    /// was written). `step_frame` only ever returns `Err` for `Fatal`.
    pub degraded: Vec<SimError>,
    /// `true` when [`MAX_TICKS_PER_FRAME`] or the event budget split the
    /// advance and the caller must re-issue the same target —
    /// back-pressure, never a stall: a partial still advanced the clock,
    /// fired events, or applied commands (§2.6.5).
    pub partial: bool,
}

/// The world's §7.2 serialized set — everything `World::restore` needs to
/// resume a world that produces identical stamps, hashes, and state.
///
/// Deliberately excludes the in-flight `pending` queue and the
/// `command_log`: the log is an output artifact (§7.6), and snapshot
/// boundaries are defined at *drained* boundaries — an enqueued command is
/// not yet world state; it becomes state only at the drain that stamps it
/// (which is also why the log can reproduce the run from this set alone).
/// [`World::snapshot`] refuses while the queue is non-empty
/// ([`World::check_serializable`], §7.4), so the exclusion can never
/// silently drop an undrained command.
///
/// The orbit model is excluded too: it is a pure function of the data set
/// (§7.3's re-derived-on-load class), handed in at construction.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorldSnapshot {
    /// `clock.ticks()` at snapshot time.
    pub clock_ticks: u64,
    /// The stored warp ladder rung.
    pub warp_level: u32,
    /// Per-source dequeue counters — restoring them is what lets a resumed
    /// world keep stamping the recorded `source_seq`s.
    pub source_seqs: BTreeMap<(SourceClass, u32), u64>,
    /// The §2.6.5 events-per-frame budget — construction config,
    /// serialized so a restored world enforces the same cap.
    pub event_budget: u64,
    /// §7.2 next-ID counter for the vessel arena (monotonic, never reused).
    pub next_vessel_id: u64,
    /// The on-rails vessel arena. `BTreeMap` key order is ascending-ID;
    /// at M1 there is no vessel removal, so no free list or slot
    /// generations exist to serialize (§7.1 rule 5 — they would be
    /// allocator state anyway).
    pub vessels: BTreeMap<VesselId, Vessel>,
    /// §2.6.5's Silent→Clamping per-object opt-in set.
    pub tracked_vessels: BTreeSet<VesselId>,
    /// §7.2 "including `event_seq` and `next_event_seq`" — the monotonic
    /// insertion counter that makes heap ordering total under ties.
    pub next_event_seq: u64,
    /// The event heap's contents in `(tick, seq)` order — the canonical
    /// enumeration; restore re-heapifies deterministically.
    pub event_heap: Vec<ScheduledEvent>,
}

/// The §2.13 one-iteration shadow: everything one loop iteration of
/// `step_frame` can mutate, captured before the iteration runs so a
/// `Fatal`/`Recoverable` mid-iteration restores the pre-boundary state
/// *completely* — `state_hash()` after a rollback equals the
/// pre-boundary hash byte-for-byte.
///
/// The queue needs no deep copy: the drain records what it popped, and a
/// restore pushes each command back `push_front`-first in reverse drain
/// order, which rebuilds each source FIFO's production order exactly.
/// `seq_restore` plays the same trick for the counters.
///
/// The M1 fields are **lazy captures**: most iterations mutate neither
/// the vessel arena nor the event heap (an event-free interval touches
/// only the clock), so each is cloned only when the iteration is about
/// to mutate it — `Option` = "captured"; `None` on restore means "this
/// field was never touched this iteration". A capture always precedes
/// the first write to its field inside an iteration.
struct TickShadow {
    /// `clock` at the boundary.
    clock: SimTime,
    /// `warp_level` at the boundary.
    warp_level: u32,
    /// `command_log.len()` at the boundary — restore truncates to it.
    log_len: usize,
    /// Commands the drain popped, in drain order (per source, production
    /// order). Restored back into `pending` on rollback.
    drained: Vec<((SourceClass, u32), SimCommand)>,
    /// Each source the drain touched, with its pre-increment counter —
    /// `None` = the counter did not exist before this tick.
    seq_restore: Vec<((SourceClass, u32), Option<u64>)>,
    /// `vessels` — captured before the iteration's first arena write.
    vessels: Option<BTreeMap<VesselId, Vessel>>,
    /// `next_vessel_id` — captured with `vessels`.
    next_vessel_id: Option<u64>,
    /// `tracked_vessels` — captured before the first opt-in mutation.
    tracked_vessels: Option<BTreeSet<VesselId>>,
    /// `event_heap` — captured before the first push/class rewrite.
    event_heap: Option<BinaryHeap<ScheduledEvent>>,
    /// `next_event_seq` — captured with `event_heap`.
    next_event_seq: Option<u64>,
}

impl TickShadow {
    /// Snapshot the cheap pre-iteration fields; the expensive fields are
    /// captured lazily by the `World::shadow_*` helpers.
    fn capture(world: &World) -> Self {
        Self {
            clock: world.clock,
            warp_level: world.warp_level,
            log_len: world.command_log.len(),
            drained: Vec::new(),
            seq_restore: Vec::new(),
            vessels: None,
            next_vessel_id: None,
            tracked_vessels: None,
            event_heap: None,
            next_event_seq: None,
        }
    }

    /// Roll the world back to the captured pre-iteration state — the
    /// whole state, so `state_hash()` is the pre-boundary hash again
    /// (§2.13). `None` fields were never mutated.
    fn restore(self, world: &mut World) {
        world.clock = self.clock;
        world.warp_level = self.warp_level;
        world.command_log.truncate(self.log_len);
        // Reverse drain order + push_front rebuilds each FIFO's
        // production order exactly.
        for (src, command) in self.drained.into_iter().rev() {
            world.pending.entry(src).or_default().push_front(command);
        }
        for (src, seq) in self.seq_restore {
            match seq {
                Some(v) => {
                    world.source_seqs.insert(src, v);
                }
                None => {
                    world.source_seqs.remove(&src);
                }
            }
        }
        if let Some(v) = self.vessels {
            world.vessels = v;
        }
        if let Some(v) = self.next_vessel_id {
            world.next_vessel_id = v;
        }
        if let Some(v) = self.tracked_vessels {
            world.tracked_vessels = v;
        }
        if let Some(v) = self.event_heap {
            world.event_heap = v;
        }
        if let Some(v) = self.next_event_seq {
            world.next_event_seq = v;
        }
    }
}

/// The authoritative world. Owns the integer clock (§2.3), the stored warp
/// rung (§2.6.3), the command queue + log (§2.2.1, §7.6), the per-source
/// stamp counters, the on-rails vessel arena + opt-in set, and the §2.6.5
/// event heap + `event_seq` counter.
///
/// Mutation surface is exactly `enqueue` + `step_frame`; everything else is
/// observation or serialization.
pub struct World {
    /// The integer sim clock — §2.3 says it lives here and nowhere else.
    clock: SimTime,
    /// Stored warp ladder rung. In the hash, never interpreted: the
    /// multiplier ladder is `csp-app`'s warp controller's business.
    warp_level: u32,
    /// Enqueued but undrained commands: per-source FIFOs under
    /// `(source_class, source_id)` keys. The map's key order IS the
    /// §2.2.1 same-tick drain order, each `VecDeque` preserves one
    /// source's production order for `source_seq` stamping, and the
    /// per-source split is what lets a mid-tick rollback push drained
    /// commands back where they came from. Sources drained to empty FIFOs
    /// are dropped, so `check_serializable` can read the queue's
    /// emptiness straight off the map.
    pending: BTreeMap<(SourceClass, u32), VecDeque<SimCommand>>,
    /// Every applied command in `CommandKey` order — the §7.6 artifact.
    /// Appended only by the drain, so the vec itself is the sorted log.
    command_log: Vec<(CommandKey, SimCommand)>,
    /// Per-`(source_class, source_id)` dequeue counters. Part of the
    /// hashed state AND the snapshot — a counter reset would silently
    /// change every future stamp.
    source_seqs: BTreeMap<(SourceClass, u32), u64>,
    /// The §2.8 orbit model — a pure function of the loaded `BodyDef`s,
    /// so construction config, never serialized or hashed (§7.3).
    orbits: PatchedConics,
    /// §2.6.5's events-per-frame budget — construction config (loaded
    /// from `balance.ron` in real worlds; [`DEFAULT_MAX_EVENTS_PER_FRAME`]
    /// otherwise). Serialized and hashed: it changes event processing,
    /// so an undocumented divergence here would red the wrong tests.
    event_budget: u64,
    /// §2.2 next-ID counter for the vessel arena — monotonic, never
    /// reused, serialized and hashed.
    next_vessel_id: u64,
    /// The on-rails vessel arena. `BTreeMap` order is ascending-ID order,
    /// which §2.2 requires of every state-affecting iteration (the M1
    /// slot/generation arena machinery lands with removal; with none,
    /// the map IS the arena).
    vessels: BTreeMap<VesselId, Vessel>,
    /// §2.6.5's per-object opt-in — members' `SoiTransition`s schedule and
    /// re-class as `Clamping`. Hashed state.
    tracked_vessels: BTreeSet<VesselId>,
    /// §2.6.5's `BinaryHeap<ScheduledEvent>` keyed by `(tick, event_seq)`
    /// — the `Ord` on `ScheduledEvent` makes the heap a MIN-heap on that
    /// key. Invariant: every entry's `tick` is strictly ahead of the
    /// clock ([`World::schedule_event`] enforces it), so each loop
    /// iteration advances at least one tick and each tick value hosts at
    /// most one boundary drain.
    event_heap: BinaryHeap<ScheduledEvent>,
    /// §7.2's `next_event_seq` — assigned at insertion; because insertion
    /// order follows the §2.6.5 subsystem order array, the counter is
    /// hashed sim state.
    next_event_seq: u64,
}

impl World {
    /// A pristine world at epoch 0, warp ×1, no commands seen — with an
    /// EMPTY system model: `SpawnVessel` fails `Recoverable` on any
    /// parent (there are no bodies), which is exactly what the
    /// fixture-free M0-shaped runs and command-path tests want.
    pub fn new() -> Self {
        Self::with_system(PatchedConics::new(BTreeMap::new()))
    }

    /// A world over a real orbit model, at the
    /// [`DEFAULT_MAX_EVENTS_PER_FRAME`] event budget.
    pub fn with_system(orbits: PatchedConics) -> Self {
        Self::with_config(orbits, DEFAULT_MAX_EVENTS_PER_FRAME)
    }

    /// A world over a real orbit model with an explicit §2.6.5 event
    /// budget — the value `data/balance.ron` loads in production, or a
    /// deliberately small cap in back-pressure tests. `0` clamps to `1`:
    /// a zero budget could fire one batch per call but never count it,
    /// which would loop forever.
    pub fn with_config(orbits: PatchedConics, event_budget: u64) -> Self {
        Self {
            clock: SimTime::ZERO,
            warp_level: 1,
            pending: BTreeMap::new(),
            command_log: Vec::new(),
            source_seqs: BTreeMap::new(),
            orbits,
            event_budget: event_budget.max(1),
            next_vessel_id: 0,
            vessels: BTreeMap::new(),
            tracked_vessels: BTreeSet::new(),
            event_heap: BinaryHeap::new(),
            next_event_seq: 0,
        }
    }

    /// Rebuild a world from its §7.2 serialized set plus the orbit model
    /// the set was produced against (data-derived, §7.3 — the caller
    /// rebuilds it from the same `BodyDef`s). The queue and log restart
    /// empty — the log is replayed *into* the world by the harness
    /// (§7.6), it is not part of the snapshot (see [`WorldSnapshot`]).
    /// The heap rebuilds by heapifying its canonical `(tick, seq)`
    /// enumeration — deterministic by construction.
    ///
    /// Fallible: the serialized set is validated against the world's own
    /// invariants before anything is built — a crafted or corrupt
    /// snapshot is refused with `Fatal` (the §2.13 class for violated
    /// invariants), never half-loaded into a world that would diverge or
    /// panic later:
    ///
    /// - `event_budget ≥ 1` (the construction clamp [`Self::with_config`]
    ///   applies is not repeated here — a 0 in the snapshot is invalid
    ///   input, not a value to silently repair);
    /// - every event `tick ≥ clock_ticks` — `>` for clean worlds, `==`
    ///   for a snapshot taken mid-flush of a deferred over-ceiling batch
    ///   (the only state in which at-clock events legitimately persist);
    /// - `(tick, seq)` is unique per event and `seq < next_event_seq`;
    /// - the stored `class` equals the derived one
    ///   (`events::class_of(kind, tracked_vessels)` — the derived field
    ///   must agree with what was serialized or the restored heap would
    ///   classify differently than the world that wrote it);
    /// - every vessel a `SoiTransition`/`SoiRescan` names exists in the
    ///   arena (so the `"unknown vessel"` Fatal inside firing is
    ///   unreachable for restored worlds);
    /// - `vessel.id == key` per arena entry and `next_vessel_id` exceeds
    ///   every live id (monotonic, never reused);
    /// - `tracked_vessels ⊆ vessels`.
    ///
    /// `source_seqs` entries are free-form per-source dequeue counters —
    /// no external constraint pins them (the command log is not part of
    /// the snapshot), so any `u64` is a coherent value.
    pub fn restore(orbits: PatchedConics, snapshot: WorldSnapshot) -> Result<Self, SimError> {
        if snapshot.event_budget == 0 {
            return Err(SimError::Fatal("snapshot: zero event budget"));
        }
        if snapshot.next_vessel_id < snapshot.vessels.keys().next_back().map_or(0, |id| id.0 + 1) {
            return Err(SimError::Fatal(
                "snapshot: next_vessel_id not ahead of live ids",
            ));
        }
        for (id, vessel) in &snapshot.vessels {
            if vessel.id != *id {
                return Err(SimError::Fatal("snapshot: vessel id != arena key"));
            }
        }
        for id in &snapshot.tracked_vessels {
            if !snapshot.vessels.contains_key(id) {
                return Err(SimError::Fatal("snapshot: tracked vessel not in arena"));
            }
        }
        let mut seen = std::collections::BTreeSet::new();
        for e in &snapshot.event_heap {
            if e.tick < snapshot.clock_ticks {
                return Err(SimError::Fatal("snapshot: event behind the clock"));
            }
            if e.seq >= snapshot.next_event_seq {
                return Err(SimError::Fatal(
                    "snapshot: event seq not below next_event_seq",
                ));
            }
            if !seen.insert((e.tick, e.seq)) {
                return Err(SimError::Fatal("snapshot: duplicate (tick, seq)"));
            }
            if e.class != events::class_of(&e.kind, &snapshot.tracked_vessels) {
                return Err(SimError::Fatal("snapshot: event class incoherent"));
            }
            let vessel = match &e.kind {
                EventKind::SoiTransition { vessel, .. } | EventKind::SoiRescan { vessel } => {
                    Some(*vessel)
                }
                EventKind::PlayerAlarm { .. } => None,
            };
            if let Some(v) = vessel
                && !snapshot.vessels.contains_key(&v)
            {
                return Err(SimError::Fatal("snapshot: event names unknown vessel"));
            }
        }
        Ok(Self {
            clock: SimTime(snapshot.clock_ticks),
            warp_level: snapshot.warp_level,
            pending: BTreeMap::new(),
            command_log: Vec::new(),
            source_seqs: snapshot.source_seqs,
            orbits,
            event_budget: snapshot.event_budget,
            next_vessel_id: snapshot.next_vessel_id,
            vessels: snapshot.vessels,
            tracked_vessels: snapshot.tracked_vessels,
            event_heap: BinaryHeap::from(snapshot.event_heap),
            next_event_seq: snapshot.next_event_seq,
        })
    }

    /// The §7.4 serialization-boundary refusal class, applied to the one
    /// M1 case that can violate it: undrained commands in flight. An
    /// enqueued command is not yet world state (its tick/seq stamps are
    /// assigned by the drain), so a snapshot taken while the queue is
    /// non-empty could never reproduce the run — the boundary refuses
    /// `SaveBlocking` rather than silently dropping the in-flight work.
    pub fn check_serializable(&self) -> Result<(), SimError> {
        if self.pending.values().any(|fifo| !fifo.is_empty()) {
            return Err(SimError::SaveBlocking(
                "commands pending at serialization boundary",
            ));
        }
        Ok(())
    }

    /// The §7.2 serialized set (see [`WorldSnapshot`]). Fallible: it first
    /// runs [`Self::check_serializable`] — the queue-empty invariant makes
    /// the boundary a drained boundary, which is what lets the §7.6 log
    /// reproduce the run from this set alone.
    pub fn snapshot(&self) -> Result<WorldSnapshot, SimError> {
        self.check_serializable()?;
        let mut event_heap: Vec<ScheduledEvent> = self.event_heap.iter().cloned().collect();
        event_heap.sort_by_key(|e| (e.tick, e.seq));
        Ok(WorldSnapshot {
            clock_ticks: self.clock.ticks(),
            warp_level: self.warp_level,
            source_seqs: self.source_seqs.clone(),
            event_budget: self.event_budget,
            next_vessel_id: self.next_vessel_id,
            vessels: self.vessels.clone(),
            tracked_vessels: self.tracked_vessels.clone(),
            next_event_seq: self.next_event_seq,
            event_heap,
        })
    }

    /// The integer sim clock (§2.3).
    pub fn clock(&self) -> SimTime {
        self.clock
    }

    /// The stored warp ladder rung (§2.6.3). Sim state, in the hash.
    pub fn warp_level(&self) -> u32 {
        self.warp_level
    }

    /// Every applied command in `CommandKey` order — the §7.6 log.
    pub fn command_log(&self) -> &[(CommandKey, SimCommand)] {
        &self.command_log
    }

    /// Queue a `SimCommand` for the next boundary drain (§2.2.1). Touches
    /// only the in-flight queue — the tick/seq stamps are assigned by the
    /// drain inside `step_frame`, never here, so enqueuing is free of
    /// ordering hazards.
    pub fn enqueue(&mut self, source_class: SourceClass, source_id: u32, command: SimCommand) {
        self.pending
            .entry((source_class, source_id))
            .or_default()
            .push_back(command);
    }

    /// Read-only access to the §2.8 orbit model — observation, never
    /// mutation (the model is construction config anyway, §7.3). The
    /// presentation layer's map view (M1-D) evaluates `state_at` through
    /// this; render code never touches it (§2.1: csp-view bridges).
    pub fn orbits(&self) -> &PatchedConics {
        &self.orbits
    }

    /// Read-only access to the on-rails vessel arena — `BTreeMap` order is
    /// ascending-ID (§2.2). Observation only: the mutation surface stays
    /// `enqueue` + `step_frame`.
    pub fn vessels(&self) -> &BTreeMap<VesselId, Vessel> {
        &self.vessels
    }

    /// §2.6.2's rails-warp refusal predicate — the SEAM, wired now
    /// (`csp-app` calls it every frame). Returns the refusal reason when
    /// rails warp must be refused, else `None`.
    ///
    /// The spec's refusal set — a bubble-resident vessel under thrust, an
    /// unsettled contact, a vessel in atmosphere — has no M1 reading:
    /// there is no bubble (every vessel lives on rails), no throttle
    /// field (§2.6.2 forces throttle to zero on bubble exit, and nothing
    /// yet produces a nonzero one), and no atmosphere integration. So
    /// the predicate is honestly vacuous today: the branches future
    /// milestones fill are named here rather than smuggled in later.
    pub fn rails_warp_refusal(&self) -> Option<&'static str> {
        None
    }

    /// Lazy-shadow capture for the vessel arena + next-ID — call before
    /// the first write to either inside an iteration.
    fn shadow_vessels(&mut self, shadow: &mut TickShadow) {
        if shadow.vessels.is_none() {
            shadow.vessels = Some(self.vessels.clone());
            shadow.next_vessel_id = Some(self.next_vessel_id);
        }
    }

    /// Lazy-shadow capture for the tracked-vessel set.
    fn shadow_tracked(&mut self, shadow: &mut TickShadow) {
        if shadow.tracked_vessels.is_none() {
            shadow.tracked_vessels = Some(self.tracked_vessels.clone());
        }
    }

    /// Lazy-shadow capture for the event heap + seq counter.
    fn shadow_events(&mut self, shadow: &mut TickShadow) {
        if shadow.event_heap.is_none() {
            shadow.event_heap = Some(self.event_heap.clone());
            shadow.next_event_seq = Some(self.next_event_seq);
        }
    }

    /// §2.6.5's insertion path: assign `seq` from the serialized
    /// monotonic counter (`event_seq` is assigned at insertion and
    /// therefore depends on the subsystem order array), pick the class
    /// from the event kind + the tracked opt-in, and push.
    ///
    /// Invariant enforcement: `tick` must be strictly ahead of the
    /// clock — an event at-or-behind `now` would produce a zero-advance
    /// loop iteration and a repeated drain at one tick value. Internal
    /// producers satisfy it by construction; a violation is a `Fatal`
    /// invariant bug, not bad input.
    fn schedule_event(
        &mut self,
        kind: EventKind,
        tick: u64,
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        if tick <= self.clock.ticks() {
            return Err(SimError::Fatal("scheduled event not ahead of the clock"));
        }
        self.shadow_events(shadow);
        let seq = self.next_event_seq;
        self.next_event_seq = seq
            .checked_add(1)
            .ok_or(SimError::Fatal("event_seq counter overflow"))?;
        self.event_heap.push(ScheduledEvent {
            tick,
            seq,
            class: events::class_of(&kind, &self.tracked_vessels),
            kind,
        });
        Ok(())
    }

    /// The Orbits subsystem's publish step (§2.6.5's "publish its next
    /// event time"): run the §2.6.6 scan for `vessel` over
    /// `(now, horizon]` and schedule what it finds — a `SoiTransition` at
    /// the crossing tick, or a `SoiRescan` at the horizon when no
    /// crossing exists inside it (the rescan IS the subsystem's next
    /// publish deadline, so no crossing can ever slip past unqueried).
    ///
    /// The horizon comes from [`PatchedConics::scan_horizon`]: a few
    /// periods of the tightest orbit in play, clamped below the
    /// [`orbit::SOI_SCAN_MAX_SAMPLES`] span — the physical scale is the
    /// rule, the sample cap only bounds it. A vessel with zero candidates
    /// (e.g. orbiting a body with no SOI boundary at all) can never cross
    /// → nothing scheduled.
    ///
    /// `suppress` carries the just-fired transition `(body, direction)`
    /// when this publish is the post-rebase rescan: the ≤1-tick Brent
    /// bound can leave the rebased vessel marginally on the *wrong* side
    /// of the boundary it just crossed, so a first-stride re-detection of
    /// the same direction on the same body is the degenerate
    /// `Enter{own-parent}`/`Exit{non-parent}` artifact — dropped by the
    /// scan rather than scheduled into a guaranteed-`Recoverable` event.
    fn schedule_next_soi(
        &mut self,
        vessel: VesselId,
        now: SimTime,
        shadow: &mut TickShadow,
        suppress: Option<(BodyId, orbit::SoiDirection)>,
    ) -> Result<(), SimError> {
        let orbit = self
            .vessels
            .get(&vessel)
            .ok_or(SimError::Fatal("soi schedule: unknown vessel"))?
            .orbit
            .clone();
        let Some(span) = self.orbits.scan_horizon(&orbit)? else {
            return Ok(());
        };
        let horizon = SimTime(now.ticks().saturating_add(span));
        match self.orbits.next_soi_event_within(
            &orbit,
            now,
            horizon,
            orbit::SOI_SCAN_MAX_SAMPLES,
            suppress,
        )? {
            (Some(ev), _) => self.schedule_event(
                EventKind::SoiTransition {
                    vessel,
                    body: ev.body,
                    direction: ev.direction,
                },
                ev.tick,
                shadow,
            ),
            (None, _) => {
                self.schedule_event(EventKind::SoiRescan { vessel }, horizon.ticks(), shadow)
            }
        }
    }

    /// One command's effect (§2.2: commands are the only mutation path).
    /// Runs after the drain's pop pass, so the whole world is borrowable —
    /// applies may touch the vessel arena, the opt-in set, and the event
    /// heap; everything they touch is shadow-captured first.
    fn apply_command(
        &mut self,
        command: &SimCommand,
        effective_target: &mut u64,
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        match command {
            SimCommand::SetWarp(level) => self.warp_level = *level,
            SimCommand::AdvanceTo(t) => *effective_target = (*effective_target).max(*t),
            SimCommand::SpawnVessel { parent, elements } => {
                let orbit = OrbitState {
                    parent: parent.clone(),
                    elements: elements.clone(),
                    // The spawn instant: elements' M0 refers to this tick.
                    epoch: self.clock,
                };
                // Validation before allocation: the parent must exist and
                // the elements must solve at the spawn tick. A failure is
                // a command error — the drain rolls this command's effects
                // back and reports it on `AdvanceReport::rejected` (§2.2.1:
                // "rejected on the originating channel"), then continues
                // the boundary.
                self.orbits.body_orbit(&orbit.parent)?;
                self.orbits.state_at(&orbit, self.clock)?;
                self.shadow_vessels(shadow);
                let id = VesselId(self.next_vessel_id);
                self.next_vessel_id = self
                    .next_vessel_id
                    .checked_add(1)
                    .ok_or(SimError::Fatal("vessel id counter overflow"))?;
                self.vessels.insert(id, Vessel { id, orbit });
                // §2.6.5: publish the new vessel's next event time now —
                // the heap must already hold it before `t_next` is next
                // computed. No transition just fired → nothing suppressed.
                self.schedule_next_soi(id, self.clock, shadow, None)?;
            }
            SimCommand::SetVesselTracked { vessel, tracked } => {
                if !self.vessels.contains_key(vessel) {
                    return Err(SimError::Recoverable("track: unknown vessel"));
                }
                self.shadow_tracked(shadow);
                if *tracked {
                    self.tracked_vessels.insert(*vessel);
                } else {
                    self.tracked_vessels.remove(vessel);
                }
                // The opt-in promotes/demotes already-queued events too —
                // class lives on the entry, so rewrite the vessel's
                // pending SoiTransitions. `class` is not part of the heap
                // key, so rewriting it via drain/re-heapify keeps the
                // `(tick, seq)` order intact.
                self.shadow_events(shadow);
                let heap = std::mem::take(&mut self.event_heap);
                self.event_heap = heap
                    .into_iter()
                    .map(|mut e| {
                        if let EventKind::SoiTransition { vessel: v, .. } = &e.kind
                            && v == vessel
                        {
                            e.class = events::class_of(&e.kind, &self.tracked_vessels);
                        }
                        e
                    })
                    .collect();
            }
            SimCommand::SetAlarm { tick, label } => {
                // Alarms may not rewrite the past: at-or-past ticks clamp
                // to the next tick (an event must sit strictly ahead of
                // the clock — `schedule_event` enforces it).
                let at = (*tick).max(self.clock.ticks().saturating_add(1));
                self.schedule_event(
                    EventKind::PlayerAlarm {
                        label: label.clone(),
                    },
                    at,
                    shadow,
                )?;
            }
        }
        Ok(())
    }

    /// §2.2.1's boundary drain at the current tick: pop every pending
    /// command in `(source_class, source_id)` key order — each source's
    /// FIFO in production order — stamp each with its source's next
    /// `source_seq`, apply it, and append it to the `command_log`.
    /// `SetWarp` stores the rung; each `AdvanceTo(t)` raises this call's
    /// effective target to `max(effective_target, t)`.
    ///
    /// Two phases because the pop pass borrows `self.pending`: pops record
    /// into `shadow.drained` (so a `Fatal` — a `source_seq` overflow,
    /// §2.13's abort class — restores the queue's production order
    /// exactly); stamping, applying, and logging then run per command
    /// against the whole world. Stamps moved into phase 2 so a *rejected*
    /// command can roll its `source_seq` consumption back — a rejected
    /// command spends nothing and leaves no log entry: post-drain state
    /// is identical to the command never having existed.
    ///
    /// Per-command rollback: each apply runs under its own [`TickShadow`]
    /// — a non-`Fatal` failure restores only that command's effects and
    /// reports `(would-be key, error)` on `rejected` (§2.2.1's "rejected
    /// on the originating channel"), then the boundary continues. `Fatal`
    /// still aborts the iteration whole via the caller's shadow.
    fn drain_boundary(
        &mut self,
        effective_target: &mut u64,
        shadow: &mut TickShadow,
        commands_applied: &mut u64,
        rejected: &mut Vec<(CommandKey, SimError)>,
    ) -> Result<(), SimError> {
        let tick = self.clock.ticks();
        // Phase 1 — pops only. BTreeMap iteration IS the §2.2.1
        // (class, source_id) order; each source's FIFO keeps production
        // order, so `staged` is contiguous per source.
        let mut staged: Vec<((SourceClass, u32), SimCommand)> = Vec::new();
        for (&src, fifo) in &mut self.pending {
            while let Some(command) = fifo.pop_front() {
                shadow.drained.push((src, command.clone()));
                staged.push((src, command));
            }
        }
        // Drained FIFOs go empty — drop them so the queue's emptiness is
        // literally the map's, the §7.4 invariant `check_serializable`
        // reads.
        self.pending.retain(|_, fifo| !fifo.is_empty());
        // Phase 2 — stamp, apply, log; in staged order. `last_src` flips
        // exactly when a new source's group begins.
        let mut last_src: Option<(SourceClass, u32)> = None;
        for (src, command) in staged {
            // First touch of this source this iteration: record the
            // pre-increment counter for a whole-iteration (Fatal) restore.
            if last_src != Some(src) {
                shadow
                    .seq_restore
                    .push((src, self.source_seqs.get(&src).copied()));
                last_src = Some(src);
            }
            // Per-command shadow: on a non-Fatal apply failure this
            // restores only what this command touched — plus, via
            // `seq_restore`, the seq it was stamped with.
            let mut cmd_shadow = TickShadow::capture(self);
            cmd_shadow
                .seq_restore
                .push((src, self.source_seqs.get(&src).copied()));
            let seq = self.source_seqs.entry(src).or_insert(0);
            let key = CommandKey {
                tick,
                source_class: src.0,
                source_id: src.1,
                source_seq: *seq,
            };
            *seq = seq
                .checked_add(1)
                .ok_or(SimError::Fatal("source_seq counter overflow"))?;
            match self.apply_command(&command, effective_target, &mut cmd_shadow) {
                Ok(()) => {
                    self.command_log.push((key, command));
                    *commands_applied += 1;
                }
                // §2.13: Fatal aborts the iteration — the caller's shadow
                // rolls the whole boundary back, queue included.
                Err(e @ SimError::Fatal(_)) => return Err(e),
                Err(e) => {
                    cmd_shadow.restore(self);
                    rejected.push((key, e));
                }
            }
        }
        Ok(())
    }

    /// §2.6.1's interval advance over `(now, t_next]` in §2.6.5's
    /// CONSTANT subsystem order — the array is iterated verbatim;
    /// reordering it is a replay- and save-breaking change
    /// (`HASH_SCHEMA_VERSION` bump) because `event_seq` insertion order
    /// is bound to it. Slots whose subsystems have no M1 state are
    /// honest no-ops — the array exists now so the order is real before
    /// any subsystem fills its slot.
    fn advance_interval_subsystems(
        &mut self,
        now: SimTime,
        t_next: SimTime,
        _shadow: &mut TickShadow,
        degraded: &mut Vec<SimError>,
    ) -> Result<(), SimError> {
        debug_assert!(t_next >= now, "interval must not be reversed");
        for subsystem in SUBSYSTEM_ORDER {
            if subsystem == Subsystem::Orbits {
                self.advance_orbits(t_next, degraded)?;
            }
            // Vessels..Alarms: the remaining §2.6.5 slots have no
            // interval state at M1 — their advance is a no-op by
            // definition until the subsystem lands.
        }
        let _ = now;
        Ok(())
    }

    /// The `Orbits` slot (§2.6.5 index 0). A rails `OrbitState` is
    /// epoch-anchored: its hashed value does NOT change over an
    /// interval — that is what makes advancement partition-independent
    /// (§2.6.1; see the module doc for the float-associativity argument).
    /// The interval advance therefore *mutates nothing*: it re-solves
    /// each vessel's conic at `t_next` to prove the closed form still
    /// evaluates (§2.8's solvability contract — a failed solve is
    /// `Recoverable` — reported on `degraded`, and the orbit keeps its
    /// last valid state because nothing was written). Mutation belongs
    /// to events (the SOI re-base) and commands (spawn), never to the
    /// interval itself. Ascending-ID iteration per §2.2.
    fn advance_orbits(
        &mut self,
        t_next: SimTime,
        degraded: &mut Vec<SimError>,
    ) -> Result<(), SimError> {
        for vessel in self.vessels.values() {
            match self.orbits.state_at(&vessel.orbit, t_next) {
                Err(e @ SimError::Fatal(_)) => return Err(e),
                Err(e) => degraded.push(e),
                Ok(_) => {}
            }
        }
        Ok(())
    }

    /// §2.6.2's "fire all events at `t_next` (may insert new events)".
    /// Pops in `(tick, seq)` order (the heap's min-key) and reports every
    /// fired event into `fired`.
    ///
    /// A due batch at one tick is ATOMIC against the §2.6.5
    /// events-per-frame budget — checked between batches by the caller,
    /// never inside one ("fire ALL events at `t_next`" is verbatim spec;
    /// a half-fired tick is exactly the corrupt mid-state back-pressure
    /// exists to avoid). The atomic unit itself is bounded by
    /// [`MAX_EVENTS_PER_BATCH`]: past the ceiling the same-tick tail
    /// stays in the heap — at the clock, since `t_next == clock` now —
    /// and the caller's next iteration fires it in a zero-advance flush
    /// (a deferred tail is the only state in which `tick == clock` is
    /// legal, and `World::restore` accepts `≥`).
    ///
    /// §2.13 Recoverable per event: a failed firing rolls back only that
    /// event's application (earlier fires stand), drops the popped event,
    /// reports the error on `degraded`, and — for vessel-carrying kinds —
    /// re-publishes a bare `SoiRescan` at `clock + DEGRADED_RESCAN_TICKS`
    /// so the subsystem's publish deadline survives the failure. `Fatal`
    /// still aborts the whole iteration.
    fn fire_due_events(
        &mut self,
        t_next: u64,
        processed: &mut u64,
        fired: &mut Vec<ScheduledEvent>,
        degraded: &mut Vec<SimError>,
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        let mut batch = 0u64;
        while self.event_heap.peek().is_some_and(|e| e.tick <= t_next)
            && batch < MAX_EVENTS_PER_BATCH
        {
            // Capture BEFORE the pop mutates the heap.
            self.shadow_events(shadow);
            let Some(event) = self.event_heap.pop() else {
                break;
            };
            *processed += 1;
            batch += 1;
            fired.push(event.clone());
            // Per-event shadow: a Recoverable firing restores only this
            // event's application; the pop stands (the event is dropped).
            let mut ev_shadow = TickShadow::capture(self);
            match self.fire_event(&event, &mut ev_shadow) {
                Ok(()) => {}
                Err(e @ SimError::Fatal(_)) => return Err(e),
                Err(e) => {
                    ev_shadow.restore(self);
                    degraded.push(e);
                    let vessel = match &event.kind {
                        EventKind::SoiTransition { vessel, .. }
                        | EventKind::SoiRescan { vessel } => Some(*vessel),
                        EventKind::PlayerAlarm { .. } => None,
                    };
                    if let Some(vessel) = vessel {
                        self.schedule_event(
                            EventKind::SoiRescan { vessel },
                            self.clock.ticks().saturating_add(DEGRADED_RESCAN_TICKS),
                            shadow,
                        )?;
                    }
                }
            }
        }
        Ok(())
    }

    /// One fired event's effect — the event-side counterpart of
    /// `apply_command` (§2.2's second mutation path).
    fn fire_event(
        &mut self,
        event: &ScheduledEvent,
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        match &event.kind {
            // Alarms mutate nothing: surfacing is `AdvanceReport::
            // fired_events`'s job, and the §2.6.5 Clamping demotion is
            // csp-app's — the sim records no notification state at M1.
            EventKind::PlayerAlarm { .. } => Ok(()),
            // The Orbits subsystem's publish deadline: re-query.
            EventKind::SoiRescan { vessel } => {
                self.schedule_next_soi(*vessel, SimTime(event.tick), shadow, None)
            }
            // §2.6.6: re-base the vessel's elements into the new frame,
            // then publish the new orbit's next event time — suppressing
            // the just-crossed boundary's first-stride re-detection (the
            // ≤1-tick Brent wrong-side artifact).
            EventKind::SoiTransition {
                vessel,
                body,
                direction,
            } => {
                self.apply_soi_transition(*vessel, body, *direction, event.tick, shadow)?;
                self.schedule_next_soi(
                    *vessel,
                    SimTime(event.tick),
                    shadow,
                    Some((body.clone(), *direction)),
                )
            }
        }
    }

    /// The §2.6.6 frame re-base at a crossing: express the vessel's
    /// state in the new parent's frame and re-derive its elements from
    /// the relative cartesian state — the patched-conics stitch.
    ///
    /// - `Enter{body}`: the vessel's new parent is `body` (a child of the
    ///   current parent — guarded, since the scan's candidate set is
    ///   `{parent} ∪ children(parent)`). The relative state is
    ///   `vessel_state − child_state`, both in the old-parent frame.
    /// - `Exit{body}`: `body` must BE the current parent (guarded); the
    ///   new parent is the grandparent, and the vessel's state composes
    ///   as `vessel_in_parent + parent_in_grandparent`.
    ///
    /// A failed re-derivation (`Recoverable`) propagates: the boundary
    /// aborts and the shadow rolls the iteration back — at M1 there is
    /// no event log to degrade into, and "keep the stale conic" would be
    /// silent wrongness.
    fn apply_soi_transition(
        &mut self,
        vessel_id: VesselId,
        body: &BodyId,
        direction: orbit::SoiDirection,
        tick: u64,
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        let t = SimTime(tick);
        self.shadow_vessels(shadow);
        let vessel = self
            .vessels
            .get_mut(&vessel_id)
            .ok_or(SimError::Fatal("soi event for unknown vessel"))?;
        let (rv, vv) = self.orbits.state_at(&vessel.orbit, t)?;
        match direction {
            orbit::SoiDirection::Enter => {
                let child = self.orbits.body_orbit(body)?;
                if child.parent.as_ref() != Some(&vessel.orbit.parent) {
                    return Err(SimError::Recoverable(
                        "soi enter: target is not a child of the vessel's parent",
                    ));
                }
                let child_el = child
                    .elements
                    .clone()
                    .ok_or(SimError::Recoverable("soi enter: child has no orbit"))?;
                // The child's elements are relative to the vessel's
                // current parent with epoch = sim-time 0 (body.ron
                // convention), so its state evaluates in the same frame.
                let child_state = OrbitState {
                    parent: vessel.orbit.parent.clone(),
                    elements: child_el,
                    epoch: SimTime::ZERO,
                };
                let (rb, vb) = self.orbits.state_at(&child_state, t)?;
                let elements = orbit::cartesian_to_elements(rv - rb, vv - vb, child.mu_m3_per_s2)?;
                vessel.orbit = OrbitState {
                    parent: body.clone(),
                    elements,
                    epoch: t,
                };
            }
            orbit::SoiDirection::Exit => {
                if &vessel.orbit.parent != body {
                    return Err(SimError::Recoverable(
                        "soi exit: event body is not the vessel's parent",
                    ));
                }
                let old_parent = self.orbits.body_orbit(body)?;
                let grandparent = old_parent
                    .parent
                    .clone()
                    .ok_or(SimError::Recoverable("soi exit: parent has no parent"))?;
                let parent_el = old_parent
                    .elements
                    .clone()
                    .ok_or(SimError::Recoverable("soi exit: parent has no orbit"))?;
                // The old parent's elements are relative to the
                // grandparent at epoch 0 — compose `vessel_in_parent +
                // parent_in_grandparent` for the grandparent-frame state.
                let parent_state = OrbitState {
                    parent: grandparent.clone(),
                    elements: parent_el,
                    epoch: SimTime::ZERO,
                };
                let (rp, vp) = self.orbits.state_at(&parent_state, t)?;
                let mu_g = self.orbits.body_orbit(&grandparent)?.mu_m3_per_s2;
                let elements = orbit::cartesian_to_elements(rv + rp, vv + vp, mu_g)?;
                vessel.orbit = OrbitState {
                    parent: grandparent,
                    elements,
                    epoch: t,
                };
            }
        }
        Ok(())
    }

    /// One frame's advance — §2.6.2's single advance function, verbatim:
    ///
    /// ```text
    /// loop {
    ///   drain command queue in CommandKey order; apply
    ///   t_next = min(effective_target, event_heap.peek_tick(), tick_budget)
    ///   // bubble ticks: EMPTY at M1 — the bubble drives nothing yet
    ///   advance_interval_subsystems(now, t_next)     // closed form, const order
    ///   clock = t_next
    ///   fire all events at t_next (may insert new events)
    ///   if clock == target || budget_exhausted { break }
    /// }
    /// ```
    ///
    /// Same code path at ×1 and at ×10⁵: under rails the bubble is empty,
    /// so the loop is a jump — `t_next` lands on the earliest event tick
    /// (or the target, or the tick budget), never in between.
    ///
    /// - **Boundary drain** — once per *tick value*: every iteration
    ///   advances the clock by ≥1 tick (scheduled events sit strictly
    ///   ahead — [`Self::schedule_event`] enforces it — and the budget
    ///   bound is `now + budget`), so each tick value hosts at most one
    ///   drain and `CommandKey`s stay strictly increasing by construction.
    ///   A call whose target the clock already reached runs zero
    ///   iterations — drains nothing, stamps nothing, mutates nothing.
    /// - **Events** — due events fire in `(tick, seq)` order at `t_next`;
    ///   firing may insert new events (SOI re-scheduling, alarms never
    ///   insert). The whole same-tick batch is atomic
    ///   ([`Self::fire_due_events`]); the §2.6.5 event budget breaks the
    ///   loop *between* batches → `partial`.
    /// - **Rollback** — every iteration captures a [`TickShadow`] first
    ///   (§2.13): a `Fatal` anywhere in the iteration restores clock,
    ///   warp, log, queue, counters, arena, opt-in set, heap, and
    ///   `event_seq` to the pre-boundary state — and `Err` escapes only
    ///   for `Fatal`. Non-`Fatal` failures are narrower: a rejected
    ///   command or a degraded event rolls back just that item and rides
    ///   the report's `rejected`/`degraded` channels (§2.2.1, §2.13).
    /// - **Deferred batch tails** (§2.6.5): when a tick's due batch exceeds
    ///   [`MAX_EVENTS_PER_BATCH`], the tail stays in the heap at the clock
    ///   and later iterations flush it zero-advance. While the heap top
    ///   sits at-or-behind the clock the tick's command drain is
    ///   postponed: a tick value still hosts exactly one drain, at the
    ///   departing iteration once the deferred batch clears. Heap-min
    ///   `≤ clock` ⟺ mid-flush, so the rule survives snapshot/restore
    ///   with no extra state.
    pub fn step_frame(&mut self, target: u64) -> Result<AdvanceReport, SimError> {
        let mut effective_target = target;
        let mut tick_budget = MAX_TICKS_PER_FRAME;
        let mut commands_applied = 0u64;
        let mut events_processed = 0u64;
        let mut fired_events = Vec::new();
        let mut rejected = Vec::new();
        let mut degraded = Vec::new();
        loop {
            // §2.6.2's break conditions precede the drain: a call with no
            // advance owed is not a tick boundary — drains nothing.
            if self.clock.ticks() >= effective_target || tick_budget == 0 {
                break;
            }
            // §2.13: the shadow precedes every mutation this iteration makes.
            let mut shadow = TickShadow::capture(self);
            // Mid-flush (a deferred over-ceiling batch leaves events at
            // the clock): skip the drain — this tick's boundary drain is
            // postponed until the tail clears, and can never run twice.
            let flushing = self
                .event_heap
                .peek()
                .is_some_and(|e| e.tick <= self.clock.ticks());
            if !flushing
                && let Err(e) = self.drain_boundary(
                    &mut effective_target,
                    &mut shadow,
                    &mut commands_applied,
                    &mut rejected,
                )
            {
                shadow.restore(self);
                return Err(e);
            }
            let now = self.clock.ticks();
            let event_tick = self.event_heap.peek().map(|e| e.tick).unwrap_or(u64::MAX);
            let t_next = effective_target
                .min(event_tick)
                .min(now.saturating_add(tick_budget))
                // A deferred batch tail sits at the clock — the clamp
                // keeps the clock from ever retreating.
                .max(now);
            // Bubble ticks: none — the bubble is empty at M1 (module doc).
            // An empty interval (flush iteration) drives nothing — skip.
            if t_next > now
                && let Err(e) = self.advance_interval_subsystems(
                    SimTime(now),
                    SimTime(t_next),
                    &mut shadow,
                    &mut degraded,
                )
            {
                shadow.restore(self);
                return Err(e);
            }
            // t_next >= now by construction — the subtraction cannot
            // underflow; `advance == 0` on a flush iteration.
            let advance = t_next - now;
            if let Err(e) = self.clock.try_advance(advance) {
                shadow.restore(self);
                return Err(e);
            }
            tick_budget -= advance;
            if let Err(e) = self.fire_due_events(
                t_next,
                &mut events_processed,
                &mut fired_events,
                &mut degraded,
                &mut shadow,
            ) {
                shadow.restore(self);
                return Err(e);
            }
            // Break on reached, tick budget exhausted, or §2.6.5 event
            // budget exhausted → partial at the tick reached.
            if self.clock.ticks() >= effective_target
                || tick_budget == 0
                || events_processed >= self.event_budget
            {
                break;
            }
        }
        Ok(AdvanceReport {
            reached_tick: self.clock.ticks(),
            commands_applied,
            events_processed,
            fired_events,
            rejected,
            degraded,
            partial: self.clock.ticks() < effective_target,
        })
    }

    /// Canonical state hash (PLAN §7.1) — the T1 replay ground truth.
    /// The field walk IS the schema: reorder it and every golden reds by
    /// design (which is why this is a hand-written walk, never a derive).
    ///
    /// Walked: clock, warp rung, per-source counters, event budget,
    /// `next_vessel_id`, the vessel arena, the tracked set,
    /// `next_event_seq`, and the event heap in `(tick, seq)` order — the
    /// state's *forward-looking* content. The `command_log` is NOT
    /// hashed: it is recorded history (§7.6), and replay already proves
    /// it via `StampMismatch`. The orbit model is data-derived (§7.3).
    ///
    /// Fallible per §2.13: a recorded `StateHasher` fault surfaces here.
    pub fn state_hash(&self) -> Result<[u8; 32], SimError> {
        self.state_hash_masked(&BTreeSet::new())
    }

    /// The §7.1 bubble-masked variant of [`Self::state_hash`]: the
    /// §2.6.1 equivalence test's named, documented mask — vessel-arena
    /// entries whose ids are in `in_bubble` are excluded via
    /// `csp_core::bubble_mask`. At M1 no vessel can be bubble-resident
    /// (there is no bubble), so the mask is identity and the test hashes
    /// the whole state — the mask exists so the test is written against
    /// the real seam, not an ad-hoc filter.
    pub fn state_hash_masked(&self, in_bubble: &BTreeSet<u64>) -> Result<[u8; 32], SimError> {
        let mut h = StateHasher::new();
        h.u64(self.clock.ticks());
        h.u32(self.warp_level);
        h.btree_map(
            &self.source_seqs,
            |h, &(class, id)| {
                h.stable_enum(&class);
                h.u32(id);
            },
            |h, &seq| {
                h.u64(seq);
            },
        );
        h.u64(self.event_budget);
        h.u64(self.next_vessel_id);
        h.arena(
            csp_core::bubble_mask(self.vessels.iter().map(|(id, v)| (id.0, v)), in_bubble),
            |h, vessel| {
                h.str(vessel.orbit.parent.as_str());
                h.u64(vessel.orbit.epoch.ticks());
                let el = &vessel.orbit.elements;
                h.f64(el.semi_major_axis_m);
                h.f64(el.eccentricity);
                h.f64(el.inclination_rad);
                h.f64(el.ascending_node_rad);
                h.f64(el.arg_periapsis_rad);
                h.f64(el.mean_anomaly_epoch_rad);
                // `period_s` is deliberately NOT hashed: it is authored-
                // redundant (§3.0 — the data validator checks it; the sim
                // never reads it) and `+∞` on open orbits, which §7.1
                // rule 4 forbids in the stream.
            },
        );
        h.u32(self.tracked_vessels.len() as u32);
        for id in &self.tracked_vessels {
            h.u64(id.0);
        }
        h.u64(self.next_event_seq);
        // The heap's canonical enumeration: (tick, seq) order — its
        // internal sift layout is allocator detail, never hashed.
        let mut events: Vec<&ScheduledEvent> = self.event_heap.iter().collect();
        events.sort_by_key(|e| (e.tick, e.seq));
        h.u32(events.len() as u32);
        for e in events {
            e.hash_into(&mut h);
        }
        h.finish()
    }
}

impl Default for World {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
// §2.13's no-panic rule covers shipping fallible paths; tests signal
// failure by panicking, so unwrap/expect stays legal inside test code.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use orbit::{BodyOrbit, SoiDirection};

    // ---- fixtures ------------------------------------------------------

    // §3.0 home-system numbers (same constants orbit.rs's tests use).
    const MU_SOLEN: f64 = 1.1723e18;
    const MU_TERRIN: f64 = 3.5316e12;
    const MU_RASK: f64 = 6.5138e10;
    const TERRIN_SOI: f64 = 8.4e7;
    const TERRIN_A: f64 = 1.36e10;
    const RASK_A: f64 = 1.2e7;
    const RASK_SOI: f64 = 2.43e6;

    fn circular_elements(a: f64, m0: f64) -> KeplerElements {
        KeplerElements {
            semi_major_axis_m: a,
            eccentricity: 0.0,
            inclination_rad: 0.0,
            ascending_node_rad: 0.0,
            arg_periapsis_rad: 0.0,
            mean_anomaly_epoch_rad: m0,
            period_s: f64::INFINITY, // unused by the model
        }
    }

    /// sol → p (circular Terrin-like orbit) → m (Rask-like moon) — the
    /// smallest system exercising parent-exit AND child-entry.
    fn test_system() -> PatchedConics {
        let mut bodies = BTreeMap::new();
        bodies.insert(
            BodyId::from("sol"),
            BodyOrbit {
                mu_m3_per_s2: MU_SOLEN,
                soi_m: None,
                parent: None,
                elements: None,
            },
        );
        bodies.insert(
            BodyId::from("p"),
            BodyOrbit {
                mu_m3_per_s2: MU_TERRIN,
                soi_m: Some(TERRIN_SOI),
                parent: Some(BodyId::from("sol")),
                elements: Some(circular_elements(TERRIN_A, 0.0)),
            },
        );
        bodies.insert(
            BodyId::from("m"),
            BodyOrbit {
                mu_m3_per_s2: MU_RASK,
                soi_m: Some(RASK_SOI),
                parent: Some(BodyId::from("p")),
                elements: Some(circular_elements(RASK_A, 0.0)),
            },
        );
        PatchedConics::new(bodies)
    }

    /// A 75 km LEO-ish circular orbit about `p` — never crosses anything.
    fn leo_elements() -> KeplerElements {
        circular_elements(6.75e5, 0.0)
    }

    /// A hyperbola about `p` (e = 1.5, periapsis 7e5) that escapes through
    /// `m`'s SOI — the orbit.rs ordering-test construction: Enter{m} then
    /// Exit{m} then Exit{p}. Returns the elements; the moon's M0 is aimed
    /// in `test_system_crossing` so the encounter actually happens.
    fn crossing_vessel_elements() -> KeplerElements {
        let e_v = 1.5;
        let r_p = 7.0e5;
        KeplerElements {
            semi_major_axis_m: -r_p / (e_v - 1.0),
            eccentricity: e_v,
            inclination_rad: 0.0,
            ascending_node_rad: 0.0,
            arg_periapsis_rad: 0.0, // periapsis on +x, departure toward +y
            mean_anomaly_epoch_rad: 0.0,
            period_s: f64::INFINITY,
        }
    }

    /// `test_system` with `m`'s phase aimed at the hyperbola's crossing —
    /// same algebra as `orbit::tests::soi_events_order_across_bodies`.
    fn test_system_crossing() -> PatchedConics {
        let e_v = 1.5;
        let r_p = 7.0e5;
        let a_v: f64 = -r_p / (e_v - 1.0);
        let p_latus = a_v.abs() * (e_v * e_v - 1.0);
        let cos_nu = (p_latus / RASK_A - 1.0) / e_v;
        let nu_cross = libm::acos(cos_nu);
        let f_anom =
            libm::asinh(libm::sqrt(e_v * e_v - 1.0) * libm::sin(nu_cross) / (1.0 + e_v * cos_nu));
        let m_hyp = e_v * libm::sinh(f_anom) - f_anom;
        let n_v = libm::sqrt(MU_TERRIN / (a_v.abs() * a_v.abs() * a_v.abs()));
        let t_cross = m_hyp / n_v;
        let n_moon = libm::sqrt(MU_TERRIN / (RASK_A * RASK_A * RASK_A));
        let moon_m0 = nu_cross - n_moon * t_cross;
        let mut bodies = BTreeMap::new();
        bodies.insert(
            BodyId::from("sol"),
            BodyOrbit {
                mu_m3_per_s2: MU_SOLEN,
                soi_m: None,
                parent: None,
                elements: None,
            },
        );
        bodies.insert(
            BodyId::from("p"),
            BodyOrbit {
                mu_m3_per_s2: MU_TERRIN,
                soi_m: Some(TERRIN_SOI),
                parent: Some(BodyId::from("sol")),
                elements: Some(circular_elements(TERRIN_A, 0.0)),
            },
        );
        bodies.insert(
            BodyId::from("m"),
            BodyOrbit {
                mu_m3_per_s2: MU_RASK,
                soi_m: Some(RASK_SOI),
                parent: Some(BodyId::from("p")),
                elements: Some(circular_elements(RASK_A, moon_m0)),
            },
        );
        PatchedConics::new(bodies)
    }

    // ---- the M0 test set (kept green; restore calls gained the model arg) ----

    /// The drain stamps dequeue-time ticks, not production-time ones
    /// (§2.2.1): a command enqueued at tick 0 but drained at the tick-500
    /// boundary is stamped 500.
    #[test]
    fn stamps_are_dequeue_time() {
        let mut w = World::new();
        w.enqueue(SourceClass::Ui, 0, SimCommand::SetWarp(1));
        w.step_frame(500).unwrap();
        assert_eq!(w.command_log()[0].0.tick, 0);
        w.enqueue(SourceClass::Ui, 0, SimCommand::SetWarp(1));
        w.step_frame(501).unwrap();
        assert_eq!(w.command_log()[1].0.tick, 500);
    }

    /// Same-tick drains order by `source_class` then `source_id`, and
    /// `source_seq` counts per source across drains (§2.2.1).
    #[test]
    fn same_tick_drain_orders_by_class_and_counts_per_source() {
        let mut w = World::new();
        // Enqueued External-first; the drain must still emit Ui first.
        w.enqueue(SourceClass::External, 3, SimCommand::SetWarp(1));
        w.enqueue(SourceClass::Ui, 0, SimCommand::SetWarp(1));
        w.enqueue(SourceClass::Ui, 0, SimCommand::SetWarp(1));
        w.enqueue(SourceClass::External, 3, SimCommand::SetWarp(1));
        w.step_frame(1).unwrap();
        let keys: Vec<CommandKey> = w.command_log().iter().map(|(k, _)| *k).collect();
        assert_eq!(
            keys,
            vec![
                CommandKey {
                    tick: 0,
                    source_class: SourceClass::Ui,
                    source_id: 0,
                    source_seq: 0
                },
                CommandKey {
                    tick: 0,
                    source_class: SourceClass::Ui,
                    source_id: 0,
                    source_seq: 1
                },
                CommandKey {
                    tick: 0,
                    source_class: SourceClass::External,
                    source_id: 3,
                    source_seq: 0
                },
                CommandKey {
                    tick: 0,
                    source_class: SourceClass::External,
                    source_id: 3,
                    source_seq: 1
                },
            ]
        );
        assert!(w.command_log().windows(2).all(|w| w[0].0 < w[1].0));
    }

    /// A call whose target the clock already reached is not a tick
    /// boundary: zero iterations, zero drains, zero mutations — the
    /// queued commands wait for the next real tick (§2.2.1's
    /// drain-once-per-tick-value).
    #[test]
    fn step_frame_at_current_tick_drains_nothing() {
        let mut w = World::new();
        w.step_frame(10).unwrap();
        let pre = w.state_hash().unwrap();
        w.enqueue(SourceClass::Ui, 0, SimCommand::SetWarp(4));
        w.enqueue(SourceClass::Ui, 0, SimCommand::AdvanceTo(50));
        let r = w.step_frame(10).unwrap(); // target == clock: no boundary
        assert_eq!(r.reached_tick, 10);
        assert_eq!(r.commands_applied, 0);
        assert!(!r.partial);
        assert!(w.command_log().is_empty());
        assert_eq!(w.state_hash().unwrap(), pre);
        // The next real tick drains them at the tick-10 boundary; the
        // AdvanceTo then raises the call's effective target.
        let r = w.step_frame(11).unwrap();
        assert_eq!(r.commands_applied, 2);
        assert_eq!(w.command_log()[0].0.tick, 10);
        assert_eq!(w.clock().ticks(), 50);
    }

    /// Zero-tick frames are where naive per-frame draining inverts
    /// production order (Ui's frame-1 commands stamped after Script's
    /// frame-2 ones at the same clock). Because a non-advancing call is
    /// not a boundary, both frames' commands drain together at the next
    /// real tick — strictly increasing `CommandKey`s by construction —
    /// and the §7.6 replay of the key-sorted log reproduces the state.
    #[test]
    fn zero_tick_frames_cannot_invert_command_order() {
        let mut w = World::new();
        w.step_frame(10).unwrap(); // a real past advance; clock at 10
        // Zero-tick frame 1: Ui produces at clock 10 — no drain.
        w.enqueue(SourceClass::Ui, 0, SimCommand::SetWarp(4));
        w.step_frame(10).unwrap();
        // Zero-tick frame 2: Script produces at the same clock 10.
        w.enqueue(SourceClass::Script, 7, SimCommand::SetWarp(2));
        w.step_frame(10).unwrap();
        assert!(w.command_log().is_empty());
        // The next real tick drains both at the tick-10 boundary.
        w.step_frame(11).unwrap();
        let log = w.command_log();
        assert_eq!(log.len(), 2);
        assert!(log.windows(2).all(|w| w[0].0 < w[1].0));
        // Same tick: class order rules — Script before Ui, however late
        // each arrived. Both stamped 10, the dequeue tick.
        assert_eq!(log[0].0.source_class, SourceClass::Script);
        assert_eq!(log[1].0.source_class, SourceClass::Ui);
        assert_eq!(log[0].0.tick, 10);
        assert_eq!(log[1].0.tick, 10);

        // §7.6: sort the recorded log by key (a no-op on a strictly
        // increasing log — asserted), feed it through a fresh world the
        // way `csp_bench::replay` does, and the state must reproduce.
        let mut sorted = log.to_vec();
        sorted.sort_by_key(|(k, _)| *k);
        assert_eq!(sorted, log);
        let mut r = World::new();
        r.step_frame(10).unwrap();
        for (k, cmd) in &sorted {
            r.enqueue(k.source_class, k.source_id, cmd.clone());
        }
        r.step_frame(11).unwrap();
        assert_eq!(r.command_log(), w.command_log());
        assert_eq!(r.state_hash().unwrap(), w.state_hash().unwrap());
    }

    /// §2.13 total rollback: one source, two commands, the second
    /// overflows its `source_seq` counter — a `Fatal` mid-drain must
    /// restore *everything* the boundary touched: clock, warp, log,
    /// counters, AND the drained half of the queue (pushed back in
    /// production order).
    #[test]
    fn rollback_restores_source_seqs_and_hash() {
        // Seed (Ui,0)'s counter at u64::MAX-1 through the §7.2 set: the
        // first drained command increments to u64::MAX, the second
        // overflows — a Fatal mid-drain with the first already applied.
        let mut seqs = BTreeMap::new();
        seqs.insert((SourceClass::Ui, 0u32), u64::MAX - 1);
        let mut w = World::restore(
            PatchedConics::new(BTreeMap::new()),
            WorldSnapshot {
                clock_ticks: 5,
                warp_level: 3,
                source_seqs: seqs.clone(),
                event_budget: DEFAULT_MAX_EVENTS_PER_FRAME,
                next_vessel_id: 0,
                vessels: BTreeMap::new(),
                tracked_vessels: BTreeSet::new(),
                next_event_seq: 0,
                event_heap: Vec::new(),
            },
        )
        .unwrap();
        w.enqueue(SourceClass::Ui, 0, SimCommand::SetWarp(7));
        w.enqueue(SourceClass::Ui, 0, SimCommand::SetWarp(9));
        let pre_hash = w.state_hash().unwrap();
        let err = w.step_frame(6).unwrap_err();
        assert!(matches!(err, SimError::Fatal(_)));
        // Total rollback — the pre-tick hash is the verdict.
        assert_eq!(w.state_hash().unwrap(), pre_hash);
        assert_eq!(w.source_seqs, seqs);
        assert_eq!(w.clock().ticks(), 5);
        assert_eq!(w.warp_level(), 3); // the applied SetWarp(7) is gone
        assert!(w.command_log().is_empty());
        // And the queue got its commands back: a non-empty queue is
        // SaveBlocking at the serialization boundary (§7.4), which is
        // also the observable proof both commands returned.
        assert!(matches!(
            w.check_serializable(),
            Err(SimError::SaveBlocking(_))
        ));
    }

    /// §2.6.3: `SetWarp` is stored state — the rung lands verbatim in
    /// `warp_level` (the sim interprets nothing, so `u32::MAX` is stored,
    /// not clamped) and verbatim in the §7.6 log.
    #[test]
    fn warp_command_semantics() {
        let mut w = World::new();
        w.enqueue(SourceClass::Ui, 0, SimCommand::SetWarp(u32::MAX));
        let r = w.step_frame(1).unwrap();
        assert_eq!(r.commands_applied, 1);
        assert_eq!(r.events_processed, 0);
        assert_eq!(w.warp_level(), u32::MAX);
        assert_eq!(w.command_log()[0].1, SimCommand::SetWarp(u32::MAX));
        // Within one source, per-source seq order applies commands:
        // last writer wins.
        w.enqueue(SourceClass::Ui, 0, SimCommand::SetWarp(4));
        w.enqueue(SourceClass::Ui, 0, SimCommand::SetWarp(16));
        w.step_frame(2).unwrap();
        assert_eq!(w.warp_level(), 16);
        assert_eq!(w.command_log()[1].0.source_seq, 1);
        assert_eq!(w.command_log()[2].0.source_seq, 2);
    }

    /// `CommandKey`'s derived `Ord` is the §2.2.1 strict total order the
    /// drain produces: field precedence is declaration order — `tick`,
    /// then `source_class`, then `source_id`, then `source_seq` — and
    /// distinct keys never compare equal.
    #[test]
    fn command_key_is_a_strict_total_order() {
        let k = |tick, source_class, source_id, source_seq| CommandKey {
            tick,
            source_class,
            source_id,
            source_seq,
        };
        let ascending = [
            // tick dominates everything below it.
            k(9, SourceClass::External, 9, 9),
            k(10, SourceClass::Script, 9, 9),
            // class dominates id and seq.
            k(10, SourceClass::Autopilot, 9, 9),
            k(10, SourceClass::Ui, 0, 9),
            // id dominates seq.
            k(10, SourceClass::Ui, 1, 0),
            k(10, SourceClass::Ui, 1, 1),
            k(10, SourceClass::External, 0, 0),
            k(11, SourceClass::Script, 0, 0),
        ];
        // Strictness, pairwise: a < b and never b < a for every pair.
        for (i, a) in ascending.iter().enumerate() {
            for b in &ascending[i + 1..] {
                assert!(a < b);
                assert!(!(b < a));
                assert_ne!(a, b);
            }
        }
        assert_eq!(ascending[3], ascending[3]); // reflexive
        // Shuffling and re-sorting recovers the order — the log's sort
        // by key is total, never ambiguous.
        let mut shuffled = ascending;
        shuffled.swap(0, 7);
        shuffled.swap(2, 5);
        shuffled.sort();
        assert_eq!(shuffled, ascending);
    }

    /// `AdvanceTo` raises the call's effective target beyond the step
    /// target, and `SetWarp` lands in stored state — the §2.6.3
    /// "warp is issued as logged commands" pattern.
    #[test]
    fn advance_to_extends_target_and_set_warp_stores() {
        let mut w = World::new();
        w.enqueue(SourceClass::Ui, 0, SimCommand::SetWarp(4));
        w.enqueue(SourceClass::Ui, 0, SimCommand::AdvanceTo(500));
        let r = w.step_frame(1).unwrap();
        assert_eq!(r.reached_tick, 500);
        assert_eq!(r.commands_applied, 2);
        assert!(!r.partial);
        assert_eq!(w.warp_level(), 4);
    }

    /// §2.6.5: a target beyond `MAX_TICKS_PER_FRAME` splits the advance —
    /// `partial` is set, the clock still moved, and re-issuing the target
    /// finishes the run.
    #[test]
    fn the_per_call_budget_splits_advances() {
        let mut w = World::new();
        let target = MAX_TICKS_PER_FRAME + 7;
        let r = w.step_frame(target).unwrap();
        assert_eq!(r.reached_tick, MAX_TICKS_PER_FRAME);
        assert!(r.partial);
        let r2 = w.step_frame(target).unwrap();
        assert_eq!(r2.reached_tick, target);
        assert!(!r2.partial);
    }

    /// A stale `AdvanceTo` behind the clock is absorbed at the next real
    /// tick boundary — the clock never retreats.
    #[test]
    fn advance_to_never_retreats_the_clock() {
        let mut w = World::new();
        w.step_frame(100).unwrap();
        w.enqueue(SourceClass::Ui, 0, SimCommand::AdvanceTo(10));
        let r = w.step_frame(100).unwrap(); // zero-advance: not a boundary
        assert_eq!(r.reached_tick, 100);
        assert_eq!(r.commands_applied, 0);
        assert!(!r.partial);
        // The stale target drains at the tick-100 boundary and is
        // absorbed: effective target stays the step's 101.
        let r = w.step_frame(101).unwrap();
        assert_eq!(r.reached_tick, 101);
        assert!(!r.partial);
    }

    /// The §7.4 refusal class at M0: `snapshot` may not run while
    /// commands sit undrained — an enqueued command is not yet world
    /// state, so persisting past it would launder a lost command into a
    /// "clean" save. Draining clears the refusal.
    #[test]
    fn snapshot_refuses_while_commands_are_queued() {
        let mut w = World::new();
        w.enqueue(SourceClass::Ui, 0, SimCommand::SetWarp(2));
        assert!(matches!(
            w.check_serializable(),
            Err(SimError::SaveBlocking(_))
        ));
        assert!(matches!(w.snapshot(), Err(SimError::SaveBlocking(_))));
        w.step_frame(1).unwrap();
        w.snapshot().unwrap();
    }

    /// The §7.2 serialized set: restore produces a world that keeps
    /// stamping the recorded seqs.
    #[test]
    fn snapshot_restore_continues_source_seqs() {
        let mut w = World::new();
        w.enqueue(SourceClass::Ui, 0, SimCommand::SetWarp(1));
        w.step_frame(1).unwrap();
        let snap = w.snapshot().unwrap();
        let mut r = World::restore(PatchedConics::new(BTreeMap::new()), snap).unwrap();
        r.enqueue(SourceClass::Ui, 0, SimCommand::SetWarp(1));
        r.step_frame(2).unwrap();
        // The restored world's next Ui stamp is seq 1, not a reset 0.
        assert_eq!(r.command_log()[0].0.source_seq, 1);
        assert_eq!(r.clock().ticks(), 2);
    }

    /// `state_hash` is sensitive to the clock alone.
    #[test]
    fn state_hash_is_sensitive_to_clock() {
        let mut a = World::new();
        let mut b = World::new();
        a.step_frame(1).unwrap();
        b.step_frame(2).unwrap();
        assert_ne!(a.state_hash().unwrap(), b.state_hash().unwrap());
    }

    /// `state_hash` is sensitive to the stored warp rung — sim state, in
    /// the hash (§2.6.3).
    #[test]
    fn state_hash_is_sensitive_to_warp_level() {
        let mut a = World::new();
        let mut b = World::new();
        a.enqueue(SourceClass::Ui, 0, SimCommand::SetWarp(2));
        b.enqueue(SourceClass::Ui, 0, SimCommand::SetWarp(3));
        a.step_frame(1).unwrap();
        b.step_frame(1).unwrap();
        assert_ne!(a.state_hash().unwrap(), b.state_hash().unwrap());
    }

    /// §7.1 sensitivity, isolated: the per-source `source_seq` counters
    /// alone change the hash — same clock, same warp, different counts.
    #[test]
    fn state_hash_is_sensitive_to_source_seqs_alone() {
        let mut a = World::new();
        let mut b = World::new();
        // SetWarp(1) leaves warp at its initial value; only the counter
        // count differs (one stamp vs two from the same source).
        a.enqueue(SourceClass::Ui, 0, SimCommand::SetWarp(1));
        b.enqueue(SourceClass::Ui, 0, SimCommand::SetWarp(1));
        b.enqueue(SourceClass::Ui, 0, SimCommand::SetWarp(1));
        a.step_frame(5).unwrap();
        b.step_frame(5).unwrap();
        assert_eq!(a.clock(), b.clock());
        assert_eq!(a.warp_level(), b.warp_level());
        assert_ne!(a.source_seqs, b.source_seqs);
        assert_ne!(a.state_hash().unwrap(), b.state_hash().unwrap());
    }

    /// The M0 shape: enqueue + drain + advance reproduce the trivial
    /// scenario's log (mirrors `csp_bench::scenarios::drive_trivial`).
    #[test]
    fn the_trivial_run_shape() {
        let mut w = World::new();
        w.enqueue(SourceClass::Ui, 0, SimCommand::AdvanceTo(500));
        w.step_frame(1).unwrap();
        w.enqueue(SourceClass::Ui, 0, SimCommand::SetWarp(4));
        w.enqueue(SourceClass::External, 3, SimCommand::SetWarp(16));
        w.step_frame(501).unwrap();
        w.enqueue(SourceClass::Ui, 0, SimCommand::AdvanceTo(1000));
        w.step_frame(502).unwrap();
        assert_eq!(w.clock().ticks(), 1000);
        assert_eq!(w.warp_level(), 16);
        assert_eq!(w.command_log().len(), 4);
        assert_eq!(w.command_log()[1].0.source_class, SourceClass::Ui);
        assert_eq!(w.command_log()[2].0.source_class, SourceClass::External);
    }

    // ---- M1-B: event queue, vessels, alarms, equivalence ---------------

    /// The §2.6.5 constant array is pinned verbatim — reordering it
    /// reorders `event_seq` assignment (insertion order), which is a
    /// replay- and save-breaking change (`HASH_SCHEMA_VERSION` bump).
    #[test]
    fn subsystem_order_is_the_spec_array() {
        assert_eq!(
            SUBSYSTEM_ORDER,
            [
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
            ]
        );
    }

    /// `PlayerAlarm` fires as `Clamping` at its tick — a logged command
    /// produces it, the report surfaces it for the warp controller's
    /// demotion (§2.6.5).
    #[test]
    fn player_alarm_fires_as_clamping_at_its_tick() {
        let mut w = World::new();
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::SetAlarm {
                tick: 100,
                label: "transfer".into(),
            },
        );
        w.step_frame(1).unwrap(); // drains at tick 0, schedules seq 0
        let r = w.step_frame(100).unwrap();
        assert_eq!(r.reached_tick, 100);
        assert_eq!(r.events_processed, 1);
        assert_eq!(r.fired_events.len(), 1);
        let e = &r.fired_events[0];
        assert_eq!(e.tick, 100);
        assert_eq!(e.seq, 0);
        assert_eq!(e.class, EventClass::Clamping);
        assert!(matches!(&e.kind, EventKind::PlayerAlarm { label } if label == "transfer"));
        assert!(w.event_heap.is_empty());
    }

    /// An alarm at-or-behind the drain tick clamps to the next tick —
    /// the past cannot be rewritten (and the heap invariant holds:
    /// events sit strictly ahead of the clock).
    #[test]
    fn alarm_in_the_past_clamps_to_next_tick() {
        let mut w = World::new();
        w.step_frame(50).unwrap();
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::SetAlarm {
                tick: 30,
                label: "late".into(),
            },
        );
        // Drains at the tick-50 boundary → clamps to tick 51 — and the
        // SAME call then advances to 51 and fires it (events sit strictly
        // ahead of the clock at schedule time, so 51 it is).
        let r = w.step_frame(51).unwrap();
        assert_eq!(r.fired_events.len(), 1);
        assert_eq!(r.fired_events[0].tick, 51);
        assert_eq!(r.fired_events[0].class, EventClass::Clamping);
    }

    /// `event_seq` is assigned at insertion (§2.6.5): two alarms at the
    /// same tick fire in insertion order — the tie-break that makes the
    /// heap's order total.
    #[test]
    fn event_seq_orders_same_tick_fires_by_insertion() {
        let mut w = World::new();
        for label in ["first", "second", "third"] {
            w.enqueue(
                SourceClass::Ui,
                0,
                SimCommand::SetAlarm {
                    tick: 100,
                    label: label.into(),
                },
            );
        }
        w.step_frame(1).unwrap();
        let r = w.step_frame(100).unwrap();
        assert_eq!(r.events_processed, 3);
        assert_eq!(r.fired_events[0].seq, 0);
        assert_eq!(r.fired_events[1].seq, 1);
        assert_eq!(r.fired_events[2].seq, 2);
        let labels: Vec<&str> = r
            .fired_events
            .iter()
            .map(|e| match &e.kind {
                EventKind::PlayerAlarm { label } => label.as_str(),
                _ => "not-alarm",
            })
            .collect();
        assert_eq!(labels, ["first", "second", "third"]);
    }

    /// §2.6.5 back-pressure: a world with a small event budget fires at
    /// most `event_budget` same-or-later batches per call and returns
    /// `partial` at the tick reached — the caller re-issues and the run
    /// completes. (Separate ticks ⇒ separate atomic batches.)
    #[test]
    fn event_budget_back_pressure_returns_partial() {
        let mut w = World::with_config(PatchedConics::new(BTreeMap::new()), 3);
        for i in 0..5u64 {
            w.enqueue(
                SourceClass::Ui,
                0,
                SimCommand::SetAlarm {
                    tick: 10 + i,
                    label: format!("a{i}"),
                },
            );
        }
        w.step_frame(1).unwrap();
        let r = w.step_frame(60).unwrap();
        // Batches at 10, 11, 12 fired; the budget broke the loop.
        assert_eq!(r.events_processed, 3);
        assert_eq!(r.reached_tick, 12);
        assert!(r.partial);
        let r2 = w.step_frame(60).unwrap();
        assert_eq!(r2.events_processed, 2);
        assert_eq!(r2.reached_tick, 60);
        assert!(!r2.partial);
    }

    /// The budget is checked BETWEEN ticks' batches: a same-tick batch
    /// fires atomically even when it carries the count past the cap —
    /// "fire ALL events at t_next" (§2.6.2) is the atomic unit, and a
    /// half-fired tick is the corrupt state this avoids.
    #[test]
    fn same_tick_batch_is_atomic_over_budget() {
        let mut w = World::with_config(PatchedConics::new(BTreeMap::new()), 2);
        for i in 0..3u64 {
            w.enqueue(
                SourceClass::Ui,
                0,
                SimCommand::SetAlarm {
                    tick: 10,
                    label: format!("a{i}"),
                },
            );
        }
        w.step_frame(1).unwrap();
        let r = w.step_frame(100).unwrap();
        // All three at tick 10 fired in one atomic batch, over the cap.
        assert_eq!(r.events_processed, 3);
        assert_eq!(r.reached_tick, 10);
        assert!(r.partial);
        assert_eq!(r.fired_events.len(), 3);
    }

    /// §2.6.2's refusal seam exists and is honest about being vacuous at
    /// M1 — no bubble, no throttle field, no atmosphere integration.
    /// The frame loop calls it every frame (csp-app wiring), so the
    /// check exists where it can be.
    #[test]
    fn rails_warp_refusal_is_vacuous_at_m1() {
        let w = World::new();
        assert_eq!(w.rails_warp_refusal(), None);
    }

    /// Spawning a vessel allocates a monotonic `VesselId`, validates
    /// parent + elements, and publishes the vessel's next event time —
    /// for a crossing-free orbit, a `SoiRescan` at the bounded horizon
    /// (§2.6.5: publish the next event time, even when it is "re-query").
    #[test]
    fn spawn_vessel_creates_and_schedules() {
        let mut w = World::with_system(test_system());
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::SpawnVessel {
                parent: BodyId::from("p"),
                elements: leo_elements(),
            },
        );
        w.step_frame(1).unwrap();
        assert_eq!(w.vessels.len(), 1);
        let v = &w.vessels[&VesselId(0)];
        assert_eq!(v.orbit.parent, BodyId::from("p"));
        // A LEO vessel crosses nothing: the pending entry is a Silent
        // rescan at the bounded horizon.
        let ev = w.event_heap.peek().unwrap();
        assert_eq!(ev.class, EventClass::Silent);
        assert!(matches!(
            ev.kind,
            EventKind::SoiRescan {
                vessel: VesselId(0)
            }
        ));
        assert!(ev.tick > 0);
    }

    /// Spawn validation is a command error (§2.2.1): the bad spawn is
    /// rejected onto `rejected`, rolled back whole — `source_seq` spent,
    /// vessel allocated, event scheduled all undone — and the boundary
    /// continues. Post-frame state is identical to the command never
    /// having existed (checked against a twin world that never saw it).
    #[test]
    fn spawn_vessel_rejects_unknown_parent() {
        let mut w = World::with_system(test_system());
        let mut twin = World::with_system(test_system());
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::SpawnVessel {
                parent: BodyId::from("nope"),
                elements: leo_elements(),
            },
        );
        // A good command after the bad one proves the boundary continues.
        w.enqueue(SourceClass::Ui, 0, SimCommand::SetWarp(7));
        twin.enqueue(SourceClass::Ui, 0, SimCommand::SetWarp(7));
        let r = w.step_frame(1).unwrap();
        twin.step_frame(1).unwrap();
        assert_eq!(r.rejected.len(), 1);
        assert!(matches!(r.rejected[0].1, SimError::Recoverable(_)));
        assert_eq!(r.rejected[0].0.source_seq, 0); // its would-be key
        assert_eq!(r.commands_applied, 1);
        assert_eq!(w.state_hash().unwrap(), twin.state_hash().unwrap());
        assert_eq!(w.command_log(), twin.command_log());
        assert_eq!(w.next_vessel_id, 0);
        assert!(w.vessels.is_empty());
    }

    /// The §2.6.6 full-cycle: the moon-crossing hyperbola produces
    /// `Enter{m} → Exit{m} → Exit{p}` in that (tick, seq) order, each
    /// transition re-basing the vessel's parent, and each fire
    /// re-publishing the next event. Untracked ⇒ all Silent.
    #[test]
    fn soi_transitions_rebase_vessel_and_reschedule() {
        let mut w = World::with_system(test_system_crossing());
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::SpawnVessel {
                parent: BodyId::from("p"),
                elements: crossing_vessel_elements(),
            },
        );
        w.step_frame(1).unwrap(); // spawn drains at tick 0, schedules Enter{m}
        // Drive well past the whole transit; collect every fired event.
        let end = 300_000 * 64; // ~3.5 sim-days in ticks
        let mut fired = Vec::new();
        while w.clock().ticks() < end {
            let r = w.step_frame(end).unwrap();
            fired.extend(r.fired_events);
            if !r.partial && w.clock().ticks() < end {
                break;
            }
        }
        let chain: Vec<(SoiDirection, &str)> = fired
            .iter()
            .filter_map(|e| match &e.kind {
                EventKind::SoiTransition {
                    body, direction, ..
                } => Some((*direction, body.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(
            chain,
            [
                (SoiDirection::Enter, "m"),
                (SoiDirection::Exit, "m"),
                (SoiDirection::Exit, "p"),
            ],
            "transition chain: {chain:?}"
        );
        // All untracked ⇒ Silent — and strictly increasing (tick, seq).
        assert!(fired.iter().all(|e| e.class == EventClass::Silent));
        assert!(
            fired
                .windows(2)
                .all(|w| (w[0].tick, w[0].seq) < (w[1].tick, w[1].seq))
        );
        // The vessel ended up orbiting sol — the patched-conics stitch.
        let v = &w.vessels[&VesselId(0)];
        assert_eq!(v.orbit.parent, BodyId::from("sol"));
    }

    /// §2.6.5's opt-in: tracking a vessel promotes its PENDING
    /// `SoiTransition` to Clamping (not just future ones), and untracking
    /// demotes it back — while the moon-crossing vessel's Enter{m}
    /// still sits in the heap.
    #[test]
    fn tracked_opt_in_reclasses_pending_events() {
        let mut w = World::with_system(test_system_crossing());
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::SpawnVessel {
                parent: BodyId::from("p"),
                elements: crossing_vessel_elements(),
            },
        );
        w.step_frame(1).unwrap();
        let pending_class = |w: &World| {
            w.event_heap
                .iter()
                .find(|e| matches!(e.kind, EventKind::SoiTransition { .. }))
                .map(|e| e.class)
        };
        assert_eq!(pending_class(&w), Some(EventClass::Silent));
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::SetVesselTracked {
                vessel: VesselId(0),
                tracked: true,
            },
        );
        w.step_frame(2).unwrap();
        assert_eq!(pending_class(&w), Some(EventClass::Clamping));
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::SetVesselTracked {
                vessel: VesselId(0),
                tracked: false,
            },
        );
        w.step_frame(3).unwrap();
        assert_eq!(pending_class(&w), Some(EventClass::Silent));
        // Tracking an unknown vessel is a named rejection, not a no-op
        // and not a frame abort.
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::SetVesselTracked {
                vessel: VesselId(99),
                tracked: true,
            },
        );
        let r = w.step_frame(4).unwrap();
        assert_eq!(r.rejected.len(), 1);
        assert!(matches!(r.rejected[0].1, SimError::Recoverable(_)));
    }

    /// The §7.2 serialized set covers the new state: snapshot mid-run,
    /// restore, continue — same hash as the uninterrupted world. The
    /// heap's canonical `(tick, seq)` enumeration round-trips.
    #[test]
    fn snapshot_restore_roundtrips_vessels_and_events() {
        let mut w = World::with_system(test_system());
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::SpawnVessel {
                parent: BodyId::from("p"),
                elements: leo_elements(),
            },
        );
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::SetAlarm {
                tick: 10_000,
                label: "mid".into(),
            },
        );
        w.step_frame(1).unwrap();
        w.step_frame(5_000).unwrap();
        let snap = w.snapshot().unwrap();
        assert_eq!(snap.event_heap.len(), 2); // alarm + rescan
        assert_eq!(snap.vessels.len(), 1);
        let mut r = World::restore(test_system(), snap).unwrap();
        for _ in 0..2 {
            w.step_frame(20_000).unwrap();
            r.step_frame(20_000).unwrap();
        }
        assert_eq!(r.state_hash().unwrap(), w.state_hash().unwrap());
        assert_eq!(r.clock().ticks(), w.clock().ticks());
        // The §7.2 set does not carry the command log (§7.6: it is an
        // output artifact, replayed INTO the world) — the restored
        // world's log starts empty while the original's keeps its
        // already-applied entries.
    }

    /// Symmetric per-run counters for the equivalence test — call count
    /// and per-call advance are collected so the test can pin the two
    /// runs' *granularity* apart, not just their output.
    #[derive(Default)]
    struct EqStats {
        calls: u64,
        events: u64,
        clamping: u64,
        max_advance: u64,
    }

    /// Drive `w` to `end`, calling `step_frame` with `per_call`-tick
    /// targets (the ×1 shape) or the whole `end` per call when `None`
    /// (the ×10⁵ re-issue pattern).
    fn run_stats(w: &mut World, end: u64, per_call: Option<u64>) -> EqStats {
        let mut s = EqStats::default();
        while w.clock().ticks() < end {
            let prev = w.clock().ticks();
            let target = per_call.map_or(end, |st| (prev + st).min(end));
            let r = w.step_frame(target).unwrap();
            s.calls += 1;
            s.events += r.events_processed;
            s.clamping += r
                .fired_events
                .iter()
                .filter(|e| e.class == EventClass::Clamping)
                .count() as u64;
            s.max_advance = s.max_advance.max(r.reached_tick - prev);
            assert!(r.reached_tick > 0 || r.events_processed > 0);
        }
        s
    }

    /// §2.6.1 — THE milestone test. The same starting world runs 10
    /// sim-days at ×1-shape targets (64-tick frames) and at ×10⁵-shape
    /// targets (full `step_frame(end)` with the tick budget splitting it
    /// into partials). State hashes must be IDENTICAL — the bubble mask
    /// is identity at M1 (no vessel can be bubble-resident), so the
    /// masked comparison walks everything.
    ///
    /// Symmetric machinery counts are pinned too: `events_processed` and
    /// `Clamping` counts must agree in BOTH runs (F7), and the run
    /// granularity is asserted — this can never degrade into the same
    /// shape twice.
    ///
    /// Non-vacuous load: two vessels — a LEO one that never crosses
    /// (Silent `SoiRescan` outstanding) and the moon-crossing hyperbola
    /// (Enter/Exit/Exit transitions, one tracked ⇒ Clamping) — plus a
    /// `PlayerAlarm` mid-window. If partitioning changed anything —
    /// epoch re-stamps, scan phasing, seq assignment — the hash would
    /// diverge.
    #[test]
    fn warp_x1_vs_x1e5_equivalence_10_sim_days() {
        // §3.0's sim-day is Terrin's 21600 s rotation period.
        const DAY_TICKS: u64 = 21_600 * 64;
        const TEN_DAYS: u64 = 10 * DAY_TICKS;

        let build = |tracked: bool| {
            let mut w = World::with_system(test_system_crossing());
            w.enqueue(
                SourceClass::Ui,
                0,
                SimCommand::SpawnVessel {
                    parent: BodyId::from("p"),
                    elements: leo_elements(),
                },
            );
            w.enqueue(
                SourceClass::Ui,
                0,
                SimCommand::SpawnVessel {
                    parent: BodyId::from("p"),
                    elements: crossing_vessel_elements(),
                },
            );
            if tracked {
                w.enqueue(
                    SourceClass::Ui,
                    0,
                    SimCommand::SetVesselTracked {
                        vessel: VesselId(1),
                        tracked: true,
                    },
                );
            }
            w.enqueue(
                SourceClass::Ui,
                0,
                SimCommand::SetAlarm {
                    tick: 5 * DAY_TICKS,
                    label: "mid".into(),
                },
            );
            w.step_frame(1).unwrap(); // the spawn/alarm drain at tick 0
            w
        };

        // Symmetric counters — what both runs must produce identically.
        // ×1: 64-tick frames — real time. ×10⁵: the full target with the
        // tick budget splitting it into ~18-sim-minute partial calls —
        // the warp-re-issue pattern.
        let mut a = build(true);
        let sa = run_stats(&mut a, TEN_DAYS, Some(64));
        let mut b = build(true);
        let sb = run_stats(&mut b, TEN_DAYS, None);
        assert_eq!(a.clock().ticks(), TEN_DAYS);
        assert_eq!(b.clock().ticks(), TEN_DAYS);
        // Granularity pin: run a really is the ×1 shape (~TEN_DAYS/64
        // calls of ≤64 ticks) and run b really is the ×10⁵ shape (single
        // calls advancing ≫64 ticks, ~MAX_TICKS_PER_FRAME apart) — the
        // test cannot degrade into "same granularity twice".
        assert!(sa.calls >= TEN_DAYS / 64 - 16, "run a calls: {}", sa.calls);
        assert!(sa.max_advance <= 64);
        assert!(
            sb.max_advance > 64,
            "run b never warped: {}",
            sb.max_advance
        );
        assert!(
            sb.calls * 64 < sa.calls,
            "b must be materially coarser: {} vs {}",
            sb.calls,
            sa.calls
        );
        // Symmetric event counts: same events processed, same Clamping
        // count in BOTH runs — the machinery counts are part of the
        // equivalence claim, not just the hash.
        assert_eq!(sa.events, sb.events);
        assert!(
            sa.clamping >= 4,
            "expected ≥4 Clamping, saw {}",
            sa.clamping
        );
        assert_eq!(sa.clamping, sb.clamping);
        // §7.1's masked comparison — identity mask at M1, asserted
        // through the documented function.
        let mask = BTreeSet::new();
        assert_eq!(
            a.state_hash_masked(&mask).unwrap(),
            b.state_hash_masked(&mask).unwrap(),
            "×1 vs ×10⁵ state hashes must be identical (§2.6.1)"
        );
        assert_eq!(a.state_hash().unwrap(), b.state_hash().unwrap());
        assert_eq!(a.command_log(), b.command_log());
        let va = &a.vessels[&VesselId(1)];
        assert_eq!(va.orbit.parent, BodyId::from("sol"));
    }

    /// §2.13 Recoverable on the event side: a degenerate `SoiTransition`
    /// (a vessel spawned *parented to m* but parked marginally outside
    /// its SOI, inbound — the wrong-side shape a ≤1-tick-early Brent
    /// landing leaves after a re-base) fails its apply — and the frame
    /// does NOT wedge: the event is dropped, the error rides `degraded`,
    /// a fallback `SoiRescan` keeps the publish deadline alive, and the
    /// run continues. The pre-fix behavior returned `Err`, restoring the
    /// event to the heap to re-fail forever.
    #[test]
    fn degenerate_soi_event_degrades_and_republishes_rescan() {
        let mut w = World::with_system(test_system());
        // About m (μ_RASK), just outside r_soi = 2.43e6, inbound.
        let elements = orbit::cartesian_to_elements(
            csp_core::DVec3::new(2.44e6, 0.0, 0.0),
            csp_core::DVec3::new(-80.0, 150.0, 0.0),
            MU_RASK,
        )
        .unwrap();
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::SpawnVessel {
                parent: BodyId::from("m"),
                elements,
            },
        );
        w.step_frame(1).unwrap();
        // The publish finds Enter{m} — degenerate (m is not a child of
        // itself) but a real detection: suppression only applies to the
        // post-rebase rescan.
        let mut saw_degraded = false;
        for _ in 0..8 {
            let r = w.step_frame(w.clock().ticks() + 5000).unwrap();
            if !r.degraded.is_empty() {
                saw_degraded = true;
                break;
            }
        }
        assert!(saw_degraded, "the degenerate Enter{{m}} must degrade");
        // No wedge: the vessel is still parented to m, a rescan keeps it
        // watched, and the clock keeps advancing.
        assert_eq!(w.vessels[&VesselId(0)].orbit.parent, BodyId::from("m"));
        assert!(
            w.event_heap
                .iter()
                .any(|e| matches!(e.kind, EventKind::SoiRescan { .. }))
        );
        let clock = w.clock().ticks();
        w.step_frame(clock + 100).unwrap();
        assert!(w.clock().ticks() > clock);
    }

    /// §2.6.5's per-batch hard ceiling: a same-tick batch bigger than
    /// [`MAX_EVENTS_PER_BATCH`] fires in ceiling-sized chunks across
    /// zero-advance flush iterations — the tail is deferred, the batch
    /// still completes atomically inside the call, every event fires at
    /// its due tick in (tick, seq) order, and the tick hosts exactly one
    /// command drain (post-flush).
    #[test]
    fn over_ceiling_batch_defers_tail_and_still_completes() {
        let mut w = World::new();
        let n = MAX_EVENTS_PER_BATCH + 7;
        for i in 0..n {
            w.enqueue(
                SourceClass::Ui,
                0,
                SimCommand::SetAlarm {
                    tick: 10,
                    label: format!("a{i}"),
                },
            );
        }
        w.step_frame(1).unwrap();
        let r = w.step_frame(100).unwrap();
        assert_eq!(r.events_processed, n);
        assert_eq!(r.reached_tick, 100);
        assert!(!r.partial);
        // All fired at their due tick, in seq order.
        assert!(r.fired_events.iter().all(|e| e.tick == 10));
        assert!(r.fired_events.windows(2).all(|w| w[0].seq < w[1].seq));
        assert!(w.event_heap.is_empty());
        // Post-flush the tick-10 drain still ran exactly once — the alarm
        // commands drained at tick 0, and nothing re-drained at 10.
        assert_eq!(w.command_log().len() as u64, n);
        assert!(w.command_log().iter().all(|(k, _)| k.tick == 0));
    }

    /// §7.2's restore invariants, exercised on crafted garbage: each
    /// violated invariant is a named `Fatal`, never a half-loaded world.
    #[test]
    fn restore_rejects_crafted_snapshots() {
        let good = || World::new().snapshot().unwrap();
        // Valid baseline restores.
        World::restore(PatchedConics::new(BTreeMap::new()), good()).unwrap();
        let ev = |tick, seq| ScheduledEvent {
            tick,
            seq,
            class: EventClass::Clamping,
            kind: EventKind::PlayerAlarm { label: "x".into() },
        };
        // Event at-or-behind the clock (`<` — `==` is the legal
        // deferred-flush state).
        let mut s = good();
        s.event_heap = vec![ev(4, 0)];
        s.next_event_seq = 1;
        s.clock_ticks = 5;
        assert!(matches!(
            World::restore(PatchedConics::new(BTreeMap::new()), s),
            Err(SimError::Fatal(_))
        ));
        // Duplicate (tick, seq).
        let mut s = good();
        s.event_heap = vec![ev(9, 0), ev(9, 0)];
        s.next_event_seq = 2;
        assert!(matches!(
            World::restore(PatchedConics::new(BTreeMap::new()), s),
            Err(SimError::Fatal(_))
        ));
        // seq not below next_event_seq.
        let mut s = good();
        s.event_heap = vec![ev(9, 3)];
        s.next_event_seq = 3;
        assert!(matches!(
            World::restore(PatchedConics::new(BTreeMap::new()), s),
            Err(SimError::Fatal(_))
        ));
        // Incoherent class (alarm stored Silent instead of Clamping).
        let mut s = good();
        s.event_heap = vec![ScheduledEvent {
            class: EventClass::Silent,
            ..ev(9, 0)
        }];
        s.next_event_seq = 1;
        assert!(matches!(
            World::restore(PatchedConics::new(BTreeMap::new()), s),
            Err(SimError::Fatal(_))
        ));
        // Event naming an unknown vessel.
        let mut s = good();
        s.event_heap = vec![ScheduledEvent {
            tick: 9,
            seq: 0,
            class: EventClass::Silent,
            kind: EventKind::SoiRescan {
                vessel: VesselId(7),
            },
        }];
        s.next_event_seq = 1;
        assert!(matches!(
            World::restore(PatchedConics::new(BTreeMap::new()), s),
            Err(SimError::Fatal(_))
        ));
        // next_vessel_id behind the live arena.
        let mut s = good();
        s.vessels.insert(
            VesselId(3),
            Vessel {
                id: VesselId(3),
                orbit: OrbitState {
                    parent: BodyId::from("p"),
                    elements: leo_elements(),
                    epoch: SimTime(0),
                },
            },
        );
        s.next_vessel_id = 3;
        assert!(matches!(
            World::restore(PatchedConics::new(BTreeMap::new()), s),
            Err(SimError::Fatal(_))
        ));
        // Tracked set outside the arena; zero budget.
        let mut s = good();
        s.tracked_vessels.insert(VesselId(1));
        assert!(matches!(
            World::restore(PatchedConics::new(BTreeMap::new()), s.clone()),
            Err(SimError::Fatal(_))
        ));
        s.tracked_vessels.clear();
        s.event_budget = 0;
        assert!(matches!(
            World::restore(PatchedConics::new(BTreeMap::new()), s),
            Err(SimError::Fatal(_))
        ));
    }

    /// F2: the publish horizon is physical — `SOI_SCAN_HORIZON_PERIODS`
    /// periods of the tightest orbit — and the per-publish sample spend
    /// stays pinned far below the cap for the LEO and hyperbolic
    /// fixtures (not sized *by* the cap).
    #[test]
    fn publish_spend_is_physical_and_bounded() {
        let model = test_system();
        let leo = OrbitState {
            parent: BodyId::from("p"),
            elements: leo_elements(),
            epoch: SimTime(0),
        };
        let span = model.scan_horizon(&leo).unwrap().unwrap();
        // LEO's own period is the tightest orbit in play.
        let t_leo_s =
            2.0 * core::f64::consts::PI / libm::sqrt(MU_TERRIN / (6.75e5 * 6.75e5 * 6.75e5));
        assert_eq!(span, (4.0 * t_leo_s * csp_core::TICK_HZ as f64) as u64);
        let (ev, samples) = model
            .next_soi_event_within(
                &leo,
                SimTime(0),
                SimTime(span),
                orbit::SOI_SCAN_MAX_SAMPLES,
                None,
            )
            .unwrap();
        assert!(ev.is_none());
        // Spend = Σ_c (1 + ⌈span/h_c⌉) — pinned through the overlay's
        // own strides; the whole publish costs ~10² samples, not ~10⁶.
        let bound: u64 = model
            .scan_strides(&leo)
            .unwrap()
            .iter()
            .map(|s| 1 + span.div_ceil(s.stride_ticks))
            .sum();
        assert_eq!(samples, bound);
        assert!(samples < orbit::SOI_SCAN_MAX_SAMPLES / 1000);

        // The hyperbolic fixture: the tightest orbit is the moon's, and
        // one publish covers the whole encounter chain.
        let model = test_system_crossing();
        let hyp = OrbitState {
            parent: BodyId::from("p"),
            elements: crossing_vessel_elements(),
            epoch: SimTime(0),
        };
        let span = model.scan_horizon(&hyp).unwrap().unwrap();
        let t_moon_s =
            2.0 * core::f64::consts::PI / libm::sqrt(MU_TERRIN / (1.2e7 * 1.2e7 * 1.2e7));
        assert_eq!(span, (4.0 * t_moon_s * csp_core::TICK_HZ as f64) as u64);
        let (ev, samples) = model
            .next_soi_event_within(
                &hyp,
                SimTime(0),
                SimTime(span),
                orbit::SOI_SCAN_MAX_SAMPLES,
                None,
            )
            .unwrap();
        assert!(matches!(ev, Some(ref e) if e.body == BodyId::from("m")
            && e.direction == orbit::SoiDirection::Enter));
        assert!(samples < orbit::SOI_SCAN_MAX_SAMPLES / 4);
    }
}
