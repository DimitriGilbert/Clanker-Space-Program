//! M9a §2.10 — the scripting/autopilot control layer's sim-side surface.
//!
//! The plan's model, verbatim: **scripts are tick-entry-point programs,
//! not coroutines** — the host calls `on_tick(ctx)` once per tick; a
//! budget-exceeding call is *aborted* (there is no resumable VM), the
//! strike counter increments, and `strikes >= 5` disables the script
//! visibly. Cross-tick state is the script's own problem via the
//! `persist` map — a map of *primitives* that serializes into the save
//! and hashes. `wait`/`until` are host services that register a
//! [`WakeCondition`]; the host publishes
//! [`EventKind::ScriptWake`](crate::EventKind) (`Clamping`) at the
//! resolved tick so a rails vessel's script survives warp.
//!
//! # Crate split
//!
//! This module owns only what the *world* must know: the script
//! arena's state types, the per-tick IO surface ([`TickIo`]) the
//! engine is handed, the telemetry snapshot ([`VesselTelemetry`]), and
//! the engine/driver traits. The guest-language host itself lives in
//! `csp-script` (the [`ScriptEngine`] implementor is installed via
//! [`World::set_script_engine`](crate::World::set_script_engine)); the
//! Rust ascent autopilot lives in `csp-ai` (the [`AutopilotDriver`]
//! implementor, installed via `set_autopilot_driver`). That direction
//! keeps `rhai` out of the determinism crate's tree entirely — the
//! engine is construction config: never hashed, never serialized,
//! re-installed post-restore like every other data-derived channel.
//!
//! # Tick model (§2.10's last rule)
//!
//! Scripts run inside `step_bubble_tick()` — the per-integer-tick pass
//! inside [`World::step_bubble_range`](crate::World) — *before force
//! integration*, in ascending [`ScriptId`] order, followed by engaged
//! autopilots in ascending `VesselId` order. A control call enqueues a
//! [`SimCommand`](crate::SimCommand); the command drains at the *next*
//! tick's boundary — the same one-tick latency every producer pays
//! (§2.2.1), stamped `SourceClass::Script` / `SourceClass::Autopilot`
//! so the intra-tick `CommandKey` order is the declared class order
//! (Script < Autopilot < Ui < External).
//!
//! A script bound to a vessel that is not `InBubble` ticks **only**
//! when its `ScriptWake` event fires — that is what "survives rails
//! warp" means: there are no bubble ticks under rails, so the wake
//! event is the tick. The corollary holds too: a `ScriptWake` firing
//! for a script whose vessel IS `InBubble` consumes the registration
//! without running `on_tick` — the bubble's own control pass delivers
//! that tick (running both would double-tick the same tick value).
//!
//! Commands a script `issue`s are stamped at the producing tick + 1 —
//! the latency is measured from the tick the `on_tick` ran at, so a
//! wake-fired tick's issues never drain at the boundary that already
//! passed (see `PendingCommand` in `lib.rs`).
//!
//! # Warp discipline (§2.10)
//!
//! - `ScriptWake` is `Clamping`: warp demotes at the event, the script
//!   runs, the player cannot sleep through it.
//! - A script holding nonzero throttle already blocks rails warp via
//!   the §2.6.2 throttle check.
//! - **Poll detection**: a tick counts as *heavy* when the engine
//!   reports the call aborted on the op budget, *or* consumed
//!   [`POLL_OPS_NUM`]/[`POLL_OPS_DEN`] or more of the budget and
//!   returned. [`POLL_DEMOTE_TICKS`] consecutive heavy ticks set
//!   `poll_demoted`, which [`World::rails_warp_refusal`](crate::World::rails_warp_refusal)
//!   reports by name — a visible warp refusal that caps the ladder at
//!   the ×2 physics-warp floor (rung 1, the "bubble occupied" regime
//!   the refusal describes) while the refusal holds, per the app's
//!   rails-refusal floor rules. A clean tick
//!   resets the counter and clears the flag. (The plan's literal
//!   "budget-exhausting for 10 consecutive ticks" cannot mean aborted
//!   calls only — `strikes >= 5` would disable the script first — so
//!   near-exhaust completions count too: a script that spins right up
//!   to the limit every tick without tripping it is still a poller.
//!   PLAN §2.10 says the poll demotes warp "to ×1"; post-C24-F2 the
//!   refusal floors at ×2 like every other `rails_warp_refusal`
//!   producer — that divergence is tracked as a spec question.)
//! - A script with no pending wake and no throttle blocks nothing.
//!
//! # What is sim state (hashed + serialized, §7.2)
//!
//! `scripts` arena + `next_script_id`, `script_op_budget` (the
//! per-save op limit — construction config that changes evolution,
//! the `event_budget` precedent), `autopilots` arena. The engines and
//! their compiled-code caches are construction config (§7.3):
//! re-installed by the caller post-restore, never hashed — a cleared
//! AST cache simply recompiles the serialized `source`.

use std::collections::BTreeMap;

use csp_core::{DQuat, DVec3, SimError, StableDiscriminant, StateHasher};
use csp_data::{BodyId, KeplerElements};
use serde::{Deserialize, Serialize};

use crate::{SimCommand, SourceClass, VesselId};

/// §2.10's default per-call op budget — "default 20 000, configurable
/// per save". Mirror of `data/balance.ron: script_op_budget` (the
/// [`crate::DEFAULT_MAX_EVENTS_PER_FRAME`] precedent): the loaded
/// value reaches [`World`](crate::World) through
/// `set_script_op_budget`; no-data worlds and unit tests use this.
pub const DEFAULT_SCRIPT_OP_BUDGET: u64 = 20_000;

/// §2.10: consecutive faulting calls that disable a script ("strikes
/// >= 5 (consecutive) disables the script, visibly").
pub const STRIKE_DISABLE_LIMIT: u32 = 5;

/// §2.10's poll detector — consecutive heavy ticks that demote warp.
pub const POLL_DEMOTE_TICKS: u32 = 10;

/// The heavy-tick numerator/denominator — a completed call consuming
/// ≥ 9/10 of the op budget counts as budget-exhausting for poll
/// detection (see the module doc for why near-exhausts count).
pub const POLL_OPS_NUM: u64 = 9;
/// See [`POLL_OPS_NUM`].
pub const POLL_OPS_DEN: u64 = 10;

/// §2.2 sim-side script identity: a monotonic `u64`, never reused
/// (`next_script_id` is serialized per §7.2). Iteration that affects
/// state is ascending-ID — the `BTreeMap` arena's key order IS that
/// order (the `VesselId` precedent).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ScriptId(pub u64);

/// One cell of a script's `persist` map — §2.10's "a Rhai map of
/// primitives". The primitive set is deliberately this four-variant
/// enum: it is the set Rhai `Dynamic`s convert to *losslessly*
/// (bool / i64 / f64 / char-or-string), so a persist write of
/// anything richer (arrays, nested maps, timestamps) fails loudly at
/// conversion time instead of silently degrading in the save.
#[repr(u16)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum PersistValue {
    /// Rhai `bool`.
    Bool(bool) = 0,
    /// Rhai `INT` (i64 under the pinned feature set — `only_i32` is
    /// banned by §2.10's parity rule).
    Int(i64) = 1,
    /// Rhai `FLOAT` (f64 — `f32_float` is banned the same way).
    Float(f64) = 2,
    /// Rhai `String`.
    Text(String) = 3,
}

impl StableDiscriminant for PersistValue {
    fn discriminant(&self) -> u16 {
        match self {
            Self::Bool(_) => 0,
            Self::Int(_) => 1,
            Self::Float(_) => 2,
            Self::Text(_) => 3,
        }
    }
}

impl PersistValue {
    /// Rule-4 sanity for restore validation: the float variant must
    /// be finite (a serialized NaN would poison the hash on restore).
    fn is_valid(&self) -> bool {
        match self {
            Self::Float(f) => f.is_finite(),
            _ => true,
        }
    }

    fn hash_into(&self, h: &mut StateHasher) {
        h.stable_enum(self);
        match self {
            Self::Bool(b) => {
                h.bool(*b);
            }
            Self::Int(v) => {
                h.i64(*v);
            }
            Self::Float(v) => {
                h.f64(*v);
            }
            Self::Text(s) => {
                h.str(s);
            }
        }
    }
}

/// §2.10's `persist` — "a Rhai map of primitives, serialized into the
/// save and included in the state hash". `BTreeMap` key order is the
/// canonical enumeration (§7.1 rule 6).
pub type PersistMap = BTreeMap<String, PersistValue>;

fn hash_persist(h: &mut StateHasher, persist: &PersistMap) {
    h.btree_map(
        persist,
        |h, k| {
            h.str(k);
        },
        |h, v| v.hash_into(h),
    );
}

/// One script's sim-side record — everything about it that survives a
/// save. The engine-side compiled artifact is NOT here: it is a pure
/// function of `source`, re-derived on demand (§7.3).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScriptState {
    /// Its arena key — kept inline per §2.2's arena-of-structs idiom.
    pub id: ScriptId,
    /// The vessel this script is bound to — the default telemetry and
    /// command target (kOS's per-CPU-part binding). A script on a
    /// removed vessel keeps ticking; its vessel-bound telemetry reads
    /// `Missing` and its commands reject `Recoverable`.
    pub vessel: VesselId,
    /// The guest-language source verbatim — sim state: a changed
    /// script is a changed run.
    pub source: String,
    /// `false` = stopped (unloaded stays a record until
    /// `UnloadScript`; disabled scripts keep their `persist` so a
    /// re-enabled script resumes its state machine).
    pub enabled: bool,
    /// Why the host disabled it — the "visibly, with the reason in
    /// the UI" half of §2.10's strike rule. `None` while `enabled` or
    /// while disabled by command rather than by fault.
    pub disabled_reason: Option<String>,
    /// §2.10's per-script strike counter — consecutive faulting calls
    /// (budget abort or runtime error); a clean tick resets it.
    pub strikes: u32,
    /// Consecutive heavy ticks (see the module doc's poll rule).
    pub consecutive_polls: u32,
    /// Set when `consecutive_polls` reached [`POLL_DEMOTE_TICKS`] —
    /// the flag `rails_warp_refusal` names. Clears on the first
    /// non-heavy tick.
    pub poll_demoted: bool,
    /// The resolved wake tick this script is waiting on — pairs with
    /// exactly one pending [`EventKind::ScriptWake`](crate::EventKind)
    /// at this tick (the `ManeuverNode`↔`ManeuverNodeStart` pairing
    /// precedent; `None` = no wake registered).
    pub wake_tick: Option<u64>,
    /// §2.10's cross-tick continuation state.
    pub persist: PersistMap,
}

/// What an [`AutopilotDriver`] flies — serialized with the autopilot
/// state so a restored world resumes the same program. Discriminants
/// append-only (§7.1 rule 7).
#[repr(u16)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum AutopilotProgram {
    /// Launch-to-orbit ascent (csp-ai's gravity-turn implementation):
    /// `target_apoapsis_m` and `target_periapsis_m` are *altitudes*
    /// above the datum, both must be finite.
    Ascent {
        /// Target apoapsis altitude above datum, m.
        target_apoapsis_m: f64,
        /// Target periapsis (circularization) altitude above datum, m.
        target_periapsis_m: f64,
    } = 0,
}

impl StableDiscriminant for AutopilotProgram {
    fn discriminant(&self) -> u16 {
        match self {
            Self::Ascent { .. } => 0,
        }
    }
}

impl AutopilotProgram {
    /// Finite-parameter check for the command apply and restore.
    pub fn is_valid(&self) -> bool {
        match self {
            Self::Ascent {
                target_apoapsis_m,
                target_periapsis_m,
            } => target_apoapsis_m.is_finite() && target_periapsis_m.is_finite(),
        }
    }
}

/// One engaged autopilot — the Rust-side counterpart of
/// [`ScriptState`]: a program plus its `persist`-equivalent scratch
/// map (the driver keeps its phase machine there, so save/load
/// resumes mid-flight). Keyed by vessel — at most one autopilot per
/// vessel in v1 (a second `EngageAutopilot` replaces it).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AutopilotState {
    /// The program the driver runs.
    pub program: AutopilotProgram,
    /// `false` pauses the driver without losing the phase state.
    pub enabled: bool,
    /// The driver's deterministic cross-tick scratch — same rules as
    /// a script's `persist` map.
    pub persist: PersistMap,
}

/// §2.10's wake registration — "conditions that reduce to a closed-
/// form time … are accepted; conditions that do not are refused at
/// registration". The host resolves a condition to a tick *at
/// registration*; the pending [`EventKind::ScriptWake`](crate::EventKind)
/// is the wake itself. Not serialized — what persists is the resolved
/// [`ScriptState::wake_tick`] ↔ pending-event pair.
///
/// **The in-bubble rule (documented semantics):** a wake never gates
/// an `InBubble` script's ticking — the bubble's control pass runs
/// `on_tick` every tick regardless, and a firing `ScriptWake` is
/// consumed silently under the double-tick rule. So `wait_ticks` /
/// `wait_until_tick` are *vacuous* while in-bubble (the script keeps
/// ticking; use a `persist` counter for an in-bubble delay). Their
/// value is as the tick carrier off-bubble: on `OnRails`/
/// `SurfaceLatched` vessels the wake IS the only `on_tick` a script
/// ever gets.
#[derive(Clone, Debug, PartialEq)]
pub enum WakeCondition {
    /// An absolute sim tick — must be strictly ahead of the clock.
    AtTick(u64),
    /// `n` ticks from now — must be `> 0`.
    InTicks(u64),
    /// Next apoapsis of the bound vessel's conic. Resolved against
    /// the stamped conic on `OnRails`, against the osculating conic
    /// on `InBubble` (so an ascent script can arm the apsis before
    /// the auto-rails edge — the wake then CARRIES the post-rails
    /// tick); refused with no closed-form future apsis (open conic,
    /// latched hull, unsolvable state).
    AtApoapsis,
    /// Next periapsis of the bound vessel's conic. Same osculating
    /// rule as [`Self::AtApoapsis`]; refused when the conic has no
    /// periapsis ahead (a hyperbolic vessel already past its
    /// periapsis is refused).
    AtPeriapsis,
    /// §2.10's "altitude crossing on rails" — the next crossing of
    /// `above_m` above the frame body's datum, `ascending` selecting
    /// the outbound or inbound branch. Closed-form like the apsides:
    /// the ellipse's eccentric anomaly at `r = a(1 − e·cosE)`, the
    /// hyperbola's `coshF` mirror. Refused when the conic never
    /// crosses (target outside `[r_p, r_a)`, or the crossing is
    /// behind a hyperbolic vessel already outbound), or for a
    /// circular conic (`e ≈ 0` has no defined crossing phase).
    AtAltitude {
        /// Altitude above the central body's datum, m.
        above_m: f64,
        /// `true` = the outbound (climbing) crossing.
        ascending: bool,
    },
    /// The bound vessel's next pending `SoiTransition` — the script
    /// wakes in the new frame. Refused when the vessel is not on
    /// rails or has no SOI event queued.
    AtNextSoiTransition,
    /// The bound vessel's scheduled maneuver-node burn tick —
    /// refused when the vessel has no `Scheduled` node.
    AtManeuverNode,
}

/// The per-tick telemetry snapshot the host hands a script or
/// autopilot — *read-only*, computed fresh each tick from the arena
/// (never stored, never hashed: a pure function of hashed state).
///
/// Positions/velocities are **parent-centred inertial** in whatever
/// frame the residence reports: the bubble frame's body-centred
/// vectors for `InBubble`, the conic parent's frame for `OnRails`
/// (matching [`OrbitModel::state_at`](crate::orbit::OrbitModel)).
///
/// `Serialize` (one-directional) so the M9c interface tier can put it
/// straight on the wire — it is a read snapshot, never a save type.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct VesselTelemetry {
    /// The tick this snapshot was taken at.
    pub tick: u64,
    /// `false` when the vessel id is unknown — every field below is
    /// then its `None`/zero default.
    pub found: bool,
    /// The vessel's residence, mirrored into the telemetry enum so
    /// scripts match on a stable, serialized-schema-free value.
    pub residence: TelemetryResidence,
    /// Parent-centred inertial position, m (`Missing`: zero).
    pub pos_m: DVec3,
    /// Inertial velocity, m/s.
    pub vel_mps: DVec3,
    /// Orientation: body frame → inertial frame.
    pub rot: DQuat,
    /// Angular velocity, body frame, rad/s.
    pub ang_vel_mps: DVec3,
    /// Current throttle fraction `[0, 1]`.
    pub throttle: f64,
    /// CoM altitude above the datum radius, m — when the frame's
    /// central body radius is known; `None` for `Missing` or a
    /// radius-less parent.
    pub altitude_m: Option<f64>,
    /// Body-centred speed, m/s.
    pub speed_mps: f64,
    /// Radial (vertical) speed — `v·r̂`, m/s.
    pub vertical_speed_mps: f64,
    /// Downrange (horizontal) speed — `‖v − v_r·r̂‖`, m/s.
    pub horizontal_speed_mps: f64,
    /// Welded mass, kg — `None` for rails/latched/missing vessels
    /// (only the bubble body carries the welded mass).
    pub mass_kg: Option<f64>,
    /// Osculating apoapsis altitude above the datum, m — `None` on an
    /// open conic, an unsolvable state, or `Missing`.
    pub apoapsis_altitude_m: Option<f64>,
    /// Osculating periapsis altitude above the datum, m.
    pub periapsis_altitude_m: Option<f64>,
    /// Closed-form seconds to the next apoapsis — `None` on open
    /// conics and unsolvable states.
    pub time_to_apoapsis_s: Option<f64>,
    /// Closed-form seconds to the next periapsis — `None` when the
    /// conic has no periapsis ahead.
    pub time_to_periapsis_s: Option<f64>,
    /// The frame's gravitational parameter, m³/s² — `None` for
    /// `Missing` or a bubble-less world.
    pub mu_m3_per_s2: Option<f64>,
    /// The frame's central body.
    pub body: Option<BodyId>,
    /// The conic elements (rails vessels, or the osculating
    /// conversion of a bubble body's state).
    pub elements: Option<KeplerElements>,
    /// The vessel's scheduled maneuver node, if any — `(tick, dv
    /// prograde, normal, radial)`, so a script can arm/wait on it.
    pub maneuver_node: Option<(u64, DVec3)>,
    /// Stored resource units aboard — `resource id → units` summed
    /// over every tank node (the `vessel_resource` read,
    /// denormalized so a script can inventory its propellant without
    /// a part-graph walk). Empty for a craft-less vessel.
    pub resources: BTreeMap<String, f64>,
    /// The vessel's part graph as aimable handles — `(node id, part
    /// id)` pairs, the targets `stage`/`jettison`/`deploy` take.
    /// Empty for a craft-less vessel.
    pub parts: Vec<(u64, String)>,
}

/// Where the vessel lives — the telemetry enum mirror of
/// [`VesselState`](crate::VesselState) so the control layer doesn't
/// pattern-match sim internals.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum TelemetryResidence {
    /// Conic state, no body.
    OnRails,
    /// Welded body stepped per tick.
    InBubble,
    /// Body-fixed pose, no integration.
    SurfaceLatched,
    /// Unknown vessel id.
    Missing,
}

/// What one script tick did to its budget — returned by
/// [`ScriptEngine::run_tick`].
#[derive(Clone, Debug, PartialEq)]
pub struct ScriptRunReport {
    /// Operations the engine counted for this call (Rhai's
    /// `on_progress` tally — 0 for engines without a counter).
    pub ops_used: u64,
    /// `Some` when the call faulted; the strike rules live in
    /// [`World`](crate::World)'s tick pass, not the engine.
    pub fault: Option<ScriptFault>,
}

/// Why a script call ended early — the two fault classes §2.10
/// distinguishes (a budget abort is ALSO the poll detector's input).
#[derive(Clone, Debug, PartialEq)]
pub enum ScriptFault {
    /// `set_max_operations` tripped — the call was aborted mid-
    /// statement; there is no continuation.
    BudgetExceeded,
    /// Any other engine/runtime failure — carried verbatim for the
    /// strike log and `disabled_reason`.
    Runtime(String),
}

/// The engine seam — `csp-script`'s `RhaiHost` implements this; the
/// world owns it as `Option<Box<dyn ScriptEngine>>` (construction
/// config: a world with no engine loaded scripts that simply never
/// tick — their `LoadScript` apply still validates the arena, and
/// `prepare` is skipped).
///
/// Object-safe by construction: one method per lifecycle verb, no
/// generics, no `Self` returns.
pub trait ScriptEngine {
    /// Compile/prepare a script — called from the `LoadScript` apply
    /// so a syntax fault rejects the command (`Recoverable`), never
    /// lands as a silent dead script. The engine may cache whatever
    /// it derives from `source` keyed by `id`; the cache is scratch
    /// (§7.3), never sim state.
    fn prepare(&mut self, id: ScriptId, source: &str) -> Result<(), String>;

    /// `on_tick(ctx)` for one script — §2.10's once-per-tick entry
    /// point. The engine may mutate `script.persist` — that map is
    /// the script's own cross-tick state — but must not touch
    /// strikes/enabled/wake bookkeeping: those are sim fields applied
    /// by the world after the call returns.
    fn run_tick(
        &mut self,
        script: &mut ScriptState,
        tick: u64,
        io: &mut dyn TickIo,
    ) -> ScriptRunReport;

    /// Drop engine-side state for `id` (`UnloadScript`, vessel
    /// removal housekeeping). Must tolerate unknown ids.
    fn unload(&mut self, id: ScriptId);

    /// The world forwards its per-save [`script_op_budget`] —
    /// construction-time and `set_script_op_budget` calls reach the
    /// engine through this so the world's budget is the ONE source
    /// of truth (the engine's `max_operations` is its enforcement
    /// mechanism; the world still strikes on the reported fault).
    fn set_op_budget(&mut self, _ops: u64) {}
}

/// The Rust-autopilot seam — `csp-ai`'s `AscentAutopilot` implements
/// this; the world drives one [`AutopilotState`] per engaged vessel
/// through it, same tick slot as scripts (after them — ascending
/// `VesselId`), same `TickIo` command surface stamped
/// `SourceClass::Autopilot`.
pub trait AutopilotDriver {
    /// Which program this driver flies — the world runs `run_tick`
    /// only when `state.program` matches (M9a ships exactly one
    /// variant, so this is a forward-compat seam).
    fn program(&self) -> AutopilotProgram;

    /// One tick of the program — reads telemetry through `io`,
    /// enqueues commands through `io`, keeps its phase machine in
    /// `state.persist`. `Err` reports on `degraded` (an autopilot
    /// fault is a bug, not a strike — the driver is trusted code).
    fn run_tick(
        &mut self,
        vessel: VesselId,
        state: &mut AutopilotState,
        tick: u64,
        io: &mut dyn TickIo,
    ) -> Result<(), String>;
}

/// The per-tick IO surface the world hands a running script or
/// autopilot — the plumbing layer under `csp-script`'s `ControlApi`.
/// Telemetry is a read; every mutation becomes a [`SimCommand`]
/// enqueued for the next tick's drain (§2.10: "Nothing bypasses the
/// queue").
pub trait TickIo {
    /// The bound vessel's telemetry snapshot — `found: false` fields
    /// when the id is unknown.
    fn telemetry(&self, vessel: VesselId) -> VesselTelemetry;

    /// Every live vessel id — the enumeration `telemetry` reads
    /// against (the guest's `vessels()`/`telemetry_of` surface).
    /// Ascending-id order, same as the arena.
    fn vessels(&self) -> Vec<VesselId>;

    /// Enqueue a command for the *next* tick's drain under
    /// `(class, source_id)` — production order per source is
    /// preserved by the FIFO (§2.2.1).
    ///
    /// The `(class, source_id)` arguments are advisory: the io is
    /// bound to exactly one producer and re-derives the stamp from
    /// that binding (a script io stamps `(Script, its id)`, an
    /// autopilot io `(Autopilot, its vessel)`). An honest engine
    /// passes what the io would derive anyway; a mismatched stamp
    /// is an engine bug — the sim's io debug-asserts on it and never
    /// lets it reach the queue, so a guest cannot stamp `Ui` to
    /// bypass the §2.2.1 lease gate or impersonate another
    /// producer's channel.
    fn issue(&mut self, class: SourceClass, source_id: u32, command: SimCommand);

    /// Register `script`'s wake: resolve `condition` to a tick, drop
    /// any previous pending `ScriptWake` for the script, and schedule
    /// the new `Clamping` event. Returns the resolved tick.
    /// `Recoverable` refusal when the condition has no closed-form
    /// resolution (§2.10's "refused at registration").
    fn request_wake(
        &mut self,
        script: ScriptId,
        condition: &WakeCondition,
    ) -> Result<u64, SimError>;

    /// Drop `script`'s pending wake registration — `WAIT`-less
    /// scripts need it to stop being warp-relevant.
    fn cancel_wake(&mut self, script: ScriptId) -> Result<(), SimError>;

    /// A `log`/`print` line — rides `AdvanceReport::script_log` to
    /// the player's console. Bounded callers' responsibility (the op
    /// budget caps per-tick volume).
    fn log_line(&mut self, script: ScriptId, line: &str);
}

// ---------------------------------------------------------------------------
// Hashing + validity (§7.1 walk / §7.4 restore coherence)
// ---------------------------------------------------------------------------

/// §7.1 field walk for one script — called under the arena's
/// canonical ascending-`ScriptId` enumeration.
pub fn hash_script(h: &mut StateHasher, s: &ScriptState) {
    h.u64(s.id.0);
    h.u64(s.vessel.0);
    h.str(&s.source);
    h.bool(s.enabled);
    h.opt(&s.disabled_reason, |h, r| {
        h.str(r);
    });
    h.u32(s.strikes);
    h.u32(s.consecutive_polls);
    h.bool(s.poll_demoted);
    h.opt(&s.wake_tick, |h, t| {
        h.u64(*t);
    });
    hash_persist(h, &s.persist);
}

/// The script arena's walk — counter then arena (the `transfers`
/// precedent).
pub fn hash_scripts(
    h: &mut StateHasher,
    next_script_id: u64,
    scripts: &BTreeMap<ScriptId, ScriptState>,
) {
    h.u64(next_script_id);
    h.arena(scripts.iter().map(|(id, s)| (id.0, s)), |h, s| {
        hash_script(h, s);
    });
}

/// §7.1 field walk for one autopilot — called under
/// [`hash_autopilots`]'s arena walk, which writes the `VesselId` key.
pub fn hash_autopilot(h: &mut StateHasher, a: &AutopilotState) {
    h.stable_enum(&a.program);
    match &a.program {
        AutopilotProgram::Ascent {
            target_apoapsis_m,
            target_periapsis_m,
        } => {
            h.f64(*target_apoapsis_m);
            h.f64(*target_periapsis_m);
        }
    }
    h.bool(a.enabled);
    hash_persist(h, &a.persist);
}

/// The autopilot arena's walk — `BTreeMap<VesselId, _>` key order
/// (the arena helper writes each `VesselId` key itself).
pub fn hash_autopilots(h: &mut StateHasher, autopilots: &BTreeMap<VesselId, AutopilotState>) {
    h.arena(autopilots.iter().map(|(v, a)| (v.0, a)), |h, a| {
        hash_autopilot(h, a);
    });
}

/// Restore-side per-record validity — finite persist floats + a
/// well-formed program (the `q.is_valid()` precedent from tech).
pub fn script_state_is_valid(s: &ScriptState) -> bool {
    s.persist.values().all(PersistValue::is_valid)
}

/// See [`script_state_is_valid`].
pub fn autopilot_state_is_valid(a: &AutopilotState) -> bool {
    a.program.is_valid() && a.persist.values().all(PersistValue::is_valid)
}

// ---------------------------------------------------------------------------
// Closed-form wake resolution (§2.10: accepted conditions only)
// ---------------------------------------------------------------------------

/// Resolve a wake condition to an absolute tick — the closed-form
/// rule §2.10 requires. `elements`/`mu`/`now` describe the bound
/// vessel's *rails* conic; `next_soi_tick`/`node_tick` come from the
/// world's pending-event scan / maneuver record. `epoch_ticks` is the
/// conic's stamping epoch; `clock` is the current tick.
///
/// Every non-`AtTick`/`InTicks` condition needs the caller to have
/// already verified the vessel is `OnRails` (or carries a node) —
/// this is the pure-math half; the world-side wrapper enforces the
/// residence preconditions.
pub fn resolve_apsis_ticks(
    elements: &KeplerElements,
    mu: f64,
    epoch_ticks: u64,
    clock: u64,
    apoapsis: bool,
) -> Result<Option<u64>, SimError> {
    let e = elements.eccentricity;
    if !e.is_finite() || e < 0.0 || !mu.is_finite() || mu <= 0.0 {
        return Err(SimError::Recoverable("wake: degenerate conic"));
    }
    let n = if e < 1.0 {
        let a = elements.semi_major_axis_m;
        if !(a.is_finite() && a > 0.0) {
            return Err(SimError::Recoverable("wake: elliptic a <= 0"));
        }
        libm::sqrt(mu / (a * a * a))
    } else {
        let a = elements.semi_major_axis_m;
        // Parabolic encoding a := −p gives n = sqrt(μ/p³) =
        // sqrt(μ/|a|³); hyperbolic a < 0 the same |a|³.
        if !(a.is_finite() && a < 0.0) {
            return Err(SimError::Recoverable("wake: open conic a >= 0"));
        }
        libm::sqrt(mu / -(a * a * a))
    };
    let dt_epoch = (clock as i128 - epoch_ticks as i128) as f64 / csp_core::TICK_HZ as f64;
    let m = elements.mean_anomaly_epoch_rad + n * dt_epoch;
    let two_pi = 2.0 * core::f64::consts::PI;
    let dt_s = if e < 1.0 {
        // Closed: wrap into (−π, π]-style [0, 2π) then measure forward.
        let mw = m - two_pi * libm::floor(m / two_pi);
        if apoapsis {
            let d = core::f64::consts::PI - mw;
            (if d <= 0.0 { d + two_pi } else { d }) / n
        } else {
            (two_pi - mw) / n
        }
    } else {
        // Open: M is unbounded, zero at periapsis. Apoapsis does not
        // exist; periapsis only when still inbound (m < 0).
        if apoapsis {
            return Ok(None);
        }
        if m >= 0.0 {
            return Ok(None);
        }
        -m / n
    };
    if !(dt_s.is_finite() && dt_s > 0.0) {
        return Ok(None);
    }
    Ok(Some(
        clock
            .saturating_add((dt_s * csp_core::TICK_HZ as f64).ceil() as u64)
            .max(1),
    ))
}

/// §2.10's "altitude crossing on rails" — the closed-form twin of
/// [`resolve_apsis_ticks`] for [`WakeCondition::AtAltitude`]: solve
/// `r = radius_m + above_m` on the conic, convert the anomaly at the
/// crossing to a mean anomaly, and measure the time ahead of the
/// clock the same way.
///
/// `Ok(None)` = no crossing ahead in the requested direction
/// (target outside the conic's radial range, crossing already past
/// on an open conic); `Err` = degenerate inputs (no closed-form
/// answer exists at all).
pub fn resolve_altitude_ticks(
    elements: &KeplerElements,
    mu: f64,
    radius_m: f64,
    above_m: f64,
    ascending: bool,
    epoch_ticks: u64,
    clock: u64,
) -> Result<Option<u64>, SimError> {
    let e = elements.eccentricity;
    let a = elements.semi_major_axis_m;
    if !e.is_finite() || e < 0.0 || !mu.is_finite() || mu <= 0.0 || !radius_m.is_finite() {
        return Err(SimError::Recoverable("wake: degenerate conic"));
    }
    if !(above_m.is_finite() && above_m >= 0.0) {
        return Err(SimError::Recoverable("wake: non-finite altitude"));
    }
    let r_t = radius_m + above_m;
    let dt_epoch = (clock as i128 - epoch_ticks as i128) as f64 / csp_core::TICK_HZ as f64;
    let m_now = elements.mean_anomaly_epoch_rad;
    let two_pi = 2.0 * core::f64::consts::PI;
    // Seconds until mean anomaly `m_t` from the current mean
    // position, forward-only — `None` when the target is behind
    // (open conics: the branch already flown).
    let dt_s = if e < 1.0 {
        // r = a(1 − e·cosE) ⇒ cosE = (a − r)/(a·e). `e ≈ 0` has no
        // defined crossing phase (radius is constant), and a target
        // outside [r_p, r_a] is never crossed.
        if !(a.is_finite() && a > 0.0) {
            return Err(SimError::Recoverable("wake: elliptic a <= 0"));
        }
        if e < 1.0e-9 {
            return Err(SimError::Recoverable("wake: circular conic — no crossing"));
        }
        let c = (a - r_t) / (a * e);
        if c.abs() > 1.0 {
            return Ok(None);
        }
        let n = libm::sqrt(mu / (a * a * a));
        let big_e = libm::acos(c.clamp(-1.0, 1.0)); // ∈ [0, π]
        // E ∈ (0, π) is the ascending branch (dr/dE = a·e·sinE > 0);
        // the descending crossing mirrors at 2π − E.
        let e_cross = if ascending { big_e } else { two_pi - big_e };
        let m_t = e_cross - e * libm::sin(e_cross);
        let m = m_now + n * dt_epoch;
        let mw = m - two_pi * libm::floor(m / two_pi);
        let d = m_t - mw;
        (if d <= 0.0 { d + two_pi } else { d }) / n
    } else {
        // Open conic: r = |a|(e·coshF − 1) ⇒ coshF = (r/|a| + 1)/e.
        // F > 0 is the outbound (ascending) branch, F < 0 inbound;
        // M = e·sinhF − F is 0 at periapsis.
        if !(a.is_finite() && a < 0.0) {
            return Err(SimError::Recoverable("wake: open conic a >= 0"));
        }
        let aa = -a;
        let c = (r_t / aa + 1.0) / e;
        if c < 1.0 {
            // Below periapsis — never reached.
            return Ok(None);
        }
        let n = libm::sqrt(mu / (aa * aa * aa));
        let f = libm::acosh(c);
        let m_t = if ascending {
            e * libm::sinh(f) - f
        } else {
            -(e * libm::sinh(f) - f)
        };
        let m = m_now + n * dt_epoch;
        let d = m_t - m;
        if d <= 0.0 {
            return Ok(None);
        }
        d / n
    };
    if !(dt_s.is_finite() && dt_s > 0.0) {
        return Ok(None);
    }
    Ok(Some(
        clock
            .saturating_add((dt_s * csp_core::TICK_HZ as f64).ceil() as u64)
            .max(1),
    ))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn circular() -> KeplerElements {
        KeplerElements {
            semi_major_axis_m: 6.7e6,
            eccentricity: 0.0,
            inclination_rad: 0.0,
            ascending_node_rad: 0.0,
            arg_periapsis_rad: 0.0,
            mean_anomaly_epoch_rad: 0.0,
            period_s: 0.0,
        }
    }

    /// Elliptic wake math: apoapsis lands a half-period out, periapsis
    /// a full period when starting at periapsis.
    #[test]
    fn elliptic_apsis_resolution() {
        let mu = 3.0e14;
        let el = circular();
        // M0=0 → at periapsis now; next periapsis = one period.
        let pe = resolve_apsis_ticks(&el, mu, 0, 0, false).unwrap().unwrap();
        let ap = resolve_apsis_ticks(&el, mu, 0, 0, true).unwrap().unwrap();
        let a = el.semi_major_axis_m;
        let period_ticks =
            (2.0 * core::f64::consts::PI * libm::sqrt(a * a * a / mu) * csp_core::TICK_HZ as f64)
                .ceil() as u64;
        assert_eq!(pe, period_ticks);
        assert!(
            (period_ticks / 2..=period_ticks / 2 + 1).contains(&ap),
            "apoapsis ≈ half a period out: {ap} vs {period_ticks}"
        );
    }

    /// Hyperbolic wake math: no apoapsis; periapsis only while inbound.
    #[test]
    fn hyperbolic_apsis_resolution() {
        let mu = 3.0e14;
        let el = KeplerElements {
            semi_major_axis_m: -8.0e6,
            eccentricity: 1.3,
            inclination_rad: 0.0,
            ascending_node_rad: 0.0,
            arg_periapsis_rad: 0.0,
            mean_anomaly_epoch_rad: -5.0, // inbound
            period_s: f64::INFINITY,
        };
        assert_eq!(resolve_apsis_ticks(&el, mu, 0, 0, true).unwrap(), None);
        assert!(resolve_apsis_ticks(&el, mu, 0, 0, false).unwrap().is_some());
        // m ≥ 0 → already past periapsis → refused.
        let out = KeplerElements {
            mean_anomaly_epoch_rad: 1.0,
            ..el
        };
        assert_eq!(resolve_apsis_ticks(&out, mu, 0, 0, false).unwrap(), None);
    }
}
