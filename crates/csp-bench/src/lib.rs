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

pub mod replay;
pub mod scenarios;
