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
//! M0 scope is deliberately thin: the integer clock (§2.3), the stored warp
//! level (§2.6.3 — a ladder *rung*, never a multiplier), `AdvanceTo`
//! targets, and the §2.6.5 per-call advance budget
//! [`MAX_TICKS_PER_FRAME`]. Vessels, the event heap, and the rails arrive
//! with M1; the seam for them is this file's drain/apply/harness shape.

use std::collections::{BTreeMap, VecDeque};

use csp_core::{SimError, SimTime, StableDiscriminant, StateHasher};
use serde::{Deserialize, Serialize};

/// The §2.6.5 per-call advance budget: the maximum number of ticks a single
/// `step_frame` may advance — 64 ticks/s × 1080 sim-seconds (18 sim-minutes)
/// per call. A `step_frame` whose target lies further ahead returns a
/// [`AdvanceReport::partial`] report and the caller (frame loop or replay
/// harness) re-issues the same target — the budget bounds one call, never
/// the run. This is a *tick* budget; the §2.6.5/PLAN:548 2000-events-per-
/// frame processing budget lives in `data/balance.ron` and binds once the
/// M1 event heap exists.
pub const MAX_TICKS_PER_FRAME: u64 = 69_120;

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
/// golden-breaking change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SimCommand {
    /// Store a new warp ladder rung (§2.6.3). Stored state, in the hash;
    /// interpreting it as a multiplier is presentation's job.
    SetWarp(u32),
    /// Ask the sim to advance to `tick` — a *logged* advance target
    /// (§2.6.3: warp is issued as commands, so the §7.6 log is
    /// self-contained). Effective per-call target is `max(step target,
    /// every AdvanceTo arg drained at that boundary)`.
    AdvanceTo(u64),
}

/// What `step_frame` did (§2.6.2/§2.6.5): where the clock landed, what the
/// call's drains applied, and whether the call ran out of budget before
/// reaching the effective target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdvanceReport {
    /// The tick the clock reached after this call.
    pub reached_tick: u64,
    /// Commands this call's boundary drains stamped and applied — 0 on a
    /// zero-advance call, which is not a tick boundary at all (§2.2.1).
    pub commands_applied: u64,
    /// Reserved for the M1 event heap (§2.6.2's `events_processed`):
    /// always 0 at M0 — an honest field, never a placeholder count.
    pub events_processed: u64,
    /// `true` when [`MAX_TICKS_PER_FRAME`] split the advance and the
    /// caller must re-issue the same target — back-pressure, never a
    /// stall: a partial still advanced the clock (§2.6.5's Clamping
    /// class).
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
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorldSnapshot {
    /// `clock.ticks()` at snapshot time.
    pub clock_ticks: u64,
    /// The stored warp ladder rung.
    pub warp_level: u32,
    /// Per-source dequeue counters — restoring them is what lets a resumed
    /// world keep stamping the recorded `source_seq`s.
    pub source_seqs: BTreeMap<(SourceClass, u32), u64>,
}

/// The §2.13 one-tick shadow: everything one tick-boundary iteration of
/// `step_frame` can mutate, captured before the boundary runs so a `Fatal`
/// mid-tick restores the pre-tick state *completely* — `state_hash()`
/// after a rollback equals the pre-tick hash byte-for-byte.
///
/// The queue needs no deep copy: the drain records what it popped, and a
/// restore pushes each command back `push_front`-first in reverse drain
/// order, which rebuilds each source FIFO's production order exactly.
/// `seq_restore` plays the same trick for the counters: each entry is a
/// source's pre-increment value — `None` meaning the drain created the
/// counter, so a rollback removes it rather than writing a 0 that was
/// never there.
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
}

impl TickShadow {
    /// Snapshot the pre-tick state `step_frame` is about to mutate.
    fn capture(world: &World) -> Self {
        Self {
            clock: world.clock,
            warp_level: world.warp_level,
            log_len: world.command_log.len(),
            drained: Vec::new(),
            seq_restore: Vec::new(),
        }
    }

    /// Roll the world back to the captured pre-tick state — the whole
    /// state, so `state_hash()` is the pre-tick hash again (§2.13).
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
    }
}

/// The authoritative world. Owns the integer clock (§2.3), the stored warp
/// rung (§2.6.3), the command queue + log (§2.2.1, §7.6), and the
/// per-source stamp counters.
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
    /// commands back where they came from. Sources drain to empty FIFOs
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
}

impl World {
    /// A pristine world at epoch 0, warp ×1, no commands seen.
    pub fn new() -> Self {
        Self {
            clock: SimTime::ZERO,
            warp_level: 1,
            pending: BTreeMap::new(),
            command_log: Vec::new(),
            source_seqs: BTreeMap::new(),
        }
    }

    /// Rebuild a world from its §7.2 serialized set. The queue and log
    /// restart empty — the log is replayed *into* the world by the harness
    /// (§7.6), it is not part of the snapshot (see [`WorldSnapshot`]).
    pub fn restore(snapshot: WorldSnapshot) -> Self {
        Self {
            clock: SimTime(snapshot.clock_ticks),
            warp_level: snapshot.warp_level,
            pending: BTreeMap::new(),
            command_log: Vec::new(),
            source_seqs: snapshot.source_seqs,
        }
    }

    /// The §7.4 serialization-boundary refusal class, applied to the one
    /// M0 case that can violate it: undrained commands in flight. An
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
        Ok(WorldSnapshot {
            clock_ticks: self.clock.ticks(),
            warp_level: self.warp_level,
            source_seqs: self.source_seqs.clone(),
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

    /// §2.2.1's boundary drain at the current tick: pop every pending
    /// command in `(source_class, source_id)` key order — each source's
    /// FIFO in production order — stamp each with its source's next
    /// `source_seq`, apply it, and append it to the `command_log`.
    /// `SetWarp` stores the rung; each `AdvanceTo(t)` raises this call's
    /// effective target to `max(effective_target, t)`.
    ///
    /// Everything the drain mutates is recorded into `shadow` as it goes,
    /// so a `Fatal` (a `source_seq` counter overflow — §2.13's abort class)
    /// can restore the pre-tick state completely.
    fn drain_boundary(
        &mut self,
        effective_target: &mut u64,
        shadow: &mut TickShadow,
        commands_applied: &mut u64,
    ) -> Result<(), SimError> {
        let tick = self.clock.ticks();
        // BTreeMap iteration IS the §2.2.1 (class, source_id) order.
        for (&src, fifo) in &mut self.pending {
            if !fifo.is_empty() {
                // First touch of this source this tick — record the
                // counter the drain is about to move for rollback.
                shadow
                    .seq_restore
                    .push((src, self.source_seqs.get(&src).copied()));
            }
            while let Some(command) = fifo.pop_front() {
                shadow.drained.push((src, command));
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
                match command {
                    SimCommand::SetWarp(level) => self.warp_level = level,
                    SimCommand::AdvanceTo(t) => *effective_target = (*effective_target).max(t),
                }
                self.command_log.push((key, command));
                *commands_applied += 1;
            }
        }
        // Drained FIFOs go empty — drop them so the queue's emptiness is
        // literally the map's, the §7.4 invariant `check_serializable`
        // reads.
        self.pending.retain(|_, fifo| !fifo.is_empty());
        Ok(())
    }

    /// One frame's advance (§2.5 SimStep, §2.6.2's single advance
    /// function): `while clock.ticks() < effective_target { shadow; drain;
    /// advance; budget-check }`.
    ///
    /// - **Boundary drain** — once per *tick value*, gated on a real
    ///   advance being owed: a call whose `target` the clock has already
    ///   reached runs zero iterations — drains nothing, stamps nothing,
    ///   mutates nothing. That is what makes the `command_log`'s
    ///   `CommandKey`s strictly increasing by construction: each tick
    ///   value hosts at most one drain, and a drain's own key order is
    ///   `(source_class, source_id)` with per-source `source_seq`s in
    ///   production order.
    /// - **Advance** — each iteration moves the clock toward the
    ///   effective target, which drained `AdvanceTo` commands may have
    ///   raised; a stale `AdvanceTo` behind the clock is absorbed (the
    ///   clock never retreats). At most [`MAX_TICKS_PER_FRAME`] ticks per
    ///   call (§2.6.5); on exhaustion the loop breaks with
    ///   [`AdvanceReport::partial`] set and the caller re-issues.
    /// - **Rollback** — every iteration captures a [`TickShadow`] first
    ///   (§2.13's one-tick shadow copy); a `Fatal` mid-tick restores
    ///   clock, warp, log, queue, and counters to the pre-tick state.
    ///
    /// Errors are §2.13 `SimError`s propagated from the fallible
    /// primitives beneath (counter overflows), never panics.
    pub fn step_frame(&mut self, target: u64) -> Result<AdvanceReport, SimError> {
        let mut effective_target = target;
        let mut budget = MAX_TICKS_PER_FRAME;
        let mut commands_applied = 0u64;
        while self.clock.ticks() < effective_target {
            // §2.13: the shadow precedes every mutation this tick makes.
            let mut shadow = TickShadow::capture(self);
            if let Err(e) =
                self.drain_boundary(&mut effective_target, &mut shadow, &mut commands_applied)
            {
                shadow.restore(self);
                return Err(e);
            }
            // M0's advance is a jump — the event heap and the bubble
            // arrive with M1, so nothing splits t_next inside the
            // interval yet (§2.6.2's `min(target, peek_tick(), …)`).
            let advance = (effective_target - self.clock.ticks()).min(budget);
            if let Err(e) = self.clock.try_advance(advance) {
                shadow.restore(self);
                return Err(e);
            }
            budget -= advance;
            if budget == 0 {
                break; // §2.6.5 back-pressure — the caller re-issues.
            }
        }
        Ok(AdvanceReport {
            reached_tick: self.clock.ticks(),
            commands_applied,
            events_processed: 0,
            partial: self.clock.ticks() < effective_target,
        })
    }

    /// Canonical state hash (PLAN §7.1) — the T1 replay ground truth.
    /// The field walk IS the schema: reorder it and every golden reds by
    /// design (which is why this is a hand-written walk, never a derive).
    ///
    /// Walked: clock ticks, the stored warp rung, and the per-source
    /// dequeue counters — the state's *forward-looking* content. The
    /// `command_log` is deliberately NOT hashed: it is recorded history
    /// (the §7.6 artifact), and replay already proves it byte-for-byte via
    /// `StampMismatch` before the hash comparison is ever reached.
    ///
    /// Fallible per §2.13: a recorded `StateHasher` fault surfaces here.
    pub fn state_hash(&self) -> Result<[u8; 32], SimError> {
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
            r.enqueue(k.source_class, k.source_id, *cmd);
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
        let mut w = World::restore(WorldSnapshot {
            clock_ticks: 5,
            warp_level: 3,
            source_seqs: seqs.clone(),
        });
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
        let mut r = World::restore(snap);
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

    /// The full M0 shape: enqueue + drain + advance reproduce the trivial
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
}
