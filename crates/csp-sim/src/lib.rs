//! Headless authoritative simulation (PLAN §2.2).
//!
//! Plain owned data — no ECS, no wgpu, no winit, no rand facade, no `std::time`.
//! The ONLY way state changes is `step()` / `advance_to()` plus `SimCommand`s.

use csp_core::{HASH_SCHEMA_VERSION, SimTime, StateHasher};

/// Source of a `SimCommand` — fixed ordering class for total intra-tick order
/// (PLAN §2.2.1). Ordered: Script < Autopilot < Ui < External.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SourceClass {
    Script = 0,
    Autopilot = 1,
    Ui = 2,
    External = 3,
}

/// Total ordering key for commands within a tick (PLAN §2.2.1).
/// Stamped at dequeue time, never at production time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CommandKey {
    pub tick: u64,
    pub source: SourceClass,
    pub source_id: u64,
    pub seq: u64,
}

/// A mutation request. The only path into state (PLAN §2.2).
#[derive(Clone, Copy, Debug)]
pub enum SimCommand {
    SetWarp(u32),
    AdvanceTo(u64),
    Noop,
}

/// The authoritative world. Owns the integer clock and the command log.
pub struct World {
    clock: SimTime,
    command_log: Vec<(CommandKey, SimCommand)>,
    next_event_seq: u64,
}

impl World {
    pub fn new() -> Self {
        Self {
            clock: SimTime::ZERO,
            command_log: Vec::new(),
            next_event_seq: 0,
        }
    }

    /// One fixed sim tick (PLAN §2.5 — the only thing SimStep drives).
    pub fn step(&mut self) {
        self.clock.advance(1);
    }

    pub fn clock(&self) -> SimTime {
        self.clock
    }

    /// Canonical state hash (PLAN §7.1) — the T1 replay ground truth.
    pub fn state_hash(&self) -> [u8; 32] {
        let mut h = StateHasher::new();
        h.u32(HASH_SCHEMA_VERSION);
        h.u64(self.clock.ticks());
        h.u64(self.command_log.len() as u64);
        h.u64(self.next_event_seq);
        h.finish()
    }
}

impl Default for World {
    fn default() -> Self {
        Self::new()
    }
}
