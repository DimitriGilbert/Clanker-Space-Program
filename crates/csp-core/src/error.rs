//! The sim's typed error taxonomy (PLAN §2.13).
//!
//! `csp-sim` runs inside a server and a bench harness and must never panic in
//! release builds; every error it can produce maps to exactly one of these
//! four classes, and the class *is* the handling contract. The type lives in
//! `csp-core` so `csp-sim`, `csp-phys`, `csp-data`, and tooling agree on it.

use core::fmt;

/// The four error classes of the determinism domain (PLAN §2.13).
///
/// Payloads are `&'static str` diagnostic text intended for logs, dialogs, and
/// bug-report bundles only — they are never hashed, never serialized into the
/// canonical stream, and never branch-determining.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SimError {
    /// Invariant violated beyond recovery (NaN/±inf in state, corrupt arena,
    /// tick-counter overflow): abort the tick, roll back to the pre-tick
    /// snapshot, surface a dialog, mark the session read-only, write a
    /// bug-report bundle (§2.14).
    Fatal(&'static str),
    /// The world is playable but not persistable (data-set validation
    /// failures, uninstalled weld): play continues, saving is refused with
    /// the reason named in the UI.
    SaveBlocking(&'static str),
    /// A bounded subsystem failed (Kepler non-convergence, script abort,
    /// joint-cap degradation): log, apply the documented fallback, continue.
    Recoverable(&'static str),
    /// Presentation-only failure (missing mesh, shader warm-up): log once,
    /// placeholder, continue.
    Cosmetic(&'static str),
}

impl SimError {
    /// The diagnostic text — for logs and bug bundles only, never hashed.
    pub const fn reason(self) -> &'static str {
        match self {
            Self::Fatal(r) | Self::SaveBlocking(r) | Self::Recoverable(r) | Self::Cosmetic(r) => r,
        }
    }
}

/// The typed kind of a §2.2.1 drain rejection — the *branchable*
/// complement to [`SimError`]'s diagnostic-only reason text.
///
/// `SimError`'s payload is explicitly never branch-determining (see
/// its doc), so a consumer that must react to *which* command was
/// refused — e.g. the flight focus pin releasing when the recover's
/// own `ToBubble` drains rejected — matches on
/// `RejectedCommand::kind`, never on `reason().starts_with(..)`
/// across a crate boundary. `csp-sim` derives the kind from the
/// rejected command itself at drain time, so a failure anywhere in
/// the command's apply path carries it — including nested failures
/// whose own `SimError` text lacks any command-name prefix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RejectKind {
    /// A `SimCommand::ToBubble` rejection — the player-facing
    /// "recover control" conversion refused. `csp-view` releases the
    /// pending focus pin on this kind.
    ToBubble,
    /// A `SimCommand::ManufactureCraft` rejection — the §3.5 build
    /// order refused at its drain. `csp-view` treats one on an
    /// already-empty arena as the dead-scene edge the
    /// vessel-removal bounce cannot see (no vessel left to remove,
    /// no `ActiveBuild` in flight, Flight mode stranded).
    ManufactureCraft,
    /// Any other rejection — no consumer branches on it.
    Other,
}

impl fmt::Display for SimError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let class = match self {
            Self::Fatal(_) => "fatal",
            Self::SaveBlocking(_) => "save-blocking",
            Self::Recoverable(_) => "recoverable",
            Self::Cosmetic(_) => "cosmetic",
        };
        write!(f, "{class}: {}", self.reason())
    }
}

impl std::error::Error for SimError {}
