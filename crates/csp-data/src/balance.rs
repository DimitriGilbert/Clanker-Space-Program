//! The `data/balance.ron` schema — global balance constants (PLAN §8.3:
//! "ALL tuning lives here"). Named fields only — positional tuples drift
//! silently under reordering, same convention as the body/system files.

use serde::{Deserialize, Serialize};

/// The `schema_version` this loader understands for `balance.ron` — its
/// own namespace (balance files are not body/system files, though they
/// share the §3.1 first-field convention).
///
/// Version 2 (M6-B): the §2.4 docking block landed —
/// `capture_window_ticks`, the capture predicate tunables
/// (range/angle/relative speed), the soft-capture joint compliance
/// (pull rate + correction cap), and the per-bubble live-joint cap.
///
/// Version 3 (M6-C): the latch settle tunables —
/// `dock_latch_separation_m`/`dock_latch_rel_speed_mps`, the "settled"
/// gates that consume a soft-capture inside the window (§2.4's
/// "docking rigidizes" step 2).
///
/// Version 4 (M6-D): the §3.5 transfer block — `transfer_radius_m`,
/// `transfer_rate_units_per_s`, and `crew_transfer_ticks`, the
/// resource/crew transfer process tunables.
pub const BALANCE_SCHEMA_VERSION: u32 = 4;

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
    /// PLAN §2.9: `max_tracked_vessels` — vessel-arena cap; spawns
    /// beyond it reject `Recoverable`. The sim-side mirror is
    /// `csp_sim::DEFAULT_MAX_VESSELS` (512).
    pub max_tracked_vessels: u64,
    /// PLAN §2.9: `max_debris` — breakup debris cap; the sim evicts
    /// oldest-first by `(debris_since, id)`. Mirror:
    /// `csp_sim::DEFAULT_MAX_DEBRIS` (64).
    pub max_debris: u64,
    /// PLAN §2.4: hard cap on live transient joints per bubble — joint
    /// creation beyond it degrades to an immediate clean separation.
    /// Mirror: `csp_sim::DEFAULT_MAX_LIVE_JOINTS` (16).
    pub max_live_joints_per_bubble: u64,
    /// PLAN §2.4: `capture_window_ticks` — the soft-capture joint's
    /// maximum life, ticks (default 192 = 3 s at 64 Hz). Mirror:
    /// `csp_sim::DEFAULT_CAPTURE_WINDOW_TICKS`.
    pub capture_window_ticks: u64,
    /// Docking capture predicate: two port faces within this distance
    /// (m) may soft-capture.
    pub dock_capture_range_m: f64,
    /// Docking capture predicate: maximum port-facing misalignment
    /// (degrees). The sim stores it as a cosine — capture requires
    /// `dir_a · dir_b ≤ −cos(angle)`.
    pub dock_capture_angle_deg: f64,
    /// Docking capture predicate: maximum relative speed at the port
    /// faces (m/s) — a fast fly-through cannot capture.
    pub dock_capture_rel_speed_mps: f64,
    /// Soft-capture compliance (§2.4 "a few seconds"): exponential
    /// anchor reel-in rate (1/s) — separation decays at `e^{−rate·t}`.
    pub dock_soft_pull_per_s: f64,
    /// Soft-capture compliance: the joint's anchor-closing speed cap
    /// (m/s) — the "soft": approach energy damps rather than welding.
    pub dock_soft_max_correction_mps: f64,
    /// §2.4's latch merge (M6-C): the joint's anchors within this
    /// distance (m) count as mated — the first half of the settle
    /// predicate that rigidizes the pair inside the capture window.
    /// Mirror: `csp_sim::DEFAULT_LATCH_SEPARATION_M` (0.1).
    pub dock_latch_separation_m: f64,
    /// §2.4's latch merge (M6-C): relative point speed at the anchors
    /// at or below this (m/s) — the second half of the settle
    /// predicate. Mirror: `csp_sim::DEFAULT_LATCH_REL_SPEED_MPS` (0.5).
    pub dock_latch_rel_speed_mps: f64,
    /// §3.5's `transfer_radius_m` (default 100 m) — the undocked
    /// transfer precondition: two vessels whose centres are within
    /// this distance may run a `ResourceTransfer`/crew transfer
    /// without docking. Mirror: `csp_sim::DEFAULT_TRANSFER_RADIUS_M`.
    pub transfer_radius_m: f64,
    /// §3.5's transfer rate — the plan's `min(source_outflow,
    /// dest_inflow)` per connected port pair. v1 has no per-port flow
    /// stats, so a single rate (units/s) stands in for the pair —
    /// documented M6-D simplification. Mirror:
    /// `csp_sim::DEFAULT_TRANSFER_RATE_PER_S`.
    pub transfer_rate_units_per_s: f64,
    /// The crew-transfer process duration in ticks (§3.5's
    /// `ResourceTransfer` process applied to crew — the plan names a
    /// rate for resources only; a fixed traversing delay stands in).
    /// Mirror: `csp_sim::DEFAULT_CREW_TRANSFER_TICKS`.
    pub crew_transfer_ticks: u64,
}
