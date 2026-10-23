//! M9c §2.2.1/§2.12.3 — bounded external ingress.
//!
//! External commands reach `World::enqueue` through exactly one
//! bounded queue, stamped `SourceClass::External` with `source_id` =
//! the client index (assigned in connection order, recorded in the
//! log). The limits are the plan's, verbatim:
//!
//! - **≤ 32 commands per client per drain** — a client that floods
//!   gets a [`IngressError::ClientBudget`] backpressure error, not a
//!   timing accident ("the drop/defer decision is a *queue rule*,
//!   never a timing accident, so it is reproducible").
//! - **64 KiB max message** — enforced on the encoded command here
//!   (the transport layers enforce it on the wire too).
//! - **Bounded queue** — [`DEFAULT_QUEUE_DEPTH`] total outstanding
//!   commands; overflow is [`IngressError::QueueFull`].
//!
//! The queue is synchronous on purpose: the *drain* side is the game
//! — the running app pops it every `Input` phase; the headless host
//! ([`crate::backend::LocalGame`]) drains it into `step_frame` on
//! submit. Transport layers never see `World` directly.

use std::collections::{BTreeMap, VecDeque};

use csp_sim::{SimCommand, SourceClass, World};
use serde::Serialize;

/// §2.2.1: "a 64 KiB max message size" — checked on the command's
/// serialized size; the HTTP body limit and the WS frame cap enforce
/// the same bound on the wire.
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;

/// §2.2.1: "≤32 commands per tick" per external client — enforced as
/// at most 32 commands outstanding *per client between drains*: the
/// drain consumes the queue each boundary, so outstanding is the
/// honest, clock-free proxy for per-tick.
pub const MAX_COMMANDS_PER_CLIENT: u32 = 32;

/// Total outstanding commands across all clients — the channel's
/// bound. Beyond it, ingress is an explicit `QueueFull`, not a block.
pub const DEFAULT_QUEUE_DEPTH: usize = 256;

/// An ingress refusal — §2.2.1's "explicit backpressure error to the
/// client". Surfaced as a JSON-RPC/MCP error or a WS error frame.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "error", rename_all = "snake_case")]
pub enum IngressError {
    /// The shared queue is at capacity.
    QueueFull {
        /// The queue bound that was hit.
        max: usize,
    },
    /// This client's outstanding-command budget is spent.
    ClientBudget {
        /// The client the refusal belongs to.
        client: u32,
        /// The per-client bound that was hit.
        max: u32,
    },
    /// The encoded command exceeds the 64 KiB message cap.
    TooLarge {
        /// The offending size in bytes.
        size: usize,
        /// The cap that was hit.
        max: usize,
    },
}

impl std::fmt::Display for IngressError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::QueueFull { max } => write!(f, "ingress queue full (max {max})"),
            Self::ClientBudget { client, max } => {
                write!(
                    f,
                    "client {client} command budget exceeded (max {max} per drain)"
                )
            }
            Self::TooLarge { size, max } => {
                write!(f, "command too large ({size} bytes > {max})")
            }
        }
    }
}

impl std::error::Error for IngressError {}

/// The bounded FIFO between external clients and `World::enqueue`.
/// Production order across clients is merge order; per-client order
/// is preserved into the sim's per-`(External, client)` FIFOs — which
/// is what `source_seq` stamps.
#[derive(Debug)]
pub struct IngressQueue {
    pending: VecDeque<(u32, SimCommand)>,
    /// Outstanding commands per client — the per-drain budget ledger.
    per_client: BTreeMap<u32, u32>,
    max_queue: usize,
    max_per_client: u32,
    max_message_bytes: usize,
}

impl Default for IngressQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl IngressQueue {
    /// The §2.2.1 bounds.
    pub fn new() -> Self {
        Self {
            pending: VecDeque::new(),
            per_client: BTreeMap::new(),
            max_queue: DEFAULT_QUEUE_DEPTH,
            max_per_client: MAX_COMMANDS_PER_CLIENT,
            max_message_bytes: MAX_MESSAGE_BYTES,
        }
    }

    /// Tighter bounds for tests.
    pub fn with_limits(max_queue: usize, max_per_client: u32) -> Self {
        Self {
            max_queue,
            max_per_client,
            ..Self::new()
        }
    }

    /// Outstanding commands right now.
    pub fn len(&self) -> usize {
        self.pending.len()
    }

    /// `true` when nothing waits to drain.
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Enqueue one command — the bounded, budgeted entry point.
    /// Refusals are the §2.2.1 explicit backpressure errors.
    pub fn push(&mut self, client: u32, command: SimCommand) -> Result<(), IngressError> {
        // The 64 KiB message cap on the encoded command — a `CraftDef`
        // payload is the only variant that can approach it.
        let size = serde_json::to_vec(&command)
            .map(|b| b.len())
            .unwrap_or(usize::MAX);
        if size > self.max_message_bytes {
            return Err(IngressError::TooLarge {
                size,
                max: self.max_message_bytes,
            });
        }
        let used = self.per_client.get(&client).copied().unwrap_or(0);
        if used >= self.max_per_client {
            return Err(IngressError::ClientBudget {
                client,
                max: self.max_per_client,
            });
        }
        if self.pending.len() >= self.max_queue {
            return Err(IngressError::QueueFull {
                max: self.max_queue,
            });
        }
        self.pending.push_back((client, command));
        *self.per_client.entry(client).or_insert(0) += 1;
        Ok(())
    }

    /// Pop the oldest outstanding command, decrementing its client's
    /// budget — FIFO across clients.
    pub fn pop(&mut self) -> Option<(u32, SimCommand)> {
        let (client, command) = self.pending.pop_front()?;
        if let Some(n) = self.per_client.get_mut(&client) {
            *n = n.saturating_sub(1);
        }
        Some((client, command))
    }

    /// Drain everything into `world` — the game-side entry: each
    /// command lands as `(SourceClass::External, client)` in the sim's
    /// per-source FIFO, stamped at the next boundary drain (§2.2.1:
    /// agents get the same one-tick latency as UI input). Returns the
    /// count moved.
    pub fn drain_into(&mut self, world: &mut World) -> usize {
        let mut n = 0;
        while let Some((client, command)) = self.pop() {
            world.enqueue(SourceClass::External, client, command);
            n += 1;
        }
        n
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // test assertions
mod tests {
    use super::*;

    #[test]
    fn per_client_budget_is_enforced() {
        let mut q = IngressQueue::with_limits(1024, 2);
        for _ in 0..2 {
            q.push(7, SimCommand::SetWarp(1)).expect("within budget");
        }
        assert!(matches!(
            q.push(7, SimCommand::SetWarp(1)),
            Err(IngressError::ClientBudget { client: 7, .. })
        ));
        // A different client is unaffected.
        q.push(8, SimCommand::SetWarp(1))
            .expect("other client budget");
        // Draining frees the budget.
        let mut w = World::new();
        assert_eq!(q.drain_into(&mut w), 3);
        q.push(7, SimCommand::SetWarp(1))
            .expect("freed after drain");
    }

    #[test]
    fn queue_depth_is_bounded() {
        let mut q = IngressQueue::with_limits(2, 10);
        q.push(0, SimCommand::SetWarp(1)).unwrap();
        q.push(0, SimCommand::SetWarp(1)).unwrap();
        assert!(matches!(
            q.push(1, SimCommand::SetWarp(1)),
            Err(IngressError::QueueFull { .. })
        ));
    }

    #[test]
    fn drain_stamps_external_source() {
        let mut q = IngressQueue::new();
        q.push(5, SimCommand::SetWarp(9)).unwrap();
        let mut w = World::new();
        q.drain_into(&mut w);
        w.step_frame(1).unwrap();
        let (key, _) = &w.command_log()[0];
        assert_eq!(key.source_class, SourceClass::External);
        assert_eq!(key.source_id, 5);
    }
}
