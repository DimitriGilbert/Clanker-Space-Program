//! The `data/balance.ron` schema — global balance constants (PLAN §8.3:
//! "ALL tuning lives here"). Named fields only — positional tuples drift
//! silently under reordering, same convention as the body/system files.

use serde::{Deserialize, Serialize};

/// The `schema_version` this loader understands for `balance.ron` — its
/// own namespace (balance files are not body/system files, though they
/// share the §3.1 first-field convention).
pub const BALANCE_SCHEMA_VERSION: u32 = 1;

/// `data/balance.ron` — the tunables every later milestone consumes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BalanceDef {
    /// First field of every data file (§3.1). Must equal
    /// [`BALANCE_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Physics bubble radius in metres (PLAN §2.3: 20 km; vessels go on
    /// rails at the radius and rejoin at 90% — the hysteresis is sim
    /// logic, not a second constant).
    pub bubble_radius_m: f64,
    /// PLAN §2.6.5 / balance.ron's own comment: `advance_to` processes at
    /// most this many events per frame; overflow returns early at the
    /// tick reached with `partial: true`. The sim-side mirror for
    /// no-data worlds is `csp_sim::DEFAULT_MAX_EVENTS_PER_FRAME` (2000) —
    /// a `csp-bench` test pins the two together.
    pub advance_to_max_events_per_frame: u64,
}
