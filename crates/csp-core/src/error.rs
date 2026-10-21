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
