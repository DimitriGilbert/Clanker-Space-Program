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
//! may insert new events. M2-C fills the bubble slot: between `now` and
//! `t_next` the §2.6.2 loop runs `step_bubble_tick()` per integer tick
//! for every bubble-resident vessel (see [`World::step_bubble_range`]).
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
//! every partition. The bubble mask of the equivalence test excludes a
//! vessel whose state is `InBubble` — its welded body is bubble contents
//! per §2.6.1 ([`World::state_hash_masked`], [`World::in_bubble_ids`]).
//!
//! # Rails-warp refusal (§2.6.2)
//!
//! Rails warp is allowed only when no bubble-resident vessel is under
//! thrust, in unsettled contact, or in atmosphere —
//! [`World::rails_warp_refusal`] checks exactly that over `InBubble`
//! vessels (the throttle field, the geometric ground-contact predicate,
//! and the §3.0 `top_m` altitude gate) and is called every frame by
//! `csp-app`'s warp wiring.

use std::collections::{BTreeMap, BTreeSet, BinaryHeap, VecDeque};

use csp_core::{DQuat, DVec3, SimError, SimTime, StableDiscriminant, StateHasher};
use csp_data::{BodyId, KeplerElements};
use csp_phys::{PhysicsBackend, SimpleBackend, WeldedBody};
use serde::{Deserialize, Serialize};

/// The M1 orbit layer (PLAN §2.8): patched-conics [`orbit::OrbitModel`]
/// with the universal-variable solver and the §2.6.6 SOI-transition scan.
pub mod orbit;

/// The §2.6.5 event-queue data types: classes, kinds, heap entries, and
/// the constant subsystem order array.
pub mod events;

/// The §2.4/§2.6.4 bubble type surface: vessel-residence states, the
/// physical-spawn weld inputs, the body-fixed latch record, and the
/// physics construction config.
pub mod bubble;

pub use bubble::{CraftDef, PhysicsConfig, SurfacePose, VesselState};
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

/// A vessel — the §2.4 unit of craft. `state` is the residence triple
/// (§2.6.2): on rails, in the bubble, or surface-latched.
///
/// `craft` is the weld input set (§2.4) — `Some` iff the vessel can be
/// physics-resident: physical spawns carry it from day one and it is
/// preserved across the bubble↔rails transitions so a vessel that left
/// the bubble can re-materialize (the reentry leg). `None` for
/// elements-only `SpawnVessel`s, which can never enter the bubble.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Vessel {
    /// Its arena key — kept inline per §2.2's arena-of-structs idiom.
    pub id: VesselId,
    /// The weld inputs — see the type doc.
    pub craft: Option<CraftDef>,
    /// The residence state (§2.4/§2.6.2/§2.6.4).
    pub state: VesselState,
    /// §2.4.1's active aero `ConfigMask` — bit i over the craft's
    /// ascending `PartNodeId` set; `ConfigMask::ALL` = every part
    /// aero-active. Sim state: hashed and serialized (the mask selects
    /// which derived aero table is authoritative), while the tables
    /// themselves are re-derived caches.
    pub aero_mask: csp_phys::ConfigMask,
}

impl Vessel {
    /// The rails orbit — `Some` only while [`VesselState::OnRails`].
    pub fn orbit(&self) -> Option<&OrbitState> {
        match &self.state {
            VesselState::OnRails(orbit) => Some(orbit),
            _ => None,
        }
    }

    /// The bubble-resident welded body — `Some` only while
    /// [`VesselState::InBubble`].
    pub fn body(&self) -> Option<&WeldedBody> {
        match &self.state {
            VesselState::InBubble(body) => Some(body),
            _ => None,
        }
    }
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
    /// Spawn a bubble-resident vessel — the §2.4 physical spawn: weld
    /// `craft` → `WeldedBody`, stamp the pose and momentum in the
    /// bubble frame. The apply validates: the world was built
    /// `with_physics`, the craft welds, and every kinematic field is
    /// finite — failures reject onto [`AdvanceReport::rejected`]
    /// (§2.2.1). No SOI event is scheduled; a bubble-resident vessel
    /// has no conic to scan.
    SpawnPhysicalVessel {
        /// The weld inputs (§2.4): part graph + resolved parts.
        craft: CraftDef,
        /// Body-origin position, bubble frame, metres.
        pos: DVec3,
        /// Orientation: vessel body frame → bubble frame.
        rot: csp_core::DQuat,
        /// CoM velocity, bubble frame, m/s.
        vel: DVec3,
        /// Angular velocity, body frame, rad/s.
        ang_vel: DVec3,
    },
    /// Vessel-level throttle fraction — the §2.6.2 rails-refusal input.
    /// Clamped to `[0, 1]` on apply. §2.6.4: a throttle command
    /// unlatches a surface-latched vessel first. On a rails vessel the
    /// command is a `Recoverable` rejection — throttle is zero on rails
    /// by the plan's forced-zero rule, so there is nothing to write.
    SetThrottle {
        /// The vessel to throttle.
        vessel: VesselId,
        /// Requested fraction; non-finite values reject.
        throttle: f64,
    },
    /// Absolute attitude + angular-velocity command — the M2 control
    /// surface standing in for M3+/M9a's real attitude authority.
    /// Absolute, per the command-log discipline. §2.6.4 counts it as a
    /// control command: it unlatches a surface-latched vessel first.
    /// Rails vessels reject — they have no attitude state.
    SetAttitude {
        /// The vessel to re-orient.
        vessel: VesselId,
        /// New orientation: vessel body frame → bubble frame.
        rot: csp_core::DQuat,
        /// New angular velocity, body frame, rad/s.
        ang_vel: DVec3,
    },
    /// §2.6.4's explicit unlatch trigger: restore the latched vessel's
    /// body-fixed pose at the drain tick and return it to the bubble.
    /// Meaningful only from `SurfaceLatched` — an `OnRails`, already
    /// `InBubble`, or unknown vessel is a `Recoverable` rejection on
    /// [`AdvanceReport::rejected`], never a silent no-op.
    Unlatch {
        /// The latched vessel to release.
        vessel: VesselId,
    },
    /// Bubble → rails (§2.6.2): convert the body's osculating state to
    /// elements stamped at the drain tick. Allowed only when the vessel
    /// is bubble-resident, has no live ground contact, is outside the
    /// atmosphere's `top_m`, and the resulting conic clears the surface
    /// (`r_p > radius_m`) — a command outside those is a `Recoverable`
    /// rejection. Throttle is forced to zero on exit — the plan's rule
    /// so a rails vessel can never wedge warp.
    ToRails {
        /// The bubble-resident vessel to put on rails.
        vessel: VesselId,
    },
    /// Rails → bubble: re-materialize the vessel's `WeldedBody` from
    /// its `craft` at the current osculating state (the reentry leg —
    /// §2.6.2's allowed-when is "commanded"). Requires `craft` stored
    /// (physical vessels keep it across transitions; elements-only
    /// spawns reject) and `orbit.parent == physics.dominant`. The
    /// conic contributes position and velocity only — `rot`, `ang_vel`,
    /// `throttle`, and `settle` come out at the weld defaults
    /// (identity, zero, zero, fresh): re-materialization does not try
    /// to reconstruct an attitude the rails never tracked. The
    /// vessel's pending SOI events are dropped: they are dead the
    /// moment it leaves rails.
    ToBubble {
        /// The rails vessel to make physical.
        vessel: VesselId,
    },
    /// §2.4's Stage-2 install path: `csp-app`/worker-computed aero
    /// tables enter the sim only through this command, stamped at the
    /// drain tick. The apply verifies `tables.graph_hash` against the
    /// vessel's stored craft and `tables.mask` against the carried
    /// `mask`, then caches the table; if `mask` is the vessel's active
    /// `aero_mask` and it is `InBubble`, the body is attached
    /// immediately. Debug builds additionally recompute the table
    /// sim-side and byte-compare (`CraftAeroTables::bit_eq`) — a
    /// mismatch is `Fatal` (a determinism break, not bad input).
    InstallAeroTables {
        /// The vessel the tables bind to.
        vessel: VesselId,
        /// The mask the tables were computed for.
        mask: csp_phys::ConfigMask,
        /// The Stage-2 output (self-describing: `graph_hash`, `mask`).
        tables: csp_phys::CraftAeroTables,
    },
    /// §2.4.1 configuration switch — set the vessel's active
    /// `ConfigMask` (deployed/stowed geometry without a graph change:
    /// open bay doors, stowed gear, fairing-state masks). If the
    /// mask's table is not yet computed it is computed here (the
    /// deterministic sim-side path); past the §2.4.1 cap of 8 masks
    /// the command resolves to the nearest computed mask by Hamming
    /// distance (documented — not a rejection). Rails/latched vessels
    /// carry the mask for their next materialization.
    SetAeroConfig {
        /// The vessel to reconfigure.
        vessel: VesselId,
        /// The requested active mask.
        mask: csp_phys::ConfigMask,
    },
    /// Fairing/part jettison: remove one node from the vessel's part
    /// graph — its mass leaves the weld (children re-parent to the
    /// removed node's parent with composed transforms), the body's
    /// kinematics are preserved through the re-weld, and previously
    /// shielded parts gain aero exposure when the fresh Stage-2 table
    /// lands. `InBubble` only in v1 (rails/latched reject); the
    /// jettisoned mass simply leaves the simulation — debris spawn is
    /// M4c's business. `aero_mask` resets to `ConfigMask::ALL` since
    /// the node set changed.
    JettisonPart {
        /// The vessel shedding the part.
        vessel: VesselId,
        /// The graph node to remove (non-root required).
        node: csp_phys::PartNodeId,
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

/// The world's construction config — data-derived (§7.3): the orbit
/// model's body table plus the physics environment and dominant body
/// when the world was built [`World::with_physics`]. The §7.6 replay
/// artifact carries it so a replay rebuilds the same construction
/// without re-reading the recipe.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorldConfig {
    /// The `PatchedConics` body table.
    pub orbits: BTreeMap<BodyId, orbit::BodyOrbit>,
    /// The bubble's environment + dominant body, if physics was
    /// configured.
    pub physics: Option<PhysicsConfig>,
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
    /// The §2.4/§2.9 bubble: backend + dominant body — construction
    /// config (data-derived, §7.3), `None` for worlds built without
    /// physics. `SimpleBackend`'s contact record is scratch — derived
    /// per tick, never hashed or serialized.
    physics: Option<Physics>,
}

/// The world's physics half: the §2.9 backend it steps plus the
/// dominant body the bubble frame is centered on (see
/// [`PhysicsConfig`], the serializable form). Construction config —
/// excluded from the snapshot and the hash.
struct Physics {
    /// The stepping backend — owns its `Environment` and the derived
    /// contact record.
    backend: SimpleBackend,
    /// The environment's central body — bubble-frame parent for
    /// `ToRails`/`ToBubble` and the latch record's `body`.
    dominant: BodyId,
    /// §2.4.1 Stage-2 derived cache: computed aero tables per
    /// `(vessel, ConfigMask)`, a pure function of the stored craft.
    /// Never serialized, never hashed, never shadowed — a cleared or
    /// absent entry is simply recomputed by the same deterministic
    /// path. `InstallAeroTables` writes here; `ensure_aero` reads.
    aero: BTreeMap<VesselId, VesselAeroCache>,
}

/// One vessel's per-mask table cache — `graph_hash` binds the cache
/// to the craft it was built from; a jettison/graph change invalidates
/// it wholesale.
struct VesselAeroCache {
    /// `craft_graph_hash` of the craft these tables were built for.
    graph_hash: u64,
    /// Computed tables by mask — capped at
    /// `csp_phys::MAX_CONFIG_MASKS` per §2.4.1.
    tables: BTreeMap<csp_phys::ConfigMask, csp_phys::CraftAeroTables>,
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
            physics: None,
        }
    }

    /// A world with the §2.4/§2.9 physics bubble configured — the
    /// `SimpleBackend` over `environment` and the dominant body the
    /// bubble frame is centered on. `SpawnPhysicalVessel` requires it.
    pub fn with_physics(orbits: PatchedConics, physics: PhysicsConfig, event_budget: u64) -> Self {
        let mut w = Self::with_config(orbits, event_budget);
        w.physics = Some(Physics {
            backend: SimpleBackend::new(physics.environment),
            dominant: physics.dominant,
            aero: BTreeMap::new(),
        });
        w
    }

    /// The construction config — data-derived (§7.3): the orbit model's
    /// body table and the physics environment + dominant body, when the
    /// world was built `with_physics`. The §7.6 replay artifact carries
    /// it so [`replay`](csp_bench) can rebuild the same construction
    /// without re-running the recipe.
    pub fn config(&self) -> WorldConfig {
        WorldConfig {
            orbits: self.orbits.bodies().clone(),
            physics: self.physics.as_ref().map(|p| PhysicsConfig {
                environment: p.backend.environment().clone(),
                dominant: p.dominant.clone(),
            }),
        }
    }

    /// The physics environment, when the world was built `with_physics`
    /// — observation only (construction config).
    pub fn environment(&self) -> Option<&csp_phys::Environment> {
        self.physics.as_ref().map(|p| p.backend.environment())
    }

    /// Vessel ids currently `InBubble` — the documented mask set the
    /// §2.6.1 equivalence test feeds [`Self::state_hash_masked`].
    pub fn in_bubble_ids(&self) -> BTreeSet<u64> {
        self.vessels
            .iter()
            .filter(|(_, v)| matches!(v.state, VesselState::InBubble(_)))
            .map(|(id, _)| id.0)
            .collect()
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
    ///
    /// `physics` is the same construction-config channel
    /// [`Self::with_physics`] takes: bubble-resident or latched vessels
    /// require it (`Fatal` otherwise — a snapshot their world could not
    /// have produced). For `InBubble` vessels the welded body is
    /// **re-created by re-welding the stored `craft`** and stamping the
    /// serialized kinematic/control fields back on — the §7.3
    /// re-derivation rule, exercised by construction.
    pub fn restore(
        orbits: PatchedConics,
        physics: Option<PhysicsConfig>,
        mut snapshot: WorldSnapshot,
    ) -> Result<Self, SimError> {
        if snapshot.event_budget == 0 {
            return Err(SimError::Fatal("snapshot: zero event budget"));
        }
        if snapshot.next_vessel_id < snapshot.vessels.keys().next_back().map_or(0, |id| id.0 + 1) {
            return Err(SimError::Fatal(
                "snapshot: next_vessel_id not ahead of live ids",
            ));
        }
        for (id, vessel) in &mut snapshot.vessels {
            if vessel.id != *id {
                return Err(SimError::Fatal("snapshot: vessel id != arena key"));
            }
            restore_vessel_state(vessel, physics.is_some())?;
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
            physics: physics.map(|p| Physics {
                backend: SimpleBackend::new(p.environment),
                dominant: p.dominant,
                aero: BTreeMap::new(),
            }),
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
        // §7.4 rule 2: a bubble body with live contacts and not
        // surface-latched refuses the boundary (mid-landing, bouncing,
        // rolling). `SimpleBackend` solves cold from pose every tick —
        // no warm-start state to lose — but the refusal is the plan's
        // letter and protects the same case the Rapier path will need.
        if let Some(physics) = &self.physics {
            for vessel in self.vessels.values() {
                if let VesselState::InBubble(body) = &vessel.state
                    && physics.backend.ground_contact(body)
                {
                    return Err(SimError::SaveBlocking(
                        "bubble body has live contacts and is not surface-latched",
                    ));
                }
            }
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

    /// §2.6.2's rails-warp refusal predicate — checked every frame by
    /// `csp-app`'s warp wiring. Returns the refusal reason when rails
    /// warp must be refused, else `None`. The reason **names the
    /// offending vessel** per §2.6.2 — the warp-refusal UI reads it
    /// verbatim.
    ///
    /// The refusal set is verbatim: a bubble-resident vessel **under
    /// thrust** (`throttle > 0`), in **unsettled contact** (the
    /// geometric ground-contact predicate — a surface-latched vessel
    /// has left the bubble and so never refuses), or **in atmosphere**
    /// (CoM altitude below the §3.0 `top_m`). A ballistic body coasting
    /// in vacuum above `top_m` refuses nothing: per the plan's table,
    /// rails warp is allowed then — the bubble still ticks every
    /// integer tick under it, warp-independent.
    ///
    /// Ascending-`VesselId` scan; the first found reason wins.
    pub fn rails_warp_refusal(&self) -> Option<String> {
        let physics = self.physics.as_ref()?;
        let env = physics.backend.environment();
        for vessel in self.vessels.values() {
            let VesselState::InBubble(body) = &vessel.state else {
                continue;
            };
            if body.throttle > 0.0 {
                return Some(format!(
                    "bubble-resident vessel {} under thrust",
                    vessel.id.0
                ));
            }
            if physics.backend.ground_contact(body) {
                return Some(format!(
                    "bubble-resident vessel {} in unsettled contact",
                    vessel.id.0
                ));
            }
            if let Some(atm) = &env.atmosphere {
                let alt = (body.com_world() - env.body_center).norm() - env.radius_m;
                if alt < atm.top_m {
                    return Some(format!(
                        "bubble-resident vessel {} in atmosphere",
                        vessel.id.0
                    ));
                }
            }
        }
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

    /// §2.6.4's shared unlatch path: returns the vessel's welded body —
    /// re-materializing it from the stored `craft` and `SurfacePose`
    /// first when the vessel is latched (the pose reproduces exactly at
    /// the current clock; fresh `SettleState`, throttle zero — per the
    /// plan's unlatch rules). `Recoverable` for unknown vessels and for
    /// vessels that cannot unlatch (`OnRails` — a control command on
    /// rails has no body to address).
    fn unlatched_body(
        &mut self,
        vessel: VesselId,
        shadow: &mut TickShadow,
    ) -> Result<&mut WeldedBody, SimError> {
        match self.vessels.get(&vessel).map(|v| &v.state) {
            None => return Err(SimError::Recoverable("control: unknown vessel")),
            Some(VesselState::OnRails(_)) => {
                return Err(SimError::Recoverable("control: vessel on rails"));
            }
            Some(VesselState::SurfaceLatched(pose)) => {
                // Store the pose verbatim → re-materialize at the
                // drain tick (§2.6.4: exact reproduction, no drift).
                let pose = pose.clone();
                let env = self
                    .physics
                    .as_ref()
                    .ok_or(SimError::Fatal("latched vessel but no physics environment"))?
                    .backend
                    .environment()
                    .clone();
                let craft = self
                    .vessels
                    .get(&vessel)
                    .and_then(|v| v.craft.clone())
                    .ok_or(SimError::Fatal("latched vessel has no craft"))?;
                self.shadow_vessels(shadow);
                let mut body = csp_phys::weld(&craft.graph, &craft.parts)
                    .map_err(|e| SimError::Fatal(e.reason()))?;
                let (pos, rot, vel, ang_vel) =
                    bubble::surface_pose_restore(&env, &pose, self.clock);
                body.pos = pos;
                body.rot = rot;
                body.vel = vel;
                body.ang_vel = ang_vel;
                self.vessels
                    .get_mut(&vessel)
                    .ok_or(SimError::Fatal("unlatch: vessel vanished"))?
                    .state = VesselState::InBubble(body);
            }
            Some(VesselState::InBubble(_)) => {}
        }
        match &mut self
            .vessels
            .get_mut(&vessel)
            .ok_or(SimError::Fatal("unlatch: vessel vanished"))?
            .state
        {
            VesselState::InBubble(body) => Ok(body),
            _ => Err(SimError::Fatal("unlatch: unreachable state")),
        }
    }

    /// §2.6.2's `for t in now..t_next { step_bubble_tick() }` — the one
    /// place integer physics ticks run. Each bubble-resident vessel's
    /// welded body is stepped once per integer tick, in ascending
    /// `VesselId` order — the §2.9 solver's fixed order; under the M2
    /// contact model (vertex-vs-environment-sphere only, no inter-body
    /// coupling) stepping bodies one at a time is bit-identical to one
    /// slice call, and per-body calls are what keeps the bodies inside
    /// their vessel records.
    ///
    /// A body the backend's settle counter latches this tick transitions
    /// to `SurfaceLatched` immediately: the §2.6.4 body-fixed pose is
    /// captured at the tick's sim time, the body leaves the bubble, and
    /// no further ticks touch it.
    fn step_bubble_range(
        &mut self,
        now: u64,
        t_next: u64,
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        if self.physics.is_none()
            || !self
                .vessels
                .values()
                .any(|v| matches!(v.state, VesselState::InBubble(_)))
        {
            return Ok(());
        }
        // §2.4.1: every InBubble body's attached tables must match its
        // vessel's active mask before the first tick of this range —
        // covers re-welded bodies (restore, unlatch, jettison, spawn)
        // and SetAeroConfig installs. Collected first so the borrow
        // split stays clean; ascending-id order is the fixed order.
        let needing: Vec<VesselId> = self
            .vessels
            .iter()
            .filter(|(_, v)| match &v.state {
                VesselState::InBubble(b) => b.aero.as_ref().is_none_or(|t| t.mask != v.aero_mask),
                _ => false,
            })
            .map(|(id, _)| *id)
            .collect();
        for id in needing {
            self.ensure_aero(id)?;
        }
        self.shadow_vessels(shadow);
        let Self {
            vessels, physics, ..
        } = self;
        let physics = physics
            .as_mut()
            .ok_or(SimError::Fatal("bubble check raced physics"))?;
        for t in now..t_next {
            for vessel in vessels.values_mut() {
                let VesselState::InBubble(body) = &mut vessel.state else {
                    continue;
                };
                if !body.pos.is_finite() {
                    return Err(SimError::Fatal("non-finite body state mid-tick"));
                }
                physics.backend.step(std::slice::from_mut(body), &[]);
                if body.settle.latched {
                    let pose = bubble::surface_pose_capture(
                        physics.backend.environment(),
                        &physics.dominant,
                        body,
                        SimTime(t + 1),
                    );
                    vessel.state = VesselState::SurfaceLatched(pose);
                }
            }
        }
        Ok(())
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
        // A vessel with no conic (bubble-resident or latched) has no
        // scan to publish — the invariant is that SOI events are only
        // ever scheduled while on rails.
        let Some(orbit) = self
            .vessels
            .get(&vessel)
            .ok_or(SimError::Fatal("soi schedule: unknown vessel"))?
            .orbit()
            .cloned()
        else {
            return Ok(());
        };
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
                self.apply_spawn_vessel(parent, elements, shadow)?
            }
            SimCommand::SpawnPhysicalVessel {
                craft,
                pos,
                rot,
                vel,
                ang_vel,
            } => self.apply_spawn_physical(craft, *pos, *rot, *vel, *ang_vel, shadow)?,
            SimCommand::SetThrottle { vessel, throttle } => {
                if !throttle.is_finite() {
                    return Err(SimError::Recoverable("throttle: non-finite"));
                }
                let body = self.unlatched_body(*vessel, shadow)?;
                body.throttle = throttle.clamp(0.0, 1.0);
            }
            SimCommand::SetAttitude {
                vessel,
                rot,
                ang_vel,
            } => {
                if !(rot.is_finite() && ang_vel.is_finite() && rot.norm() > 0.0) {
                    return Err(SimError::Recoverable("attitude: non-finite"));
                }
                let body = self.unlatched_body(*vessel, shadow)?;
                body.rot = rot.normalize();
                body.ang_vel = *ang_vel;
            }
            SimCommand::Unlatch { vessel } => self.apply_unlatch(*vessel, shadow)?,
            SimCommand::ToRails { vessel } => self.apply_to_rails(*vessel, shadow)?,
            SimCommand::ToBubble { vessel } => self.apply_to_bubble(*vessel, shadow)?,
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
            SimCommand::InstallAeroTables {
                vessel,
                mask,
                tables,
            } => self.apply_install_aero(*vessel, *mask, tables, shadow)?,
            SimCommand::SetAeroConfig { vessel, mask } => {
                self.apply_set_aero_config(*vessel, *mask, shadow)?
            }
            SimCommand::JettisonPart { vessel, node } => {
                self.apply_jettison_part(*vessel, *node, shadow)?
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

    /// `Unlatch`: restore the latched vessel's pose and leave the
    /// body's control state at the §2.6.4 defaults (throttle zero).
    /// Meaningful only from `SurfaceLatched`: every other residence —
    /// including already-`InBubble` — is a typed rejection, never a
    /// silent no-op. (`unlatched_body` itself also accepts `InBubble`,
    /// but that is the shared control-command path: a throttle or
    /// attitude command legitimately addresses a live body.)
    fn apply_unlatch(&mut self, vessel: VesselId, shadow: &mut TickShadow) -> Result<(), SimError> {
        match self.vessels.get(&vessel).map(|v| &v.state) {
            Some(VesselState::SurfaceLatched(_)) => {}
            Some(_) => {
                return Err(SimError::Recoverable("unlatch: vessel not surface-latched"));
            }
            None => return Err(SimError::Recoverable("unlatch: unknown vessel")),
        }
        self.unlatched_body(vessel, shadow)?;
        Ok(())
    }

    /// `SpawnVessel` — create an on-rails elements-only vessel at the
    /// drain tick. Validation before allocation: the parent must
    /// exist and the elements must solve at the spawn tick; a failure
    /// rolls the whole command back via `AdvanceReport::rejected`.
    fn apply_spawn_vessel(
        &mut self,
        parent: &BodyId,
        elements: &csp_data::KeplerElements,
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        let orbit = OrbitState {
            parent: parent.clone(),
            elements: elements.clone(),
            // The spawn instant: elements' M0 refers to this tick.
            epoch: self.clock,
        };
        self.orbits.body_orbit(&orbit.parent)?;
        self.orbits.state_at(&orbit, self.clock)?;
        self.shadow_vessels(shadow);
        let id = VesselId(self.next_vessel_id);
        self.next_vessel_id = self
            .next_vessel_id
            .checked_add(1)
            .ok_or(SimError::Fatal("vessel id counter overflow"))?;
        self.vessels.insert(
            id,
            Vessel {
                id,
                craft: None,
                state: VesselState::OnRails(orbit),
                aero_mask: csp_phys::ConfigMask::ALL,
            },
        );
        // §2.6.5: publish the new vessel's next event time now — the
        // heap must hold it before `t_next` is next computed.
        self.schedule_next_soi(id, self.clock, shadow, None)
    }

    /// `SpawnPhysicalVessel`: weld the craft graph into a fresh
    /// `WeldedBody` at the authored kinematics and insert it
    /// `InBubble`. Elements-only vessels (`SpawnVessel`) keep
    /// `craft: None`; this path keeps the graph on the vessel so
    /// bubble↔rails transitions and snapshot re-weld can rebuild it.
    #[allow(clippy::too_many_arguments)]
    fn apply_spawn_physical(
        &mut self,
        craft: &CraftDef,
        pos: DVec3,
        rot: DQuat,
        vel: DVec3,
        ang_vel: DVec3,
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        let Some(_) = &self.physics else {
            return Err(SimError::Recoverable(
                "physical spawn: world has no physics environment",
            ));
        };
        if !(pos.is_finite()
            && rot.is_finite()
            && vel.is_finite()
            && ang_vel.is_finite()
            && rot.norm() > 0.0)
        {
            return Err(SimError::Recoverable(
                "physical spawn: non-finite kinematic state",
            ));
        }
        let mut body = csp_phys::weld(&craft.graph, &craft.parts)
            .map_err(|e| SimError::Recoverable(e.reason()))?;
        body.pos = pos;
        body.rot = rot.normalize();
        body.vel = vel;
        body.ang_vel = ang_vel;
        // §2.4: a craft in an atmosphere-bearing environment gets its
        // Stage-2 default-mask tables at spawn — the sim-side
        // deterministic computation (the same path the install
        // command's debug byte-compare uses). A vacuum environment
        // skips Stage 2 entirely (§2.4.1's skippable clause).
        let gh = craft_graph_hash(craft);
        let mut tables = None;
        if self
            .physics
            .as_ref()
            .is_some_and(|p| p.backend.environment().atmosphere.is_some())
        {
            let t = csp_phys::aero::build_aero_tables(
                &body,
                &craft.graph,
                &craft.parts,
                csp_phys::ConfigMask::ALL,
                gh,
            )
            .map_err(|e| SimError::Recoverable(e.reason()))?;
            body.aero = Some(Box::new(t.clone()));
            tables = Some(t);
        }
        self.shadow_vessels(shadow);
        let id = VesselId(self.next_vessel_id);
        if let (Some(t), Some(p)) = (tables, self.physics.as_mut()) {
            let mut tables_map = BTreeMap::new();
            tables_map.insert(csp_phys::ConfigMask::ALL, t);
            p.aero.insert(
                id,
                VesselAeroCache {
                    graph_hash: gh,
                    tables: tables_map,
                },
            );
        }
        self.next_vessel_id = self
            .next_vessel_id
            .checked_add(1)
            .ok_or(SimError::Fatal("vessel id counter overflow"))?;
        self.vessels.insert(
            id,
            Vessel {
                id,
                craft: Some(craft.clone()),
                state: VesselState::InBubble(body),
                aero_mask: csp_phys::ConfigMask::ALL,
            },
        );
        // No SOI publish: a bubble-resident vessel has no conic.
        Ok(())
    }

    /// `ToRails`: §2.6.2's transition refusals — atmosphere, live
    /// ground contact, a conic that intersects the surface — then the
    /// osculating state stamps epoch-anchored elements and the next
    /// SOI event publishes. The throttle channel dies with the
    /// residence change (on-rails vessels have no throttle).
    fn apply_to_rails(
        &mut self,
        vessel: VesselId,
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        let (env, dominant) = match &self.physics {
            Some(p) => (p.backend.environment().clone(), p.dominant.clone()),
            None => {
                return Err(SimError::Recoverable("to-rails: no physics environment"));
            }
        };
        let elements = {
            let Some(v) = self.vessels.get(&vessel) else {
                return Err(SimError::Recoverable("to-rails: unknown vessel"));
            };
            let VesselState::InBubble(body) = &v.state else {
                return Err(SimError::Recoverable("to-rails: vessel not in bubble"));
            };
            let rel = body.com_world() - env.body_center;
            if let Some(atm) = &env.atmosphere
                && rel.norm() - env.radius_m < atm.top_m
            {
                return Err(SimError::Recoverable("to-rails: vessel in atmosphere"));
            }
            if self
                .physics
                .as_ref()
                .ok_or(SimError::Fatal("to-rails: physics vanished"))?
                .backend
                .ground_contact(body)
            {
                return Err(SimError::Recoverable("to-rails: live ground contact"));
            }
            let (r, v2) = (body.pos - env.body_center, body.vel);
            let elements = orbit::cartesian_to_elements(r, v2, env.mu_m3_per_s2)?;
            // The conic must clear the surface — an orbit that
            // intersects the planet has no rails reading.
            let h2 = r.cross(v2).norm_squared();
            let e = elements.eccentricity;
            let rp = h2 / env.mu_m3_per_s2 / (1.0 + e);
            if rp.partial_cmp(&env.radius_m) != Some(core::cmp::Ordering::Greater) {
                return Err(SimError::Recoverable(
                    "to-rails: conic intersects the surface",
                ));
            }
            elements
        };
        self.shadow_vessels(shadow);
        let vessel_mut = self
            .vessels
            .get_mut(&vessel)
            .ok_or(SimError::Fatal("to-rails: vessel vanished"))?;
        vessel_mut.state = VesselState::OnRails(OrbitState {
            parent: dominant,
            elements,
            epoch: self.clock,
        });
        self.schedule_next_soi(vessel, self.clock, shadow, None)
    }

    /// `ToBubble`: evaluate the on-rails conic at the current tick,
    /// re-weld the stored craft, and stamp the body's kinematics —
    /// then drop the vessel's dead SOI events (it has no conic now).
    fn apply_to_bubble(
        &mut self,
        vessel: VesselId,
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        let (center, dominant) = match &self.physics {
            Some(p) => (p.backend.environment().body_center, p.dominant.clone()),
            None => {
                return Err(SimError::Recoverable("to-bubble: no physics environment"));
            }
        };
        let Some(v) = self.vessels.get(&vessel) else {
            return Err(SimError::Recoverable("to-bubble: unknown vessel"));
        };
        let Some(orbit_state) = v.orbit() else {
            return Err(SimError::Recoverable("to-bubble: vessel not on rails"));
        };
        if orbit_state.parent != dominant {
            return Err(SimError::Recoverable(
                "to-bubble: orbit parent is not the dominant body",
            ));
        }
        let Some(craft) = v.craft.clone() else {
            return Err(SimError::Recoverable("to-bubble: vessel has no craft"));
        };
        let (r, vel) = self.orbits.state_at(orbit_state, self.clock)?;
        self.shadow_vessels(shadow);
        let mut body = csp_phys::weld(&craft.graph, &craft.parts)
            .map_err(|e| SimError::Recoverable(e.reason()))?;
        // The conic contributes pos/vel only — rot/ang_vel/throttle/
        // settle stay at the weld defaults (identity, zero, zero,
        // fresh): re-materialization does not reconstruct an attitude.
        body.pos = r + center;
        body.vel = vel;
        let vessel_mut = self
            .vessels
            .get_mut(&vessel)
            .ok_or(SimError::Fatal("to-bubble: vessel vanished"))?;
        vessel_mut.state = VesselState::InBubble(body);
        self.ensure_aero(vessel)?;
        // The vessel's pending SOI events are dead the moment it
        // leaves rails — drop them in one re-heapify pass.
        self.shadow_events(shadow);
        let heap = std::mem::take(&mut self.event_heap);
        self.event_heap = heap
            .into_iter()
            .filter(|e| !event_names_vessel(&e.kind, &vessel))
            .collect();
        Ok(())
    }

    /// §2.4.1's lazy attach: ensure the `InBubble` vessel's body
    /// carries tables for its active `aero_mask` — a no-op outside
    /// `InBubble`, on airless worlds, or when the attached mask is
    /// already current. Computation is the deterministic sim-side
    /// path; the per-mask cache caps at §2.4.1's 8 tables.
    fn ensure_aero(&mut self, vessel: VesselId) -> Result<(), SimError> {
        let Some(v) = self.vessels.get(&vessel) else {
            return Err(SimError::Recoverable("aero: unknown vessel"));
        };
        let needs = match &v.state {
            VesselState::InBubble(body) => body.aero.as_ref().is_none_or(|t| t.mask != v.aero_mask),
            _ => return Ok(()),
        };
        if !needs {
            return Ok(());
        }
        let Some(craft) = v.craft.clone() else {
            return Err(SimError::Fatal("aero: InBubble vessel has no craft"));
        };
        let mask = v.aero_mask;
        let physics = self
            .physics
            .as_mut()
            .ok_or(SimError::Fatal("aero: physics vanished"))?;
        if physics.backend.environment().atmosphere.is_none() {
            return Ok(());
        }
        let gh = craft_graph_hash(&craft);
        let cache = physics
            .aero
            .entry(vessel)
            .or_insert_with(|| VesselAeroCache {
                graph_hash: gh,
                tables: BTreeMap::new(),
            });
        if cache.graph_hash != gh {
            *cache = VesselAeroCache {
                graph_hash: gh,
                tables: BTreeMap::new(),
            };
        }
        if !cache.tables.contains_key(&mask) && cache.tables.len() < csp_phys::MAX_CONFIG_MASKS {
            let body = match &v.state {
                VesselState::InBubble(b) => b,
                _ => return Err(SimError::Fatal("aero: state changed under attach")),
            };
            let t = csp_phys::aero::build_aero_tables(body, &craft.graph, &craft.parts, mask, gh)
                .map_err(|e| SimError::Recoverable(e.reason()))?;
            cache.tables.insert(mask, t);
        }
        let resolved = if cache.tables.contains_key(&mask) {
            mask
        } else {
            // Past the 8-mask cap: §2.4.1's nearest-by-Hamming rule.
            cache
                .tables
                .keys()
                .min_by_key(|m| (m.hamming(mask), m.0))
                .copied()
                .unwrap_or(mask)
        };
        let tables = cache.tables.get(&resolved).cloned();
        let Some(v) = self.vessels.get_mut(&vessel) else {
            return Err(SimError::Fatal("aero: vessel vanished"));
        };
        if let (Some(t), VesselState::InBubble(b)) = (tables, &mut v.state) {
            b.aero = Some(Box::new(t));
        }
        Ok(())
    }

    /// `InstallAeroTables` — §2.4's Stage-2 install path. Validates
    /// the binding (`graph_hash`, `mask` fields agree), debug
    /// byte-compares against a sim-thread recompute, then caches.
    fn apply_install_aero(
        &mut self,
        vessel: VesselId,
        mask: csp_phys::ConfigMask,
        tables: &csp_phys::CraftAeroTables,
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        let Some(v) = self.vessels.get(&vessel) else {
            return Err(SimError::Recoverable("install-aero: unknown vessel"));
        };
        let Some(craft) = v.craft.clone() else {
            return Err(SimError::Recoverable("install-aero: vessel has no craft"));
        };
        let gh = craft_graph_hash(&craft);
        if tables.graph_hash != gh {
            return Err(SimError::Recoverable("install-aero: graph hash mismatch"));
        }
        if tables.mask != mask {
            return Err(SimError::Recoverable("install-aero: mask field mismatch"));
        }
        // §2.4's debug byte-compare: a worker-computed table must equal
        // the sim-thread recompute bit-for-bit — the determinism
        // gate between the off-tick build and the install path.
        #[cfg(debug_assertions)]
        {
            let mut probe = csp_phys::weld(&craft.graph, &craft.parts)
                .map_err(|e| SimError::Recoverable(e.reason()))?;
            if let VesselState::InBubble(b) = &v.state {
                probe = b.clone();
            }
            let check =
                csp_phys::aero::build_aero_tables(&probe, &craft.graph, &craft.parts, mask, gh)
                    .map_err(|e| SimError::Recoverable(e.reason()))?;
            if !check.bit_eq(tables) {
                return Err(SimError::Fatal(
                    "install-aero: table differs from sim-side recompute",
                ));
            }
        }
        let physics = self.physics.as_mut().ok_or(SimError::Recoverable(
            "install-aero: no physics environment",
        ))?;
        let cache = physics
            .aero
            .entry(vessel)
            .or_insert_with(|| VesselAeroCache {
                graph_hash: gh,
                tables: BTreeMap::new(),
            });
        if cache.graph_hash != gh {
            *cache = VesselAeroCache {
                graph_hash: gh,
                tables: BTreeMap::new(),
            };
        }
        if !cache.tables.contains_key(&mask) && cache.tables.len() >= csp_phys::MAX_CONFIG_MASKS {
            return Err(SimError::Recoverable(
                "install-aero: config-mask table cap reached",
            ));
        }
        cache.tables.insert(mask, tables.clone());
        if v.aero_mask == mask
            && let VesselState::InBubble(_) = v.state
        {
            self.shadow_vessels(shadow);
            if let Some(VesselState::InBubble(b)) =
                self.vessels.get_mut(&vessel).map(|v| &mut v.state)
            {
                b.aero = Some(Box::new(tables.clone()));
            }
        }
        Ok(())
    }

    /// `SetAeroConfig` — select the vessel's active mask; the table
    /// computes on demand (cap → nearest-computed fallback).
    fn apply_set_aero_config(
        &mut self,
        vessel: VesselId,
        mask: csp_phys::ConfigMask,
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        let Some(v) = self.vessels.get(&vessel) else {
            return Err(SimError::Recoverable("aero-config: unknown vessel"));
        };
        if v.craft.is_none() {
            return Err(SimError::Recoverable("aero-config: vessel has no craft"));
        }
        // Materialize (or nearest-resolve) the mask's table first —
        // only then is the vessel's active mask updated.
        let craft = v
            .craft
            .clone()
            .ok_or(SimError::Fatal("aero-config: craft vanished"))?;
        let gh = craft_graph_hash(&craft);
        let physics = self
            .physics
            .as_mut()
            .ok_or(SimError::Recoverable("aero-config: no physics"))?;
        let cache = physics
            .aero
            .entry(vessel)
            .or_insert_with(|| VesselAeroCache {
                graph_hash: gh,
                tables: BTreeMap::new(),
            });
        if cache.graph_hash != gh {
            *cache = VesselAeroCache {
                graph_hash: gh,
                tables: BTreeMap::new(),
            };
        }
        if !cache.tables.contains_key(&mask) && cache.tables.len() < csp_phys::MAX_CONFIG_MASKS {
            // Weld a stand-in body for the build when the vessel is not
            // InBubble (rails/latched) — the voxelizer needs collider
            // geometry, which is a pure function of the craft.
            let body = match &v.state {
                VesselState::InBubble(b) => b.clone(),
                _ => csp_phys::weld(&craft.graph, &craft.parts)
                    .map_err(|e| SimError::Recoverable(e.reason()))?,
            };
            let t = csp_phys::aero::build_aero_tables(&body, &craft.graph, &craft.parts, mask, gh)
                .map_err(|e| SimError::Recoverable(e.reason()))?;
            cache.tables.insert(mask, t);
        }
        let resolved = if cache.tables.contains_key(&mask) {
            mask
        } else {
            cache
                .tables
                .keys()
                .min_by_key(|m| (m.hamming(mask), m.0))
                .copied()
                .unwrap_or(mask)
        };
        self.shadow_vessels(shadow);
        let v = self
            .vessels
            .get_mut(&vessel)
            .ok_or(SimError::Fatal("aero-config: vessel vanished"))?;
        v.aero_mask = resolved;
        if let VesselState::InBubble(b) = &mut v.state {
            let physics = self
                .physics
                .as_ref()
                .ok_or(SimError::Fatal("aero-config: physics vanished"))?;
            if let Some(t) = physics
                .aero
                .get(&vessel)
                .and_then(|c| c.tables.get(&resolved))
            {
                b.aero = Some(Box::new(t.clone()));
            }
        }
        Ok(())
    }

    /// `JettisonPart` — remove one graph node: children reparent to
    /// the removed node's parent (transforms compose), the parts
    /// slice is left with an orphan entry (unreferenced effective
    /// indices are ignored by the weld — the mass is genuinely gone),
    /// the body re-welds preserving kinematics, and fresh default-mask
    /// tables attach. `InBubble` only.
    fn apply_jettison_part(
        &mut self,
        vessel: VesselId,
        node: csp_phys::PartNodeId,
        shadow: &mut TickShadow,
    ) -> Result<(), SimError> {
        let Some(v) = self.vessels.get(&vessel) else {
            return Err(SimError::Recoverable("jettison: unknown vessel"));
        };
        if !matches!(v.state, VesselState::InBubble(_)) {
            return Err(SimError::Recoverable("jettison: vessel not in bubble"));
        }
        let Some(mut craft) = v.craft.clone() else {
            return Err(SimError::Recoverable("jettison: vessel has no craft"));
        };
        if !craft.graph.nodes.contains_key(&node) {
            return Err(SimError::Recoverable("jettison: unknown part node"));
        }
        if node == craft.graph.root {
            return Err(SimError::Recoverable("jettison: cannot drop the root"));
        }
        // Reparent children of `node` to node's parent, composing
        // transforms — documented jettison semantics.
        let removed = craft.graph.nodes[&node].clone();
        for child_id in craft
            .graph
            .nodes
            .iter()
            .filter(|(_, n)| n.parent == Some(node))
            .map(|(id, _)| *id)
            .collect::<Vec<_>>()
        {
            let child = craft
                .graph
                .nodes
                .get_mut(&child_id)
                .ok_or(SimError::Fatal("jettison: child vanished mid-edit"))?;
            child.transform = removed.transform.compose(child.transform);
            child.parent = removed.parent;
        }
        craft.graph.nodes.remove(&node);
        let mut body = csp_phys::weld(&craft.graph, &craft.parts)
            .map_err(|e| SimError::Recoverable(e.reason()))?;
        // Preserve kinematics + throttle through the re-weld; the
        // jettisoned mass leaves the simulation (debris is M4c).
        let (pos, rot, vel, ang_vel, throttle, settle, com_old) = match &v.state {
            VesselState::InBubble(b) => (
                b.pos,
                b.rot,
                b.vel,
                b.ang_vel,
                b.throttle,
                b.settle,
                b.com_local,
            ),
            _ => return Err(SimError::Fatal("jettison: state changed mid-apply")),
        };
        body.pos = pos;
        body.rot = rot;
        // `vel` is the CoM velocity and the re-weld moved `com_local`:
        // the material point that is now the CoM rides the rigid
        // body's `v + ω×r`, so the retained fragment's CoM velocity is
        // `vel + ω_world × (com_new − com_old)_world`. Skipping this
        // would silently drop `ω×Δr` — order ‖ω‖·‖Δcom‖, metres/s for
        // a tumbling vessel. `pos`/`rot` are unchanged, so
        // `com_world` already lands on the correct material point.
        let omega_world = rot.rotate(ang_vel);
        body.vel = vel + omega_world.cross(rot.rotate(body.com_local - com_old));
        body.ang_vel = ang_vel;
        body.throttle = throttle;
        body.settle = settle;
        self.shadow_vessels(shadow);
        let v = self
            .vessels
            .get_mut(&vessel)
            .ok_or(SimError::Fatal("jettison: vessel vanished"))?;
        v.craft = Some(craft);
        v.aero_mask = csp_phys::ConfigMask::ALL;
        v.state = VesselState::InBubble(body);
        if let Some(p) = self.physics.as_mut() {
            p.aero.remove(&vessel);
        }
        self.ensure_aero(vessel)
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
            // Bubble-resident and latched vessels have no conic — the
            // physics tick and the pose record are their truth.
            let Some(orbit) = vessel.orbit() else {
                continue;
            };
            match self.orbits.state_at(orbit, t_next) {
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
        // The event only exists for a vessel that was on rails when it
        // was scheduled; a stale firing on a non-rails vessel is a
        // `Recoverable` degradation, never a frame-up lie.
        let Some(orbit) = self
            .vessels
            .get(&vessel_id)
            .ok_or(SimError::Fatal("soi event for unknown vessel"))?
            .orbit()
            .cloned()
        else {
            return Err(SimError::Recoverable("soi event: vessel not on rails"));
        };
        let (rv, vv) = self.orbits.state_at(&orbit, t)?;
        let new_orbit = match direction {
            orbit::SoiDirection::Enter => {
                let child = self.orbits.body_orbit(body)?;
                if child.parent.as_ref() != Some(&orbit.parent) {
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
                    parent: orbit.parent.clone(),
                    elements: child_el,
                    epoch: SimTime::ZERO,
                };
                let (rb, vb) = self.orbits.state_at(&child_state, t)?;
                let elements = orbit::cartesian_to_elements(rv - rb, vv - vb, child.mu_m3_per_s2)?;
                OrbitState {
                    parent: body.clone(),
                    elements,
                    epoch: t,
                }
            }
            orbit::SoiDirection::Exit => {
                if &orbit.parent != body {
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
                OrbitState {
                    parent: grandparent,
                    elements,
                    epoch: t,
                }
            }
        };
        self.vessels
            .get_mut(&vessel_id)
            .ok_or(SimError::Fatal("soi event: vessel vanished"))?
            .state = VesselState::OnRails(new_orbit);
        Ok(())
    }

    /// One frame's advance — §2.6.2's single advance function, verbatim:
    ///
    /// ```text
    /// loop {
    ///   drain command queue in CommandKey order; apply
    ///   t_next = min(effective_target, event_heap.peek_tick(), tick_budget)
    ///   step_bubble_range(now, t_next)  // per-integer-tick physics, M2-C
    ///   advance_interval_subsystems(now, t_next)     // closed form, const order
    ///   clock = t_next
    ///   fire all events at t_next (may insert new events)
    ///   if clock == target || budget_exhausted { break }
    /// }
    /// ```
    ///
    /// Same code path at ×1 and at ×10⁵: the bubble slot ticks every
    /// `InBubble` body once per integer tick inside `(now, t_next)` —
    /// warp-independent — and an empty bubble makes the loop a pure
    /// jump: `t_next` lands on the earliest event tick (or the target,
    /// or the tick budget), never in between.
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
            // §2.6.2's bubble slot: `for t in now..t_next { step_bubble_
            // tick() }` — integer physics ticks for every InBubble
            // vessel, warp-independent. An empty interval (flush
            // iteration) drives nothing — both calls skip on `t_next ==
            // now` (`now..t_next` is empty; the subsystem advance is
            // guarded).
            if t_next > now
                && let Err(e) = self.step_bubble_range(now, t_next, &mut shadow)
            {
                shadow.restore(self);
                return Err(e);
            }
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
    /// `csp_core::bubble_mask`. Since M2-C a vessel can be
    /// bubble-resident, so the mask does real work: the equivalence
    /// test feeds each run's own [`Self::in_bubble_ids`] and spawns a
    /// vessel `InBubble` mid-run so the exclusion is genuinely
    /// exercised — the §2.6.1 claim is "identical state except bubble
    /// contents", applied through the real seam, not an ad-hoc filter.
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
                hash_vessel(h, vessel);
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

/// Does this scheduled event name `vessel`? Used by the `ToBubble`
/// apply to drop the vessel's dead SOI events when it leaves rails.
fn event_names_vessel(kind: &EventKind, vessel: &VesselId) -> bool {
    match kind {
        EventKind::SoiTransition { vessel: v, .. } | EventKind::SoiRescan { vessel: v } => {
            v == vessel
        }
        EventKind::PlayerAlarm { .. } => false,
    }
}

/// §7.3 restore validation per vessel. `InBubble` bodies are rebuilt
/// by re-welding the stored craft — the weld's mass properties are a
/// derived cache, so restoring means re-running `weld` and stamping the
/// serialized kinematic/control/settle fields back on. `SurfaceLatched`
/// keeps only the fixed-frame pose record; `OnRails` needs nothing.
fn restore_vessel_state(vessel: &mut Vessel, has_physics: bool) -> Result<(), SimError> {
    match &mut vessel.state {
        VesselState::InBubble(body) => {
            if !has_physics {
                return Err(SimError::Fatal(
                    "snapshot: bubble-resident vessel but no physics config",
                ));
            }
            let Some(craft) = &vessel.craft else {
                return Err(SimError::Fatal("snapshot: InBubble vessel has no craft"));
            };
            let (pos, rot, vel, ang_vel, throttle, settle) = (
                body.pos,
                body.rot,
                body.vel,
                body.ang_vel,
                body.throttle,
                body.settle,
            );
            *body = csp_phys::weld(&craft.graph, &craft.parts)
                .map_err(|e| SimError::Fatal(e.reason()))?;
            if !(pos.is_finite()
                && rot.is_finite()
                && vel.is_finite()
                && ang_vel.is_finite()
                && throttle.is_finite())
            {
                return Err(SimError::Fatal("snapshot: non-finite body state"));
            }
            body.pos = pos;
            body.rot = rot;
            body.vel = vel;
            body.ang_vel = ang_vel;
            body.throttle = throttle;
            body.settle = settle;
        }
        VesselState::SurfaceLatched(pose) => {
            if !has_physics {
                return Err(SimError::Fatal(
                    "snapshot: latched vessel but no physics config",
                ));
            }
            if vessel.craft.is_none() {
                return Err(SimError::Fatal("snapshot: latched vessel has no craft"));
            }
            if !(pose.latitude_rad.is_finite()
                && pose.longitude_rad.is_finite()
                && pose.altitude_m.is_finite()
                && pose.orientation.is_finite())
            {
                return Err(SimError::Fatal("snapshot: non-finite latch pose"));
            }
        }
        VesselState::OnRails(_) => {}
    }
    Ok(())
}

/// The per-vessel hash walk (§7.2's authoritative fields): the craft
/// definition in full (the weld's mass properties are a pure function
/// of it, so hashing it covers them honestly), then the residence
/// payload — rails elements, the bubble body's kinematic + control +
/// settle state, or the latched fixed-frame pose.
/// The craft walk — the vessel's authoritative physical description.
/// The weld's mass properties are a pure function of this, so hashing
/// it covers them honestly (they are derived, never themselves state).
fn hash_craft(h: &mut StateHasher, craft: &CraftDef) {
    h.u64(craft.graph.root.0);
    h.btree_map(
        &craft.graph.nodes,
        |h, id| {
            h.u64(id.0);
        },
        |h, node| {
            h.opt(&node.parent, |h, p| {
                h.u64(p.0);
            });
            let t = &node.transform;
            h.f64(t.offset.x);
            h.f64(t.offset.y);
            h.f64(t.offset.z);
            h.f64(t.rotation.x);
            h.f64(t.rotation.y);
            h.f64(t.rotation.z);
            h.f64(t.rotation.w);
            h.u32(node.effective_index);
        },
    );
    h.vec(&craft.parts, |h, p| {
        h.f64(p.mass_kg);
        h.f64(p.com.x);
        h.f64(p.com.y);
        h.f64(p.com.z);
        h.f64(p.inertia.xx);
        h.f64(p.inertia.yy);
        h.f64(p.inertia.zz);
        h.f64(p.inertia.xy);
        h.f64(p.inertia.xz);
        h.f64(p.inertia.yz);
        match &p.collider {
            csp_phys::ColliderShape::Points(points) => {
                h.u16(0);
                h.vec(points, |h, v| {
                    h.f64(v.x);
                    h.f64(v.y);
                    h.f64(v.z);
                });
            }
            csp_phys::ColliderShape::Box { half_extents } => {
                h.u16(1);
                h.f64(half_extents.x);
                h.f64(half_extents.y);
                h.f64(half_extents.z);
            }
        }
        h.vec(&p.force_sites, |h, s| {
            h.f64(s.local_pos.x);
            h.f64(s.local_pos.y);
            h.f64(s.local_pos.z);
            h.f64(s.local_dir.x);
            h.f64(s.local_dir.y);
            h.f64(s.local_dir.z);
            h.stable_enum(&s.kind);
            h.f64(s.rated_n);
        });
        h.vec(&p.aero.cd_curve, |h, k| {
            h.f64(k.mach);
            h.f64(k.coeff);
        });
        h.vec(&p.aero.cl_curve, |h, k| {
            h.f64(k.mach);
            h.f64(k.coeff);
        });
        h.f64(p.aero.ref_len_m);
    });
}

/// The §2.4 craft identity the aero-table install command binds on:
/// first 8 bytes (LE) of the same canonical `hash_craft` walk the
/// state hash uses — a table built for one craft cannot install on
/// another. `0` when the hash stream faults (non-finite craft data —
/// weld rejects it anyway).
fn craft_graph_hash(craft: &CraftDef) -> u64 {
    let mut h = StateHasher::new();
    hash_craft(&mut h, craft);
    match h.finish() {
        Ok(d) => u64::from_le_bytes(d[..8].try_into().unwrap_or([0; 8])),
        Err(_) => 0,
    }
}

fn hash_vessel(h: &mut StateHasher, vessel: &Vessel) {
    h.opt(&vessel.craft, |h, craft| {
        hash_craft(h, craft);
    });
    h.u64(vessel.aero_mask.0);
    h.stable_enum(&vessel.state);
    match &vessel.state {
        VesselState::OnRails(orbit) => {
            h.str(orbit.parent.as_str());
            h.u64(orbit.epoch.ticks());
            let el = &orbit.elements;
            h.f64(el.semi_major_axis_m);
            h.f64(el.eccentricity);
            h.f64(el.inclination_rad);
            h.f64(el.ascending_node_rad);
            h.f64(el.arg_periapsis_rad);
            h.f64(el.mean_anomaly_epoch_rad);
            // `period_s` is deliberately NOT hashed: it is
            // authored-redundant (§3.0 — the data validator checks it;
            // the sim never reads it) and `+∞` on open orbits, which
            // §7.1 rule 4 forbids in the stream.
        }
        VesselState::InBubble(body) => {
            // The authoritative subset — §7.2's walk covers kinematic +
            // control + settle state; the weld's mass properties are
            // derived from the hashed craft, not themselves state.
            h.f64(body.pos.x);
            h.f64(body.pos.y);
            h.f64(body.pos.z);
            h.f64(body.rot.x);
            h.f64(body.rot.y);
            h.f64(body.rot.z);
            h.f64(body.rot.w);
            h.f64(body.vel.x);
            h.f64(body.vel.y);
            h.f64(body.vel.z);
            h.f64(body.ang_vel.x);
            h.f64(body.ang_vel.y);
            h.f64(body.ang_vel.z);
            h.f64(body.throttle);
            h.u32(body.settle.settled_ticks);
            h.bool(body.settle.latched);
        }
        VesselState::SurfaceLatched(pose) => {
            h.str(pose.body.as_str());
            h.f64(pose.latitude_rad);
            h.f64(pose.longitude_rad);
            h.f64(pose.altitude_m);
            h.f64(pose.orientation.x);
            h.f64(pose.orientation.y);
            h.f64(pose.orientation.z);
            h.f64(pose.orientation.w);
        }
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
            None,
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
        let mut r = World::restore(PatchedConics::new(BTreeMap::new()), None, snap).unwrap();
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

    /// §2.6.2's refusal seam exists and is vacuous on a physics-free
    /// world — a world built without a physics environment cannot hold
    /// a bubble-resident vessel, so the predicate reports `None`. The
    /// frame loop calls it every frame (csp-app wiring), so the check
    /// exists where it can be.
    #[test]
    fn rails_warp_refusal_is_vacuous_without_physics() {
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
        assert_eq!(v.orbit().unwrap().parent, BodyId::from("p"));
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
        assert_eq!(v.orbit().unwrap().parent, BodyId::from("sol"));
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
        let mut r = World::restore(test_system(), None, snap).unwrap();
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
    /// into partials). State hashes must be IDENTICAL — the comparison
    /// runs through [`World::state_hash_masked`] with each run's own
    /// [`World::in_bubble_ids`], and the mask is exercised for real: a
    /// third vessel spawns `InBubble` mid-run (a ballistic orbiter well
    /// above the atmosphere), so the equivalence claim covers "identical
    /// state except bubble contents", not an empty-mask walk.
    ///
    /// Symmetric machinery counts are pinned too: `events_processed` and
    /// `Clamping` counts must agree in BOTH runs (F7), and the run
    /// granularity is asserted — this can never degrade into the same
    /// shape twice.
    ///
    /// Non-vacuous load: two rails vessels — a LEO one that never
    /// crosses (Silent `SoiRescan` outstanding) and the moon-crossing
    /// hyperbola (Enter/Exit/Exit transitions, one tracked ⇒ Clamping)
    /// — plus a `PlayerAlarm` mid-window and the mid-run bubble
    /// resident. If partitioning changed anything — epoch re-stamps,
    /// scan phasing, seq assignment, per-tick bubble stepping — the
    /// hash would diverge.
    #[test]
    fn warp_x1_vs_x1e5_equivalence_10_sim_days() {
        // §3.0's sim-day is Terrin's 21600 s rotation period.
        const DAY_TICKS: u64 = 21_600 * 64;
        const TEN_DAYS: u64 = 10 * DAY_TICKS;

        // A physics world now: the mask needs a bubble to exclude.
        let build = |tracked: bool| {
            let mut w = World::with_physics(
                test_system_crossing(),
                PhysicsConfig {
                    environment: test_env(true),
                    dominant: BodyId::from("p"),
                },
                DEFAULT_MAX_EVENTS_PER_FRAME,
            );
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

        // The mid-run physical spawn is enqueued at the SAME clock in
        // both runs (TEN_DAYS/2 is a multiple of the ×1 frame's 64
        // ticks, so both worlds sit at exactly that tick when it drains
        // — the command stamps identically and the vessel goes InBubble
        // at the same tick in both partitions).

        // Symmetric counters — what both runs must produce identically.
        // ×1: 64-tick frames — real time. ×10⁵: the full target with the
        // tick budget splitting it into ~18-sim-minute partial calls —
        // the warp-re-issue pattern.
        let mut a = build(true);
        let mut b = build(true);
        // First half: no bubble content — identical preamble.
        let mut sa = run_stats(&mut a, TEN_DAYS / 2, Some(64));
        let mut sb = run_stats(&mut b, TEN_DAYS / 2, None);
        assert_eq!(a.clock().ticks(), TEN_DAYS / 2);
        assert_eq!(b.clock().ticks(), TEN_DAYS / 2);
        enqueue_orbiter(&mut a);
        enqueue_orbiter(&mut b);
        // Second half: the bubble resident ticks every integer tick in
        // BOTH runs — per-tick work under ×1, one `now..t_next` range
        // per partial under ×10⁵, bitwise identical either way.
        let sa2 = run_stats(&mut a, TEN_DAYS, Some(64));
        let sb2 = run_stats(&mut b, TEN_DAYS, None);
        sa.calls += sa2.calls;
        sa.events += sa2.events;
        sa.clamping += sa2.clamping;
        sa.max_advance = sa.max_advance.max(sa2.max_advance);
        sb.calls += sb2.calls;
        sb.events += sb2.events;
        sb.clamping += sb2.clamping;
        sb.max_advance = sb.max_advance.max(sb2.max_advance);
        assert_eq!(a.clock().ticks(), TEN_DAYS);
        assert_eq!(b.clock().ticks(), TEN_DAYS);
        // The mask is genuinely populated: vessel 2 is bubble-resident
        // in both runs at the end.
        assert_eq!(a.in_bubble_ids(), b.in_bubble_ids());
        assert_eq!(a.in_bubble_ids(), BTreeSet::from([2u64]));
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
        // §7.1's masked comparison — a NON-empty mask this time: the
        // mid-run bubble resident is excluded through the documented
        // `in_bubble_ids` seam, so the equivalence claim is exercised
        // where the plan scopes it ("identical except bubble contents").
        assert_eq!(
            a.state_hash_masked(&a.in_bubble_ids()).unwrap(),
            b.state_hash_masked(&b.in_bubble_ids()).unwrap(),
            "×1 vs ×10⁵ state hashes must be identical (§2.6.1)"
        );
        // The stronger statement also holds here: per-tick bubble
        // stepping is partition-independent, so the unmasked hashes —
        // bubble contents included — are bitwise identical too.
        assert_eq!(a.state_hash().unwrap(), b.state_hash().unwrap());
        assert_eq!(a.command_log(), b.command_log());
        let va = &a.vessels[&VesselId(1)];
        assert_eq!(va.orbit().unwrap().parent, BodyId::from("sol"));
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
        assert_eq!(
            w.vessels[&VesselId(0)].orbit().unwrap().parent,
            BodyId::from("m")
        );
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
        World::restore(PatchedConics::new(BTreeMap::new()), None, good()).unwrap();
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
            World::restore(PatchedConics::new(BTreeMap::new()), None, s),
            Err(SimError::Fatal(_))
        ));
        // Duplicate (tick, seq).
        let mut s = good();
        s.event_heap = vec![ev(9, 0), ev(9, 0)];
        s.next_event_seq = 2;
        assert!(matches!(
            World::restore(PatchedConics::new(BTreeMap::new()), None, s),
            Err(SimError::Fatal(_))
        ));
        // seq not below next_event_seq.
        let mut s = good();
        s.event_heap = vec![ev(9, 3)];
        s.next_event_seq = 3;
        assert!(matches!(
            World::restore(PatchedConics::new(BTreeMap::new()), None, s),
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
            World::restore(PatchedConics::new(BTreeMap::new()), None, s),
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
            World::restore(PatchedConics::new(BTreeMap::new()), None, s),
            Err(SimError::Fatal(_))
        ));
        // next_vessel_id behind the live arena.
        let mut s = good();
        s.vessels.insert(
            VesselId(3),
            Vessel {
                id: VesselId(3),
                craft: None,
                aero_mask: csp_phys::ConfigMask::ALL,
                state: VesselState::OnRails(OrbitState {
                    parent: BodyId::from("p"),
                    elements: leo_elements(),
                    epoch: SimTime(0),
                }),
            },
        );
        s.next_vessel_id = 3;
        assert!(matches!(
            World::restore(PatchedConics::new(BTreeMap::new()), None, s),
            Err(SimError::Fatal(_))
        ));
        // Tracked set outside the arena; zero budget.
        let mut s = good();
        s.tracked_vessels.insert(VesselId(1));
        assert!(matches!(
            World::restore(PatchedConics::new(BTreeMap::new()), None, s.clone()),
            Err(SimError::Fatal(_))
        ));
        s.tracked_vessels.clear();
        s.event_budget = 0;
        assert!(matches!(
            World::restore(PatchedConics::new(BTreeMap::new()), None, s),
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

    // ==================== M2-C: the physics bubble ====================
    //
    // Terrin's §3.0 numbers as an `Environment` literal (the fixture the
    // physics tests share): the spinning datum (21 600 s day), the 70 km
    // atmosphere, and the §2.9 default solver/settle tunables.
    fn test_env(spin: bool) -> csp_phys::Environment {
        csp_phys::Environment {
            body_center: DVec3::ZERO,
            radius_m: 600_000.0,
            mu_m3_per_s2: 3.5316e12,
            body_spin: if spin {
                DVec3::new(0.0, 0.0, 2.0 * core::f64::consts::PI / 21_600.0)
            } else {
                DVec3::ZERO
            },
            atmosphere: Some(csp_data::AtmosphereDef {
                sea_level_pressure_pa: 101_325.0,
                scale_height_m: 5_600.0,
                top_m: 70_000.0,
                molar_mass_kg_per_mol: 0.028_97,
                heat_capacity_ratio: 1.40,
                temperature_knots: vec![
                    csp_data::TemperatureKnot {
                        altitude_m: 0.0,
                        temperature_k: 288.15,
                    },
                    csp_data::TemperatureKnot {
                        altitude_m: 70_000.0,
                        temperature_k: 200.15,
                    },
                ],
            }),
            settle_v: 0.05,
            settle_w: 0.05,
            settle_ticks: 128,
            friction_mu: 0.5,
            drag_cd: 1.0,
        }
    }

    /// A one-part box craft — a `mass_kg` box with an engine on its −z
    /// face thrusting along +z (no engine when `thrust_n` is 0).
    fn box_craft(mass_kg: f64, half: DVec3, thrust_n: f64) -> CraftDef {
        let mut graph = csp_phys::PartGraph::new(csp_phys::PartNodeId(0));
        graph.nodes.insert(
            csp_phys::PartNodeId(0),
            csp_phys::PartNode::new(None, csp_phys::PartTransform::IDENTITY, 0),
        );
        CraftDef {
            graph,
            parts: vec![csp_phys::WeldPart {
                mass_kg,
                com: DVec3::ZERO,
                inertia: csp_phys::SymMat3::solid_box(mass_kg, half),
                collider: csp_phys::ColliderShape::Box { half_extents: half },
                force_sites: if thrust_n > 0.0 {
                    vec![csp_phys::ForceSite::new(
                        DVec3::new(0.0, 0.0, -half.z),
                        DVec3::Z,
                        csp_phys::ForceSiteKind::Thrust,
                        thrust_n,
                    )]
                } else {
                    Vec::new()
                },
                aero: csp_data::AeroDef::constant(1.0, 0.0, 1.0),
            }],
        }
    }

    fn physics_world(spin: bool) -> World {
        World::with_physics(
            test_system(),
            PhysicsConfig {
                environment: test_env(spin),
                dominant: BodyId::from("p"),
            },
            DEFAULT_MAX_EVENTS_PER_FRAME,
        )
    }

    /// Spawn `craft` — drains at tick 0; asserts the vessel is resident.
    fn spawn_physical(w: &mut World, cmd: SimCommand) {
        w.enqueue(SourceClass::Ui, 0, cmd);
        w.step_frame(1).unwrap();
        assert!(matches!(
            w.vessels()[&VesselId(0)].state,
            VesselState::InBubble(_)
        ));
    }

    /// Enqueue a `SpawnPhysicalVessel` for a ballistic orbiter 200 km
    /// over Terrin — above `top_m`, circular, never latching: the
    /// equivalence test's mid-run `InBubble` resident. The spawn drains
    /// at the caller's next tick boundary.
    fn enqueue_orbiter(w: &mut World) {
        let hi = DVec3::new(TERRIN_R + 200_000.0, 0.0, 0.0);
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::SpawnPhysicalVessel {
                craft: box_craft(1_000.0, DVec3::new(0.5, 0.5, 0.5), 0.0),
                pos: hi,
                rot: csp_core::DQuat::IDENTITY,
                vel: DVec3::new(0.0, libm::sqrt(3.5316e12 / hi.norm()), 0.0),
                ang_vel: DVec3::ZERO,
            },
        );
    }

    const TERRIN_R: f64 = 600_000.0;
    /// The surface co-rotation velocity at `p` — `ω × (p − center)`.
    fn surface_vel(p: DVec3, env: &csp_phys::Environment) -> DVec3 {
        env.body_spin.cross(p - env.body_center)
    }
    /// Body +z → world +x (up at the +x pad site).
    fn pad_rot() -> csp_core::DQuat {
        csp_core::DQuat::from_axis_angle(DVec3::Y, core::f64::consts::FRAC_PI_2)
    }

    /// §2.4: a physical spawn welds its craft into a bubble-resident
    /// `WeldedBody`; gravity alone pulls it down at ~g.
    #[test]
    fn physical_spawn_welds_and_falls() {
        let mut w = physics_world(false);
        let pos = DVec3::new(TERRIN_R + 100.0, 0.0, 0.0);
        spawn_physical(
            &mut w,
            SimCommand::SpawnPhysicalVessel {
                craft: box_craft(5_000.0, DVec3::new(0.5, 0.5, 1.5), 0.0),
                pos,
                rot: pad_rot(),
                vel: DVec3::ZERO,
                ang_vel: DVec3::ZERO,
            },
        );
        let v = &w.vessels()[&VesselId(0)];
        assert_eq!(v.craft.as_ref().unwrap().parts.len(), 1);
        let body = v.body().unwrap();
        // The spawn drained at tick 0; the body then ticked once —
        // ~2.4 mm of fall (g·dt²/2 ≈ 9.81/4096) already accrued.
        assert!((body.pos.x - pos.x).abs() < 0.01);
        assert_eq!(body.pos.y, 0.0);
        assert_eq!(body.mass_kg, 5_000.0);
        // 65 ticks ≈ 1.016 s of free fall: v ≈ −9.96·x̂, drop ≈ 5.1 m.
        w.step_frame(65).unwrap();
        let body = w.vessels()[&VesselId(0)].body().unwrap();
        assert!((body.vel.x + 9.96).abs() < 0.02);
        assert!((body.pos.x - (pos.x - 5.1)).abs() < 0.2);
    }

    /// A two-node craft: payload box (root, node 1) + a big thin
    /// fairing plate above it (node 2) — enough to make the staged
    /// and deployed tables measurably different.
    fn two_node_craft() -> CraftDef {
        let mut graph = csp_phys::PartGraph::new(csp_phys::PartNodeId(1));
        graph.insert(
            csp_phys::PartNodeId(1),
            csp_phys::PartNode::new(None, csp_phys::PartTransform::IDENTITY, 0),
        );
        graph.insert(
            csp_phys::PartNodeId(2),
            csp_phys::PartNode::new(
                Some(csp_phys::PartNodeId(1)),
                csp_phys::PartTransform::new(DVec3::new(0.0, 0.0, 1.0), csp_core::DQuat::IDENTITY),
                1,
            ),
        );
        let payload = csp_phys::WeldPart {
            mass_kg: 500.0,
            com: DVec3::ZERO,
            inertia: csp_phys::SymMat3::solid_box(500.0, DVec3::new(0.5, 0.5, 0.5)),
            collider: csp_phys::ColliderShape::Box {
                half_extents: DVec3::new(0.5, 0.5, 0.5),
            },
            force_sites: vec![],
            aero: csp_data::AeroDef::constant(0.8, 0.2, 1.0),
        };
        let plate = csp_phys::WeldPart {
            mass_kg: 50.0,
            com: DVec3::ZERO,
            inertia: csp_phys::SymMat3::solid_box(50.0, DVec3::new(1.0, 1.0, 0.02)),
            collider: csp_phys::ColliderShape::Box {
                half_extents: DVec3::new(1.0, 1.0, 0.02),
            },
            force_sites: vec![],
            aero: csp_data::AeroDef::constant(0.8, 0.2, 1.0),
        };
        CraftDef {
            graph,
            parts: vec![payload, plate],
        }
    }

    /// §2.4's staged-vs-deployed exit row, sim-side: two identical
    /// falling vessels — one keeps its fairing plate, one jettisons
    /// it. Mass leaves with the plate and the velocity profiles
    /// diverge measurably.
    #[test]
    fn jettison_staged_vs_deployed_diverges() {
        let mut w = physics_world(false);
        let pos = DVec3::new(TERRIN_R + 30_000.0, 0.0, 0.0);
        let cmd = |craft| SimCommand::SpawnPhysicalVessel {
            craft,
            pos,
            rot: csp_core::DQuat::IDENTITY,
            vel: DVec3::new(-300.0, 0.0, 0.0),
            ang_vel: DVec3::ZERO,
        };
        spawn_physical(&mut w, cmd(two_node_craft()));
        w.enqueue(SourceClass::Ui, 0, cmd(two_node_craft()));
        w.step_frame(w.clock().ticks() + 1).unwrap();
        let (m0, m1) = (
            w.vessels()[&VesselId(0)].body().unwrap().mass_kg,
            w.vessels()[&VesselId(1)].body().unwrap().mass_kg,
        );
        assert_eq!(m0, m1);
        // Spawn attached aero tables on both (atmosphere world).
        assert!(w.vessels()[&VesselId(0)].body().unwrap().aero.is_some());
        // Jettison node 2 on vessel 1.
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::JettisonPart {
                vessel: VesselId(1),
                node: csp_phys::PartNodeId(2),
            },
        );
        w.step_frame(w.clock().ticks() + 1).unwrap();
        let v1 = &w.vessels()[&VesselId(1)];
        assert_eq!(v1.body().unwrap().mass_kg, 500.0);
        assert!(v1.craft.as_ref().unwrap().graph.nodes.len() == 1);
        // Fresh table for the reduced craft.
        let t = v1.body().unwrap().aero.as_ref().unwrap();
        assert_eq!(t.mask, csp_phys::ConfigMask::ALL);
        // ~50 s of flight — velocities measurably diverge.
        w.step_frame(w.clock().ticks() + 3200).unwrap();
        let (v0, v1) = (
            w.vessels()[&VesselId(0)].body().unwrap().vel,
            w.vessels()[&VesselId(1)].body().unwrap().vel,
        );
        let dv = (v0 - v1).norm();
        assert!(dv > 1.0, "staged/deployed divergence {dv} m/s");
    }

    /// `InstallAeroTables` round trip: build sim-side, install via
    /// the command path, reject a mismatched graph hash.
    #[test]
    fn install_aero_tables_accepts_and_binds() {
        let mut w = physics_world(false);
        let pos = DVec3::new(TERRIN_R + 30_000.0, 0.0, 0.0);
        spawn_physical(
            &mut w,
            SimCommand::SpawnPhysicalVessel {
                craft: box_craft(1_000.0, DVec3::new(0.5, 0.5, 0.5), 0.0),
                pos,
                rot: csp_core::DQuat::IDENTITY,
                vel: DVec3::ZERO,
                ang_vel: DVec3::ZERO,
            },
        );
        let v = &w.vessels()[&VesselId(0)];
        let craft = v.craft.as_ref().unwrap();
        let body = v.body().unwrap();
        // gh = 0 cannot match the real craft hash → install rejects.
        let gh = 0u64;
        let mut bad = csp_phys::build_aero_tables(
            body,
            &craft.graph,
            &craft.parts,
            csp_phys::ConfigMask::ALL,
            gh,
        )
        .unwrap();
        bad.graph_hash = 0xDEAD;
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::InstallAeroTables {
                vessel: VesselId(0),
                mask: csp_phys::ConfigMask::ALL,
                tables: bad,
            },
        );
        let rep = w.step_frame(w.clock().ticks() + 1).unwrap();
        assert!(
            rep.rejected
                .iter()
                .any(|(_, e)| *e == SimError::Recoverable("install-aero: graph hash mismatch")),
            "{:?}",
            rep.rejected
        );
    }

    /// §2.9 reentry check via the table model: a capsule dropped at
    /// high Mach inside the atmosphere decelerates, and its `Cm_α`
    /// sign matches the nose-heavy-stable/tail-heavy-unstable
    /// expectation covered in `aero::cm_alpha_sign_tracks_stability`.
    #[test]
    fn table_aero_reentry_decelerates() {
        let mut w = physics_world(false);
        let pos = DVec3::new(TERRIN_R + 25_000.0, 0.0, 0.0);
        spawn_physical(
            &mut w,
            SimCommand::SpawnPhysicalVessel {
                craft: box_craft(2_000.0, DVec3::new(0.6, 0.6, 0.6), 0.0),
                pos,
                rot: csp_core::DQuat::IDENTITY,
                vel: DVec3::new(-2_000.0, 0.0, 0.0),
                ang_vel: DVec3::ZERO,
            },
        );
        let v0 = w.vessels()[&VesselId(0)].body().unwrap().vel.norm();
        w.step_frame(w.clock().ticks() + 320).unwrap(); // ~5 s
        let body = w.vessels()[&VesselId(0)].body().unwrap();
        let v1 = body.vel.norm();
        assert!(v1 < v0, "reentry should decelerate: {v0} -> {v1}");
        // And it kept falling — drag slowed, never reversed it.
        assert!(body.pos.x < pos.x);
    }

    /// `SpawnPhysicalVessel` on a physics-free world is a Recoverable
    /// rejection — the §2.2.1 rejection channel, not a panic.
    #[test]
    fn physical_spawn_rejects_without_physics() {
        let mut w = World::with_system(test_system());
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::SpawnPhysicalVessel {
                craft: box_craft(1_000.0, DVec3::new(0.5, 0.5, 0.5), 0.0),
                pos: DVec3::ZERO,
                rot: csp_core::DQuat::IDENTITY,
                vel: DVec3::ZERO,
                ang_vel: DVec3::ZERO,
            },
        );
        let report = w.step_frame(1).unwrap();
        assert_eq!(report.rejected.len(), 1);
        assert!(w.vessels.is_empty());
    }

    /// Throttle drives the engine's thrust site: a 5 t craft at full
    /// throttle on 200 kN climbs against gravity — and the rails
    /// rejection channel reads exactly per §2.6.2.
    #[test]
    fn throttle_drives_thrust_and_clamps() {
        let mut w = physics_world(false);
        let pos = DVec3::new(TERRIN_R + 200.0, 0.0, 0.0);
        spawn_physical(
            &mut w,
            SimCommand::SpawnPhysicalVessel {
                craft: box_craft(5_000.0, DVec3::new(0.5, 0.5, 1.5), 200_000.0),
                pos,
                rot: pad_rot(),
                vel: DVec3::ZERO,
                ang_vel: DVec3::ZERO,
            },
        );
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::SetThrottle {
                vessel: VesselId(0),
                throttle: 2.0, // over 1 — clamps
            },
        );
        w.step_frame(66).unwrap();
        let body = w.vessels()[&VesselId(0)].body().unwrap();
        assert_eq!(body.throttle, 1.0);
        // Net accel = 40 − 9.81 ≈ +30 m/s².
        assert!(body.vel.x > 25.0);
        // A rails vessel has no throttle channel.
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::SpawnVessel {
                parent: BodyId::from("p"),
                elements: leo_elements(),
            },
        );
        w.step_frame(67).unwrap();
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::SetThrottle {
                vessel: VesselId(1),
                throttle: 0.5,
            },
        );
        let report = w.step_frame(68).unwrap();
        assert_eq!(report.rejected.len(), 1);
    }

    /// §2.6.2's real refusal predicate — each named case.
    #[test]
    fn rails_warp_refusal_real_cases() {
        let mut w = physics_world(false);
        // A physics world with an empty bubble refuses nothing.
        assert_eq!(w.rails_warp_refusal(), None);
        let pos = DVec3::new(TERRIN_R + 2.0, 0.0, 0.0);
        spawn_physical(
            &mut w,
            SimCommand::SpawnPhysicalVessel {
                craft: box_craft(5_000.0, DVec3::new(0.5, 0.5, 1.5), 200_000.0),
                pos,
                rot: pad_rot(),
                vel: DVec3::ZERO,
                ang_vel: DVec3::ZERO,
            },
        );
        // Sitting on the pad after touchdown: unsettled contact AND in
        // atmosphere — both hold; the predicate's documented order
        // reports contact first.
        w.step_frame(60).unwrap();
        assert_eq!(
            w.rails_warp_refusal().as_deref(),
            Some("bubble-resident vessel 0 in unsettled contact")
        );
        // Under thrust → the thrust reason regardless.
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::SetThrottle {
                vessel: VesselId(0),
                throttle: 1.0,
            },
        );
        w.step_frame(61).unwrap();
        assert_eq!(
            w.rails_warp_refusal().as_deref(),
            Some("bubble-resident vessel 0 under thrust")
        );
        // Ballistic in vacuum above atmo top → no refusal.
        let mut w = physics_world(true);
        let hi = DVec3::new(TERRIN_R + 200_000.0, 0.0, 0.0);
        let v_circ = libm::sqrt(3.5316e12 / hi.norm());
        spawn_physical(
            &mut w,
            SimCommand::SpawnPhysicalVessel {
                craft: box_craft(1_000.0, DVec3::new(0.5, 0.5, 0.5), 0.0),
                pos: hi,
                rot: csp_core::DQuat::IDENTITY,
                vel: DVec3::new(0.0, v_circ, 0.0),
                ang_vel: DVec3::ZERO,
            },
        );
        assert_eq!(w.rails_warp_refusal(), None);
        // But a body inside the atmosphere refuses.
        let mut w = physics_world(false);
        let low = DVec3::new(TERRIN_R + 30_000.0, 0.0, 0.0);
        spawn_physical(
            &mut w,
            SimCommand::SpawnPhysicalVessel {
                craft: box_craft(1_000.0, DVec3::new(0.5, 0.5, 0.5), 0.0),
                pos: low,
                rot: csp_core::DQuat::IDENTITY,
                vel: DVec3::new(0.0, 100.0, 0.0),
                ang_vel: DVec3::ZERO,
            },
        );
        assert_eq!(
            w.rails_warp_refusal().as_deref(),
            Some("bubble-resident vessel 0 in atmosphere")
        );
    }

    /// §2.6.4 the acceptance case: a 20 t lander rests on the spinning
    /// Terrin datum, settles, and latches — the vessel leaves the
    /// bubble into `SurfaceLatched`, the stored body-fixed pose holds,
    /// and the observation spans the FULL 10 000 ticks of the exit
    /// criterion. The literal "‖v‖ < 1e-6 for 10 000 ticks" channel is
    /// unreachable post-latch by design — a latched vessel has no body
    /// and therefore no velocity at all — so the criterion reads on the
    /// latched record: captured at latch (~tick 130) and asserted
    /// tick-invariant through tick 10 000, which is exactly what
    /// "resting" means under surface rails.
    #[test]
    fn lander_20t_latches_and_hash_freezes() {
        let mut w = physics_world(true);
        let pos = DVec3::new(TERRIN_R + 1.51, 0.0, 0.0);
        spawn_physical(
            &mut w,
            SimCommand::SpawnPhysicalVessel {
                craft: box_craft(20_000.0, DVec3::new(1.5, 1.5, 1.5), 0.0),
                pos,
                rot: pad_rot(),
                vel: surface_vel(pos, &test_env(true)),
                ang_vel: DVec3::ZERO,
            },
        );
        // Latch lands early — observed at tick 131 in this fixture (a
        // few ticks of descent + the 128-tick settle countdown after
        // first contact).
        let mut latch_tick = None;
        let mut rest_v_rel = f64::INFINITY;
        for i in 0..10_000u64 {
            w.step_frame(i + 1).unwrap();
            if matches!(
                w.vessels()[&VesselId(0)].state,
                VesselState::SurfaceLatched(_)
            ) {
                latch_tick = Some(i + 1);
                break;
            }
            let b = w.vessels()[&VesselId(0)].body().unwrap();
            let v_rel = b.vel - surface_vel(b.com_world(), &test_env(true));
            rest_v_rel = v_rel.norm();
            // The settle gate asserts once the counter is live — the
            // initial touchdown transient (impact ~0.15 m/s) precedes it.
            if b.settle.settled_ticks > 0 {
                assert!(v_rel.norm() < 0.05, "tick {i}: v_rel={}", v_rel.norm());
            }
        }
        let latch_tick = latch_tick.expect("lander never latched");
        assert!(latch_tick < 10_000, "latched at {latch_tick}");
        // The resting residual on the solved contact — the surface-
        // relative speed just before latch. Measured ≈1.6e-4 m/s here:
        // 300× under the 0.05 settle gate (the post-latch state carries
        // no velocity at all — the ‖v‖ < 1e-6 acceptance reads on the
        // latched record, whose velocity is identically zero).
        assert!(rest_v_rel < 1e-3, "rest residual {rest_v_rel}");
        // The pose record: equatorial site → lat ≈ 0; the body origin
        // rests 1.5 m (collider half-extent) above the datum.
        let VesselState::SurfaceLatched(pose) = &w.vessels()[&VesselId(0)].state else {
            panic!("not latched");
        };
        assert_eq!(pose.body, BodyId::from("p"));
        assert!((pose.altitude_m - 1.5).abs() < 0.05);
        assert!(pose.latitude_rad.abs() < 1e-6);
        // Rails warp is free again — the latched vessel left the bubble.
        assert_eq!(w.rails_warp_refusal(), None);
        // The full 10 000-tick observation, literally: the latched
        // record is a pure function of the stored pose — tick-invariant
        // by construction — so stepping the remaining ~9 870 ticks and
        // asserting the record never moves is nearly free (no bubble
        // residents remain; each call is a bare clock advance), and it
        // meets the letter of "rests for 10 000 ticks": the lander is
        // observed resting from spawn through tick 10 000.
        let latched_state = w.vessels()[&VesselId(0)].state.clone();
        for i in latch_tick..10_000 {
            w.step_frame(i + 1).unwrap();
            assert_eq!(
                w.vessels()[&VesselId(0)].state,
                latched_state,
                "latched record moved at tick {}",
                i + 1
            );
        }
        // "constant hash once latched": the full hash still differs
        // across ticks because the clock is hashed — the honest check
        // is the tick-invariant record above plus replayed determinism
        // at the same tick: two restores of the latched snapshot stepped
        // identically hash identically.
        let snap = w.snapshot().unwrap();
        let physics = || {
            Some(PhysicsConfig {
                environment: test_env(true),
                dominant: BodyId::from("p"),
            })
        };
        let mut r1 = World::restore(test_system(), physics(), snap.clone()).unwrap();
        let mut r2 = World::restore(test_system(), physics(), snap).unwrap();
        let beyond = w.clock().ticks() + 64;
        r1.step_frame(beyond).unwrap();
        r2.step_frame(beyond).unwrap();
        assert_eq!(r1.state_hash().unwrap(), r2.state_hash().unwrap());
    }

    /// §2.6.4 exact unlatch: the stored pose is restored bit-for-bit —
    /// on the spinning datum the re-materialized body carries the
    /// co-rotation velocity `ω×r`, and a throttle command on a still-
    /// latched vessel unlatches first, per the plan's rule.
    #[test]
    fn unlatch_restores_pose_exactly() {
        let mut w = physics_world(true);
        let pos = DVec3::new(TERRIN_R + 1.51, 0.0, 0.0);
        spawn_physical(
            &mut w,
            SimCommand::SpawnPhysicalVessel {
                craft: box_craft(20_000.0, DVec3::new(1.5, 1.5, 1.5), 0.0),
                pos,
                rot: pad_rot(),
                vel: surface_vel(pos, &test_env(true)),
                ang_vel: DVec3::ZERO,
            },
        );
        for i in 0..10_000u64 {
            w.step_frame(i + 1).unwrap();
            if matches!(
                w.vessels()[&VesselId(0)].state,
                VesselState::SurfaceLatched(_)
            ) {
                break;
            }
        }
        let VesselState::SurfaceLatched(pose) = w.vessels()[&VesselId(0)].state.clone() else {
            panic!("not latched");
        };
        // Sit latched — the record does not move.
        let t_unlatch = w.clock().ticks() + 500;
        w.step_frame(t_unlatch).unwrap();
        assert_eq!(
            w.vessels()[&VesselId(0)].state,
            VesselState::SurfaceLatched(pose.clone())
        );
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::Unlatch {
                vessel: VesselId(0),
            },
        );
        w.step_frame(t_unlatch + 1).unwrap();
        let body = w.vessels()[&VesselId(0)].body().unwrap();
        let rel = body.pos - DVec3::ZERO;
        let r = rel.norm();
        assert!((r - TERRIN_R - pose.altitude_m).abs() < 0.02);
        // Inertial velocity ≈ the surface co-rotation — the state a
        // body-fixed object actually has — minus one tick of gravity
        // accrued after the restore (g·dt ≈ 0.153 m/s).
        let v_surf = surface_vel(body.com_world(), &test_env(true));
        assert!((body.vel - v_surf).norm() < 0.2);
        // A bare Unlatch on an already-InBubble vessel is a typed
        // rejection (§2.2.1's rejected channel), never a silent no-op —
        // the command is meaningful only from `SurfaceLatched`.
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::Unlatch {
                vessel: VesselId(0),
            },
        );
        let report = w.step_frame(w.clock().ticks() + 1).unwrap();
        assert_eq!(report.rejected.len(), 1);
        assert!(matches!(report.rejected[0].1, SimError::Recoverable(_)));
        // A throttle command on a still-latched vessel unlatches first —
        // §2.6.4's rule — then applies.
        let mut w2 = physics_world(true);
        spawn_physical(
            &mut w2,
            SimCommand::SpawnPhysicalVessel {
                craft: box_craft(20_000.0, DVec3::new(1.5, 1.5, 1.5), 200_000.0),
                pos,
                rot: pad_rot(),
                vel: surface_vel(pos, &test_env(true)),
                ang_vel: DVec3::ZERO,
            },
        );
        for i in 0..10_000u64 {
            w2.step_frame(i + 1).unwrap();
            if matches!(
                w2.vessels()[&VesselId(0)].state,
                VesselState::SurfaceLatched(_)
            ) {
                break;
            }
        }
        w2.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::SetThrottle {
                vessel: VesselId(0),
                throttle: 1.0,
            },
        );
        w2.step_frame(w2.clock().ticks() + 1).unwrap();
        let b = w2.vessels()[&VesselId(0)].body().unwrap();
        assert_eq!(b.throttle, 1.0);
    }

    /// Bubble→rails→bubble: `ToRails` stamps elements from the
    /// osculating state; `ToBubble` re-materializes at the advanced
    /// conic's position — and the dead SOI events are dropped.
    #[test]
    fn bubble_rails_bubble_roundtrip() {
        let mut w = physics_world(true);
        let hi = DVec3::new(TERRIN_R + 100_000.0, 0.0, 0.0);
        let v_circ = libm::sqrt(3.5316e12 / hi.norm());
        spawn_physical(
            &mut w,
            SimCommand::SpawnPhysicalVessel {
                craft: box_craft(5_000.0, DVec3::new(0.5, 0.5, 1.5), 100_000.0),
                pos: hi,
                rot: csp_core::DQuat::IDENTITY,
                vel: DVec3::new(0.0, v_circ, 0.0),
                ang_vel: DVec3::ZERO,
            },
        );
        w.step_frame(129).unwrap();
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::ToRails {
                vessel: VesselId(0),
            },
        );
        w.step_frame(130).unwrap();
        let orbit = w.vessels()[&VesselId(0)].orbit().unwrap().clone();
        let a = orbit.elements.semi_major_axis_m;
        assert!((a - hi.norm()).abs() / a < 1e-3);
        assert!(orbit.elements.eccentricity < 0.01);
        // Advance on rails (instant jump), then come back.
        w.enqueue(SourceClass::Ui, 0, SimCommand::AdvanceTo(130 + 60_000));
        w.step_frame(131).unwrap();
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::ToBubble {
                vessel: VesselId(0),
            },
        );
        w.step_frame(w.clock().ticks() + 1).unwrap();
        let body = w.vessels()[&VesselId(0)].body().unwrap();
        assert!((body.pos.norm() - a).abs() / a < 2e-2);
        // Weld defaults, not a reconstructed attitude (command doc).
        assert_eq!(body.rot, csp_core::DQuat::IDENTITY);
        assert_eq!(body.ang_vel, DVec3::ZERO);
        assert_eq!(body.throttle, 0.0);
        // The dead SOI events were dropped — none name the vessel.
        assert!(
            w.event_heap
                .iter()
                .all(|e| !event_names_vessel(&e.kind, &VesselId(0)))
        );
    }

    /// §2.6.2's `ToRails` refusals: atmosphere, contact, intersecting
    /// conic — all Recoverable, vessel untouched.
    #[test]
    fn to_rails_refusals() {
        let mut w = physics_world(false);
        let low = DVec3::new(TERRIN_R + 30_000.0, 0.0, 0.0);
        spawn_physical(
            &mut w,
            SimCommand::SpawnPhysicalVessel {
                craft: box_craft(1_000.0, DVec3::new(0.5, 0.5, 0.5), 0.0),
                pos: low,
                rot: csp_core::DQuat::IDENTITY,
                vel: DVec3::new(0.0, 500.0, 0.0),
                ang_vel: DVec3::ZERO,
            },
        );
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::ToRails {
                vessel: VesselId(0),
            },
        );
        let report = w.step_frame(2).unwrap();
        assert_eq!(report.rejected.len(), 1);
        // A pad-resting body: contact refuses.
        let mut w = physics_world(false);
        let pad = DVec3::new(TERRIN_R + 0.51, 0.0, 0.0);
        spawn_physical(
            &mut w,
            SimCommand::SpawnPhysicalVessel {
                craft: box_craft(1_000.0, DVec3::new(0.5, 0.5, 0.5), 0.0),
                pos: pad,
                rot: pad_rot(),
                vel: DVec3::ZERO,
                ang_vel: DVec3::ZERO,
            },
        );
        w.step_frame(65).unwrap();
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::ToRails {
                vessel: VesselId(0),
            },
        );
        let report = w.step_frame(66).unwrap();
        assert_eq!(report.rejected.len(), 1);
        // Suborbital above atmo: the conic dips below the surface → refused.
        let mut w = physics_world(false);
        let hi = DVec3::new(TERRIN_R + 90_000.0, 0.0, 0.0);
        spawn_physical(
            &mut w,
            SimCommand::SpawnPhysicalVessel {
                craft: box_craft(1_000.0, DVec3::new(0.5, 0.5, 0.5), 0.0),
                pos: hi,
                rot: csp_core::DQuat::IDENTITY,
                vel: DVec3::new(0.0, 300.0, 0.0),
                ang_vel: DVec3::ZERO,
            },
        );
        w.step_frame(2).unwrap();
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::ToRails {
                vessel: VesselId(0),
            },
        );
        let report = w.step_frame(3).unwrap();
        assert_eq!(report.rejected.len(), 1);
    }

    /// §7.4: a live-contact (unsettled) bubble body refuses the save
    /// boundary; a latched vessel does not.
    #[test]
    fn save_refused_with_live_contacts() {
        let mut w = physics_world(false);
        let pad = DVec3::new(TERRIN_R + 0.51, 0.0, 0.0);
        spawn_physical(
            &mut w,
            SimCommand::SpawnPhysicalVessel {
                craft: box_craft(1_000.0, DVec3::new(0.5, 0.5, 0.5), 0.0),
                pos: pad,
                rot: pad_rot(),
                vel: DVec3::ZERO,
                ang_vel: DVec3::ZERO,
            },
        );
        w.step_frame(65).unwrap();
        assert!(matches!(w.snapshot(), Err(SimError::SaveBlocking(_))));
        // Let it settle → latch → the boundary opens.
        for i in 0..10_000u64 {
            w.step_frame(65 + i + 1).unwrap();
            if matches!(
                w.vessels()[&VesselId(0)].state,
                VesselState::SurfaceLatched(_)
            ) {
                break;
            }
        }
        w.snapshot().unwrap();
    }

    /// Snapshot/restore mid-flight: the welded body is re-created by
    /// re-welding the stored craft (§7.3) and the run continues
    /// bit-identically — the re-weld-exactness rule, exercised.
    #[test]
    fn snapshot_restore_mid_flight_and_latched() {
        let mut w = physics_world(false);
        let pos = DVec3::new(TERRIN_R + 1.51, 0.0, 0.0);
        spawn_physical(
            &mut w,
            SimCommand::SpawnPhysicalVessel {
                craft: box_craft(20_000.0, DVec3::new(1.5, 1.5, 1.5), 0.0),
                pos,
                rot: pad_rot(),
                vel: DVec3::ZERO,
                ang_vel: DVec3::ZERO,
            },
        );
        let snap = w.snapshot().unwrap();
        w.step_frame(65).unwrap();
        let h_after = w.state_hash().unwrap();
        let mut w2 = World::restore(
            test_system(),
            Some(PhysicsConfig {
                environment: test_env(false),
                dominant: BodyId::from("p"),
            }),
            snap.clone(),
        )
        .unwrap();
        w2.step_frame(65).unwrap();
        assert_eq!(w2.state_hash().unwrap(), h_after);
        // Restore validation: an InBubble vessel without physics is
        // corrupt input, and without a craft it cannot re-materialize.
        // (The world has landed by now — §7.4's live-contact refusal
        // blocks a fresh snapshot, so the mid-flight snapshot stands
        // in for the corrupt-input checks.)
        assert!(matches!(
            World::restore(test_system(), None, snap.clone()),
            Err(SimError::Fatal(_))
        ));
        let mut broken = snap;
        for v in broken.vessels.values_mut() {
            v.craft = None;
        }
        assert!(matches!(
            World::restore(
                test_system(),
                Some(PhysicsConfig {
                    environment: test_env(false),
                    dominant: BodyId::from("p"),
                }),
                broken
            ),
            Err(SimError::Fatal(_))
        ));
    }

    /// §2.6.1's documented mask is real: an in-bubble vessel's whole
    /// entry — craft, welded body, everything — is excluded by
    /// `bubble_mask`, so two worlds whose bubble residents *differ*
    /// (different craft, different pose) hash identically under the
    /// mask, while the full hash distinguishes them.
    #[test]
    fn bubble_mask_excludes_in_bubble_vessels() {
        let spawn = |pos: DVec3, mass: f64| SimCommand::SpawnPhysicalVessel {
            craft: box_craft(mass, DVec3::new(0.5, 0.5, 1.5), 0.0),
            pos,
            rot: pad_rot(),
            vel: DVec3::ZERO,
            ang_vel: DVec3::ZERO,
        };
        let mut w1 = physics_world(false);
        spawn_physical(
            &mut w1,
            spawn(DVec3::new(TERRIN_R + 100.0, 0.0, 0.0), 5_000.0),
        );
        let mut w2 = physics_world(false);
        spawn_physical(
            &mut w2,
            spawn(DVec3::new(TERRIN_R + 5_000.0, 0.0, 0.0), 20_000.0),
        );
        // The full hash carries the bubble contents — different.
        assert_ne!(w1.state_hash().unwrap(), w2.state_hash().unwrap());
        // Masked with each world's own residents → identical.
        assert_eq!(
            w1.state_hash_masked(&w1.in_bubble_ids()).unwrap(),
            w2.state_hash_masked(&w2.in_bubble_ids()).unwrap()
        );
    }
}
