#![forbid(unsafe_code)]

//! Headless scenario runner: csp-sim + command logs -> state hash.
//! Golden replays, LLM/eval benchmarks, economy harness (PLAN §2.12.1).
//!
//! NATIVE ONLY (§2.1): this crate may touch the filesystem and process env.
//! The replay *reconstruction* path (`replay::replay`) drives `csp-sim`
//! through the same `enqueue`/`step_frame` surface the app uses — no
//! native-only behavior leaks into sim semantics.
//!
//! - [`replay`] — the §7.6 artifact format, §7.1 fingerprint, golden I/O.
//! - [`scenarios`] — the recorded-scenario registry behind `csp-replay`
//!   and `xtask bless-goldens`.

/// M7-C §8.3 — the economy harness: the staffed-base drain probe
/// (2–5% per sim-month band), the 100-sim-year save-size check, and
/// the insolvency watch. See the module doc.
/// M7-D §3.6 — the contract-lifecycle probe: generation →
/// acceptance → advance → event-driven completion → payout,
/// determinism-verified.
pub mod contracts;
pub mod economy;
pub mod replay;
pub mod scenarios;
