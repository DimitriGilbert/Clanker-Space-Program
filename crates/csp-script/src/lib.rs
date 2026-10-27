//! M9a §2.10 — the deterministic Rhai script host and the
//! [`ControlApi`] it surfaces.
//!
//! # The model (§2.10, verbatim where it matters)
//!
//! - **Tick entry point, not a coroutine.** Each loaded script is
//!   called once per tick as `on_tick(ctx)`. An op-budget abort ends
//!   the call — there is no resumable VM, no continuation. Cross-tick
//!   state is the script's own `persist` map (serialized + hashed by
//!   the sim).
//! - **`ControlApi` is the only way in.** Scripts read telemetry from
//!   `ctx` and request effects through host functions; every mutation
//!   becomes a [`SimCommand`] enqueued for the next tick's drain —
//!   nothing touches authoritative state directly, and scripts pay
//!   the same one-tick command latency as every producer.
//! - **`wait`/`until` are host services, never blocking calls.** A
//!   bare `wait(...)`/`until(...)` in source is a compile-time error
//!   ([`preprocess`]); the legal forms — `wait_ticks`, `wait_until_tick`,
//!   `wait_apoapsis`, `wait_periapsis`, `wait_soi`, `wait_node`,
//!   `cancel_wait` — register a [`WakeCondition`] that the sim resolves
//!   to a `ScriptWake` event.
//! - **Determinism.** The pinned Rhai feature set is the plan's:
//!   no `unchecked` (it would disable `max_operations`), no `only_i32`,
//!   no `f32_float`, `sync` off — plus `no_std`, which bundles the two
//!   determinism pins the sim itself already lives by: `no_time` (the
//!   `BasicTimePackage` — `timestamp()`/`elapsed`/`Instant` — is never
//!   registered, so §2.10's "scripts may not read the wall clock" is a
//!   fact of the build, not a convention) and `num-traits/libm` +
//!   `libm` (every transcendental — `sin`, `cos`, `sqrt`, `exp`, `ln`,
//!   `atan2`, … — resolves to libm, not the platform libm, so guest
//!   math is bit-identical across targets). `on_progress`-counted
//!   `set_max_operations` bounds the op count;
//!   `set_max_string_size`/`set_max_array_size`/`set_max_map_size`
//!   bound per-op memory — together they are the sandbox budget.
//!
//! # The `ctx` contract
//!
//! `on_tick(ctx)` receives a Rhai map: `tick`, `script` (its own
//! [`ScriptId`]), `vessel` (a telemetry map — `id` (the bound
//! vessel's own [`VesselId`], so the script can find itself in
//! `vessels()`), `altitude`, `speed`, `vertical_speed`,
//! `horizontal_speed`, `apoapsis`, `periapsis`, `time_to_apoapsis`,
//! `time_to_periapsis`, `throttle`, `mass`, `residence`, `found`,
//! `pos`, `vel`, `rot` (the orientation quat as `{w, x, y, z}`),
//! `mu`, `body`, `resources`, `parts`, `node`, `node_tick`),
//! `persist` (the live map — writes land in the save). **Optional
//! numerics encode "unknown" as NaN** — `apoapsis` on an open conic,
//! `mass` before the resolver sums it — so `v.is_nan()` is the
//! guest's test for "no value" (Rhai's total-order `==` makes
//! `v != v` false even for NaN — do not use it). Copying a NaN into
//! `persist` faults
//! (a non-finite float is not a primitive — see
//! [`dynamic_to_persist`]). `persist` is the only key read back
//! after the call, and it must stay a map of primitives: indexed
//! writes (`ctx.persist["k"] = v`) and wholesale replacement
//! (`ctx.persist = #{…}`) are both honored; leaving it a non-map is
//! a runtime fault. Extra keys added to `ctx` are inert.
//!
//! Host functions bound on `this`-free names: `set_throttle(f)`,
//! `steer(x, y, z)` (aim the craft's fore axis along the world
//! direction), `steer_quat(w, x, y, z)` (explicit orientation),
//! `hold_attitude()` (SAS-style hold of the current
//! orientation — the sim's attitude model is explicit `SetAttitude`),
//! `stage(node)` (jettison part node — the staging primitive the sim
//! ships), `add_node(tick, pro, norm, rad)` / `delete_node()`,
//! `jettison(node)` (part actions), `deploy(node)` (the decelerator
//! verb — parachutes), `transfer(from, to, resource, amount)`,
//! `add_alarm(tick, label)`, `engage_autopilot(apo_m, pe_m)` /
//! `disengage_autopilot()` (the §2.11 `mission.ascend` verbs),
//! `telemetry_of(id)` / `vessels()` (the frozen trait's read surface
//! — every live vessel's telemetry map), `log(s)`/`print(s)`, and the
//! `wait_*` family above (`wait_altitude(above_m, ascending)`
//! included). The Rust [`ControlApi`] is the same surface
//! `csp-ai`'s autopilot drives — the parity test asserts it.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use csp_core::{DQuat, DVec3, SimError};
use csp_data::ResourceId;
use csp_phys::PartNodeId;
use csp_sim::{
    DEFAULT_SCRIPT_OP_BUDGET, PersistMap, PersistValue, ScriptEngine, ScriptFault, ScriptId,
    ScriptRunReport, ScriptState, SimCommand, SourceClass, TickIo, VesselId, VesselTelemetry,
    WakeCondition,
};
use rhai::{AST, Dynamic, Engine, EvalAltResult, FLOAT, INT, Map, Scope};

/// §2.10's `ControlApi` — the control surface shared by the Rhai
/// host and the Rust autopilot (`csp-ai` drives
/// [`IoControlApi`], the host forwards the same verbs to Rhai).
///
/// Every mutation issues a [`SimCommand`] for the next tick's drain;
/// telemetry reads are snapshots of this tick's state.
///
/// **FROZEN at M9c (§6 row "Interfaces").** The MCP tool surface
/// (`csp.mcp.tools.v1`) maps onto this trait; changing a signature,
/// adding or removing a verb is a deliberate compatibility event that
/// requires a new tool-schema major version. `tests/api_freeze.rs`
/// pins the method set — a drift is a test failure, not a diff
/// reviewer's judgment call.
pub trait ControlApi {
    /// The bound vessel's telemetry snapshot.
    fn telemetry(&mut self) -> VesselTelemetry;
    /// Any vessel's telemetry — `found: false` when unknown.
    fn telemetry_of(&mut self, vessel: VesselId) -> VesselTelemetry;
    /// `SetThrottle` — clamped `[0, 1]` by the apply.
    fn set_throttle(&mut self, fraction: f64);
    /// `SetAttitude` — aim the fore axis along `dir` (world frame),
    /// zero angular rate. The sim's `align_rotation` convention; a
    /// zero `dir` holds the current orientation (the Rhai `steer`
    /// binding's zero-vector fallback is the same).
    fn steer_dir(&mut self, dir: DVec3);
    /// `SetAttitude` holding the current orientation — the closest
    /// the sim ships to a SAS hold.
    fn hold_attitude(&mut self);
    /// `JettisonPart` — the sim's staging/part-action primitive.
    fn jettison(&mut self, node: u64);
    /// `SetManeuver` — schedule a node; also cancels a running burn
    /// per the apply's existing semantics.
    fn add_node(&mut self, tick: u64, dv: DVec3);
    /// `DeleteManeuver`.
    fn delete_node(&mut self);
    /// `TransferResource` — move a resource between vessels in the
    /// same bubble.
    fn transfer_resource(&mut self, to: VesselId, resource: &str, amount: f64);
    /// `SetAlarm` — an absolute-tick `PlayerAlarm` event.
    fn add_alarm(&mut self, tick: u64, label: String);
    /// Register a wake — resolves to a `ScriptWake` event or refuses
    /// at registration (§2.10).
    fn request_wake(&mut self, condition: WakeCondition) -> Result<u64, SimError>;
    /// Drop any pending wake.
    fn cancel_wake(&mut self) -> Result<(), SimError>;
    /// `log`/`print` — to the report's `script_log`.
    fn log(&mut self, line: String);
    /// `DeployPart` — the decelerator deploy (parachute) verb.
    fn deploy(&mut self, node: u64);
    /// `EngageAutopilot` — hand the bound vessel to the installed
    /// ascent driver (§2.11's `mission.ascend` in the plan's
    /// vocabulary): the program is `AutopilotProgram::Ascent` with
    /// the two target altitudes. A script hands the stick to the
    /// Rust driver exactly like the player's engage verb.
    fn engage_autopilot(&mut self, target_apoapsis_m: f64, target_periapsis_m: f64);
    /// `DisengageAutopilot`.
    fn disengage_autopilot(&mut self);
    /// Every live vessel id — `telemetry_of`'s domain (the
    /// `vessels()` guest binding returns the full telemetry array).
    fn vessels(&mut self) -> Vec<VesselId>;
}

/// The plumbing-side [`ControlApi`] over a [`TickIo`] — what
/// `csp-ai`'s [`AutopilotDriver`] uses verbatim.
pub struct IoControlApi<'a> {
    /// The per-tick io.
    pub io: &'a mut dyn TickIo,
    /// `SourceClass` the commands stamp under.
    pub class: SourceClass,
    /// `source_id` for the stamp (vessel id for autopilots, script id
    /// is implicit — wake/log still route through it). A u32 channel
    /// key, not the full id — producers clamp u64 ids with
    /// `.min(u32::MAX)` (see `CommandKey::source_id`); the sim's io
    /// re-derives the stamp from its binding regardless.
    pub source_id: u32,
    /// The bound vessel.
    pub vessel: VesselId,
    /// The bound script (`None` for autopilot callers — wake calls
    /// then refuse and `log` drops, matching the sim's io rule: the
    /// record has no script id to attribute to).
    pub script: Option<ScriptId>,
}

impl ControlApi for IoControlApi<'_> {
    fn telemetry(&mut self) -> VesselTelemetry {
        self.io.telemetry(self.vessel)
    }
    fn telemetry_of(&mut self, vessel: VesselId) -> VesselTelemetry {
        self.io.telemetry(vessel)
    }
    fn set_throttle(&mut self, fraction: f64) {
        self.io.issue(
            self.class,
            self.source_id,
            SimCommand::SetThrottle {
                vessel: self.vessel,
                throttle: fraction,
            },
        );
    }
    fn steer_dir(&mut self, dir: DVec3) {
        let rot = if dir.norm() > 0.0 {
            // `align_rotation(from, to)` — aim the body's +Z thrust
            // axis (the weld convention) along `dir` in world frame.
            csp_sim::maneuver::align_rotation(DVec3::Z, dir / dir.norm())
        } else {
            self.io.telemetry(self.vessel).rot
        };
        self.io.issue(
            self.class,
            self.source_id,
            SimCommand::SetAttitude {
                vessel: self.vessel,
                rot,
                ang_vel: DVec3::ZERO,
            },
        );
    }
    fn hold_attitude(&mut self) {
        let rot = self.io.telemetry(self.vessel).rot;
        self.io.issue(
            self.class,
            self.source_id,
            SimCommand::SetAttitude {
                vessel: self.vessel,
                rot,
                ang_vel: DVec3::ZERO,
            },
        );
    }
    fn jettison(&mut self, node: u64) {
        self.io.issue(
            self.class,
            self.source_id,
            SimCommand::JettisonPart {
                vessel: self.vessel,
                node: PartNodeId(node),
            },
        );
    }
    fn add_node(&mut self, tick: u64, dv: DVec3) {
        self.io.issue(
            self.class,
            self.source_id,
            SimCommand::SetManeuver {
                vessel: self.vessel,
                tick,
                dv_prograde_mps: dv.x,
                dv_normal_mps: dv.y,
                dv_radial_mps: dv.z,
            },
        );
    }
    fn delete_node(&mut self) {
        self.io.issue(
            self.class,
            self.source_id,
            SimCommand::DeleteManeuver {
                vessel: self.vessel,
            },
        );
    }
    fn transfer_resource(&mut self, to: VesselId, resource: &str, amount: f64) {
        self.io.issue(
            self.class,
            self.source_id,
            SimCommand::TransferResource {
                from_vessel: self.vessel,
                to_vessel: to,
                resource: ResourceId(resource.to_string()),
                amount,
            },
        );
    }
    fn add_alarm(&mut self, tick: u64, label: String) {
        self.io.issue(
            self.class,
            self.source_id,
            SimCommand::SetAlarm { tick, label },
        );
    }
    fn request_wake(&mut self, condition: WakeCondition) -> Result<u64, SimError> {
        let Some(id) = self.script else {
            return Err(SimError::Recoverable("wake: autopilot io has no script"));
        };
        self.io.request_wake(id, &condition)
    }
    fn cancel_wake(&mut self) -> Result<(), SimError> {
        let Some(id) = self.script else {
            return Err(SimError::Recoverable("wake: autopilot io has no script"));
        };
        self.io.cancel_wake(id)
    }
    fn log(&mut self, line: String) {
        // The line rides `script_log` keyed by `ScriptId` — an
        // autopilot caller has none, and `source_id` is the
        // vessel-derived channel key, NOT a script id: attributing
        // the line to `ScriptId(source_id)` would fabricate a script
        // that never existed (and could collide with a real one).
        // `log` returns `()` — the trait is frozen at M9c — so the
        // refusal is a drop, the same honest-absence answer
        // `request_wake` gives through its `Result`.
        let Some(id) = self.script else { return };
        self.io.log_line(id, &line);
    }
    fn deploy(&mut self, node: u64) {
        self.io.issue(
            self.class,
            self.source_id,
            SimCommand::DeployPart {
                vessel: self.vessel,
                node: PartNodeId(node),
            },
        );
    }
    fn engage_autopilot(&mut self, target_apoapsis_m: f64, target_periapsis_m: f64) {
        self.io.issue(
            self.class,
            self.source_id,
            SimCommand::EngageAutopilot {
                vessel: self.vessel,
                program: csp_sim::AutopilotProgram::Ascent {
                    target_apoapsis_m,
                    target_periapsis_m,
                },
            },
        );
    }
    fn disengage_autopilot(&mut self) {
        self.io.issue(
            self.class,
            self.source_id,
            SimCommand::DisengageAutopilot {
                vessel: self.vessel,
            },
        );
    }
    fn vessels(&mut self) -> Vec<VesselId> {
        self.io.vessels()
    }
}

/// The set of guest-callable names **and their arities**, stated as
/// a literal argument list per name — the parity test asserts each
/// call actually resolves through the registered bindings (a missing
/// function OR a wrong arity is a `FunctionNotFound` fault), so this
/// list matches the documented `ControlApi` surface one-for-one.
///
/// **FROZEN at M9c** — pinned verbatim by `tests/api_freeze.rs`;
/// appending a name is a `csp.mcp.tools` major-version event.
pub const CONTROL_API_SURFACE: &[(&str, &str)] = &[
    ("set_throttle", "0.0"),
    ("steer", "0.0, 1.0, 0.0"),
    ("steer_quat", "1.0, 0.0, 0.0, 0.0"),
    ("hold_attitude", ""),
    ("stage", "0"),
    ("jettison", "0"),
    ("add_node", "0, 0.0, 0.0, 0.0"),
    ("delete_node", ""),
    ("transfer", "0, \"fuel\", 0.0"),
    ("add_alarm", "0, \"x\""),
    ("log", "\"x\""),
    ("print", "\"x\""),
    ("wait_ticks", "1"),
    ("wait_until_tick", "1"),
    ("wait_apoapsis", ""),
    ("wait_periapsis", ""),
    ("wait_soi", ""),
    ("wait_node", ""),
    ("wait_altitude", "80000.0, true"),
    ("cancel_wait", ""),
    ("deploy", "0"),
    ("engage_autopilot", "80000.0, 80000.0"),
    ("disengage_autopilot", ""),
    ("telemetry_of", "0"),
    ("vessels", ""),
];

// ---------------------------------------------------------------------------
// Preprocessor — §2.10's "bare wait/until in a tick is a compile error"
// ---------------------------------------------------------------------------

/// Reject the blocking forms at compile time: `wait(...)` and
/// `until(...)` as *free* call tokens are compile errors — the legal
/// wakes are `wait_ticks`/`wait_until_tick`/`wait_apoapsis`/
/// `wait_periapsis`/`wait_soi`/`wait_node`. The scan is token-aware,
/// not a substring search: comments (`//`, `/* */`), string literals
/// (`"..."`, `#"..."#` raw strings), char literals (`'x'`), and the
/// literal spans of backtick templates (their `${…}` interpolations
/// ARE scanned — interpolated code is real code) never match, and a
/// `wait`/`until` immediately following `.` is a member call
/// (`ctx.until(...)`, `x.wait(...)`) — Rhai resolves those against
/// the receiver's methods, never against a free blocking call.
pub fn preprocess(source: &str) -> Result<(), String> {
    Scan {
        b: source.as_bytes(),
    }
    .code(0, false, 0)
    .map(|_| ())
}

/// The gate's nesting bound: each `${…}` interpolation inside a
/// backtick template descends one native stack frame in
/// [`Scan::code`], so an unbounded `` "`${" * n `` source would
/// overflow the host's stack (a `SIGSEGV`/abort no `catch_unwind`
/// covers) before Rhai's own limits ever see the script. The cap is
/// twice Rhai's default expression-depth limit (32) — far past any
/// legitimate template — and rejects as an ordinary compile error.
const MAX_TEMPLATE_DEPTH: usize = 64;

/// The preprocessor's scanner state — `code` walks real tokens
/// (skipping comments and literals), stopping early at a `}` that
/// closes an interpolation when `brace_stop` is set.
struct Scan<'a> {
    b: &'a [u8],
}

impl Scan<'_> {
    /// Scan code from `i`. Returns the index where scanning stopped —
    /// EOF, or the `}` closing a `${…}` interpolation when
    /// `brace_stop`. `Err` is the §2.10 gate's rejection.
    /// `template_depth` counts enclosing `${…}` interpolations — the
    /// recursion is depth-capped at [`MAX_TEMPLATE_DEPTH`] so hostile
    /// source cannot overflow the host stack.
    // The lexical state machine needs every branch in one place —
    // splitting it would hide the token-boundary invariants.
    #[allow(clippy::too_many_lines)]
    fn code(&self, mut i: usize, brace_stop: bool, template_depth: usize) -> Result<usize, String> {
        let b = self.b;
        // The last significant (non-whitespace, non-comment) byte seen
        // — `Some(b'.')` means the next identifier is a member access.
        let mut last_sig = 0_u8;
        // Brace depth inside a `${…}` interpolation — a `}` only stops
        // the scan when it closes the interpolation itself.
        let mut depth = 0_usize;
        while i < b.len() {
            match b[i] {
                // Line comment — to the newline.
                b'/' if b.get(i + 1) == Some(&b'/') => {
                    while i < b.len() && b[i] != b'\n' {
                        i += 1;
                    }
                }
                // Block comment — to `*/` (or EOF; the real parser
                // diagnoses an unterminated comment).
                b'/' if b.get(i + 1) == Some(&b'*') => {
                    i += 2;
                    while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                        i += 1;
                    }
                    i = (i + 2).min(b.len());
                }
                // `"…"` string literal — `\` escapes.
                b'"' => {
                    i += 1;
                    while i < b.len() && b[i] != b'"' {
                        i += if b[i] == b'\\' { 2 } else { 1 };
                    }
                    i += 1;
                    last_sig = b'"';
                }
                // `#"…"#`-style raw string: one or more `#` then `"`,
                // closed by `"` followed by the same `#` count. A `#`
                // NOT opening a raw string (`#{` map literal) is just
                // a significant byte.
                b'#' => {
                    let mut j = i;
                    while b.get(j) == Some(&b'#') {
                        j += 1;
                    }
                    if b.get(j) == Some(&b'"') {
                        let hashes = j - i;
                        j += 1;
                        while j < b.len() {
                            if b[j] == b'"'
                                && j + 1 + hashes <= b.len()
                                && b[j + 1..j + 1 + hashes].iter().all(|&c| c == b'#')
                            {
                                j += 1 + hashes;
                                break;
                            }
                            j += 1;
                        }
                        i = j;
                    } else {
                        last_sig = b'#';
                        i += 1;
                    }
                }
                // `'x'` char literal — `\` escapes.
                b'\'' => {
                    i += 1;
                    while i < b.len() && b[i] != b'\'' {
                        i += if b[i] == b'\\' { 2 } else { 1 };
                    }
                    i += 1;
                    last_sig = b'\'';
                }
                // `` ` `` interpolated string — literal text except
                // `${…}` regions, which scan as code (an interpolated
                // `wait(...)` is a real blocking call and must reject).
                b'`' => {
                    i += 1;
                    while i < b.len() && b[i] != b'`' {
                        match b[i] {
                            b'\\' => {
                                i += 2;
                            }
                            b'$' if b.get(i + 1) == Some(&b'{') => {
                                if template_depth >= MAX_TEMPLATE_DEPTH {
                                    return Err(format!(
                                        "template `${{…}}` nested deeper than \
                                         {MAX_TEMPLATE_DEPTH} levels"
                                    ));
                                }
                                // `code` stops AT the closing `}`.
                                i = self.code(i + 2, true, template_depth + 1)? + 1;
                            }
                            _ => {
                                i += 1;
                            }
                        }
                    }
                    i += 1;
                    last_sig = b'"';
                }
                // Identifiers — the only place the gate can fire.
                c if c.is_ascii_alphabetic() || c == b'_' => {
                    let start = i;
                    while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                        i += 1;
                    }
                    let name = &b[start..i];
                    if (name == b"wait" || name == b"until") && last_sig != b'.' {
                        // The call check: `(` after optional whitespace.
                        let mut j = i;
                        while j < b.len() && b[j].is_ascii_whitespace() {
                            j += 1;
                        }
                        if b.get(j) == Some(&b'(') {
                            let name = String::from_utf8_lossy(name);
                            return Err(format!(
                                "bare `{name}(...)` blocks inside a tick — use the \
                                 wake services (wait_ticks / wait_until_tick / \
                                 wait_apoapsis / wait_periapsis / wait_soi / wait_node)"
                            ));
                        }
                    }
                    last_sig = b[i - 1];
                }
                b'{' => {
                    depth += 1;
                    last_sig = b'{';
                    i += 1;
                }
                b'}' if brace_stop && depth == 0 => {
                    return Ok(i);
                }
                b'}' => {
                    depth = depth.saturating_sub(1);
                    last_sig = b'}';
                    i += 1;
                }
                c => {
                    if !c.is_ascii_whitespace() {
                        last_sig = c;
                    }
                    i += 1;
                }
            }
        }
        Ok(i)
    }
}

// ---------------------------------------------------------------------------
// Persist <-> Rhai (§2.10's primitive map)
// ---------------------------------------------------------------------------

/// Convert a Rhai `Dynamic` to a [`PersistValue`] — the plan's "map
/// of primitives": bool / INT / FLOAT / String (+char). Anything else
/// (arrays, nested maps, timestamps) is refused loudly rather than
/// silently degrading in the save. Note the trap this creates with
/// the `ctx.vessel` encoding: optional numerics surface to the guest
/// as **NaN** ([`RhaiHost::telemetry_to_map`]), and a non-finite
/// float refuses here — `ctx.persist["apo"] = ctx.vessel.apoapsis`
/// faults ("unsupported or non-finite value", a strike) whenever the
/// apsis is unknown. A script that may copy telemetry into persist
/// must gate it first — `v.is_nan()` is the guest's test for the
/// sentinel (Rhai's total-order `==` makes `v != v` useless for it).
pub fn dynamic_to_persist(v: &Dynamic) -> Option<PersistValue> {
    if v.is_bool() {
        v.as_bool().ok().map(PersistValue::Bool)
    } else if v.is_int() {
        v.as_int().ok().map(PersistValue::Int)
    } else if v.is_float() {
        let f = v.as_float().ok()?;
        f.is_finite().then_some(PersistValue::Float(f))
    } else if v.is_string() {
        v.clone().into_string().ok().map(PersistValue::Text)
    } else if v.is_char() {
        v.as_char().ok().map(|c| PersistValue::Text(c.to_string()))
    } else {
        None
    }
}

/// A [`PersistValue`] back to a Rhai `Dynamic`.
pub fn persist_to_dynamic(v: &PersistValue) -> Dynamic {
    match v {
        PersistValue::Bool(b) => Dynamic::from(*b),
        PersistValue::Int(i) => Dynamic::from(*i),
        PersistValue::Float(f) => Dynamic::from_float(*f),
        PersistValue::Text(s) => Dynamic::from(s.clone()),
    }
}

/// Build the `ctx.persist` Rhai map from the script's state.
fn persist_map_to_rhai(persist: &PersistMap) -> Map {
    persist
        .iter()
        .map(|(k, v)| (k.clone().into(), persist_to_dynamic(v)))
        .collect()
}

/// Write the Rhai-side persist map back — rejects any non-primitive
/// the script smuggled in (the fault is a runtime error → strike).
fn rhai_map_to_persist(map: &Map) -> Result<PersistMap, String> {
    let mut out = PersistMap::new();
    for (k, v) in map {
        let pv = dynamic_to_persist(v)
            .ok_or_else(|| format!("persist[{k}]: unsupported or non-finite value"))?;
        out.insert(k.to_string(), pv);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Effects + host
// ---------------------------------------------------------------------------

/// §2.10 sandbox memory ceilings. `set_max_operations` bounds only the
/// op *count* — a guest `s = s + s` doubling loop allocates 2ⁿ bytes
/// for ~n counted ops, so uncapped strings/arrays/maps OOM-kill the
/// host inside the op budget (a non-deterministic abort — the worst
/// fault class in the determinism domain). Rhai 1.27's defaults are
/// unlimited; these pin real bounds. Over-limit growth raises a
/// runtime fault the strike classifier already handles. Sizes are
/// generous against the honest workload: `log` lines and telemetry
/// strings are small, and `persist` is "a map of primitives".
/// The string cap is in bytes.
const MAX_SCRIPT_STRING_SIZE: usize = 1 << 20; // 1 MiB
/// Max array elements — a tick's effects are dozens, not thousands.
const MAX_SCRIPT_ARRAY_SIZE: usize = 10_000;
/// Max object-map entries — `ctx`/`persist` carry tens of keys.
const MAX_SCRIPT_MAP_SIZE: usize = 10_000;

/// The tick's recorded effects — the registered Rhai functions write
/// here; after the call the host drains them into the [`TickIo`]. The
/// `Rc<RefCell<_>>` is Rhai's required capture discipline — never
/// shared across threads (the engine is `sync`-free).
#[derive(Default)]
struct ScriptEffects {
    /// Enqueued commands (production order → FIFO).
    commands: Vec<SimCommand>,
    /// The wake intent — `Some(Ok(cond))` register, `Some(Err(()))`
    /// cancel, `None` untouched.
    wake: Option<Result<WakeCondition, ()>>,
    /// log/print lines.
    logs: Vec<String>,
}

/// The deterministic Rhai host — one shared [`Engine`] (construction
/// config: `max_operations` is forwarded from the world's per-save
/// budget via [`ScriptEngine::set_op_budget`]), one AST cache keyed
/// by [`ScriptId`] (scratch — a restore recompiles from the
/// serialized `source`).
pub struct RhaiHost {
    /// The configured engine — built under the pinned `no_std`
    /// feature set, so `Engine::new`'s `StandardPackage` omits
    /// `BasicTimePackage` entirely (`timestamp`/`elapsed`/`Instant`
    /// do not exist for the guest — §2.10's no-wall-clock rule is a
    /// build fact) and every transcendental binds to libm. Nothing
    /// here re-adds any of it.
    engine: Engine,
    /// The `on_progress` tally — reset per call, read into the
    /// [`ScriptRunReport`].
    ops: Rc<RefCell<u64>>,
    /// Compiled scripts — pure functions of `ScriptState::source`.
    asts: BTreeMap<ScriptId, AST>,
    /// The per-tick binding snapshot the host closures read — set
    /// fresh by every `run_tick` before `call_fn` (the functions are
    /// registered ONCE at construction, so this cell is how a tick's
    /// vessel/rotation/telemetry reach them).
    bound: Rc<RefCell<Bound>>,
    /// The per-tick effects cell the closures write — reset by every
    /// `run_tick` before `call_fn` and drained after it.
    fx: Rc<RefCell<ScriptEffects>>,
}

impl RhaiHost {
    /// Build the host with the plan's feature set — a sane default
    /// budget until the world forwards its own.
    ///
    /// # The libm overrides
    ///
    /// `no_std` bundles `num-traits/libm`, but the impl num-traits
    /// picks is decided by *num-traits'* own feature resolution —
    /// and `num-traits/std` is pulled elsewhere in this workspace
    /// (egui/wgpu), so workspace-unified builds silently resolve
    /// `Float::exp` &co. back to the platform libm. The guest's
    /// transcendentals are therefore shadowed here with explicit
    /// libm implementations — engine-registered functions take
    /// precedence over the package's, pinning the math on every
    /// build configuration.
    pub fn new() -> Self {
        let mut engine = Engine::new();
        Self::register_libm_math(&mut engine);
        engine.set_max_operations(DEFAULT_SCRIPT_OP_BUDGET);
        // The op budget bounds op count only — cap guest memory too,
        // or a string/array doubling loop OOMs the host in ~34 ops
        // (rhai 1.27's defaults are unlimited).
        engine.set_max_string_size(MAX_SCRIPT_STRING_SIZE);
        engine.set_max_array_size(MAX_SCRIPT_ARRAY_SIZE);
        engine.set_max_map_size(MAX_SCRIPT_MAP_SIZE);
        let ops = Rc::new(RefCell::new(0_u64));
        let c = Rc::clone(&ops);
        engine.on_progress(move |n| {
            *c.borrow_mut() = n;
            None // keep running — `max_operations` does the abort
        });
        // The ControlApi functions register ONCE here — ~25
        // registrations, not ~25 × every script tick. They read the
        // tick's vessel/rotation/telemetry through `bound` and write
        // effects into `fx`; `run_tick` installs both cells before
        // `call_fn` and the closures only ever run inside it.
        let bound = Rc::new(RefCell::new(Bound::placeholder()));
        let fx = Rc::new(RefCell::new(ScriptEffects::default()));
        Self::bind_fns(&mut engine, Rc::clone(&fx), Rc::clone(&bound));
        Self {
            engine,
            ops,
            asts: BTreeMap::new(),
            bound,
            fx,
        }
    }

    /// Shadow the guest's transcendentals with libm — see [`Self::new`].
    /// Every function whose platform implementation can differ
    /// bit-for-bit across libms is covered; exact IEEE ops
    /// (floor/ceil/trunc/fract/abs/min/max, +−×÷) are identical on
    /// every platform and are deliberately left alone.
    fn register_libm_math(engine: &mut Engine) {
        engine.register_fn("sin", |x: FLOAT| libm::sin(x));
        engine.register_fn("cos", |x: FLOAT| libm::cos(x));
        engine.register_fn("tan", |x: FLOAT| libm::tan(x));
        engine.register_fn("asin", |x: FLOAT| libm::asin(x));
        engine.register_fn("acos", |x: FLOAT| libm::acos(x));
        engine.register_fn("atan", |x: FLOAT| libm::atan(x));
        // Rhai spells atan2 `atan(y, x)`.
        engine.register_fn("atan", |y: FLOAT, x: FLOAT| libm::atan2(y, x));
        engine.register_fn("sinh", |x: FLOAT| libm::sinh(x));
        engine.register_fn("cosh", |x: FLOAT| libm::cosh(x));
        engine.register_fn("tanh", |x: FLOAT| libm::tanh(x));
        engine.register_fn("asinh", |x: FLOAT| libm::asinh(x));
        engine.register_fn("acosh", |x: FLOAT| libm::acosh(x));
        engine.register_fn("atanh", |x: FLOAT| libm::atanh(x));
        engine.register_fn("hypot", |x: FLOAT, y: FLOAT| libm::hypot(x, y));
        engine.register_fn("exp", |x: FLOAT| libm::exp(x));
        engine.register_fn("ln", |x: FLOAT| libm::log(x));
        // `log(x)` is log10 in the package; `log(x, base)` is ln(x)/ln(base).
        engine.register_fn("log", |x: FLOAT| libm::log10(x));
        engine.register_fn("log", |x: FLOAT, base: FLOAT| {
            libm::log(x) / libm::log(base)
        });
        // `**` — `powf` for float exponent, a repeated-squaring powi
        // for integer exponent (libm ships no powi; this loop is the
        // same everywhere).
        engine.register_fn("**", |x: FLOAT, y: FLOAT| libm::pow(x, y));
        engine.register_fn("**", |x: FLOAT, y: INT| {
            let mut n = y.unsigned_abs();
            let mut base = x;
            let mut acc = 1.0_f64;
            while n > 0 {
                if n & 1 == 1 {
                    acc *= base;
                }
                base *= base;
                n >>= 1;
            }
            if y < 0 { 1.0 / acc } else { acc }
        });
    }

    /// Telemetry → the `ctx.vessel` map — one builder for every
    /// vessel-shaped map the guest sees (`ctx.vessel`, `vessels()`,
    /// `telemetry_of`), so the key schema cannot drift between call
    /// sites. Optional numerics encode `None` as **NaN** — the
    /// guest-visible "unknown" sentinel (a script tests `v.is_nan()`;
    /// storing a NaN back into `ctx.persist` faults at write-back,
    /// see [`dynamic_to_persist`]). `id` is the vessel's own id —
    /// what `transfer(to, …)`/`telemetry_of` take — included so a
    /// script can find itself inside `vessels()`.
    fn telemetry_to_map(t: &VesselTelemetry, id: VesselId) -> Map {
        let mut m = Map::new();
        let f = |v: Option<f64>| v.map_or(Dynamic::from_float(f64::NAN), Dynamic::from_float);
        m.insert("id".into(), Dynamic::from_int(id.0 as INT));
        m.insert("tick".into(), Dynamic::from_int(t.tick as INT));
        m.insert("found".into(), Dynamic::from(t.found));
        m.insert(
            "residence".into(),
            Dynamic::from(
                match t.residence {
                    csp_sim::TelemetryResidence::OnRails => "rails",
                    csp_sim::TelemetryResidence::InBubble => "bubble",
                    csp_sim::TelemetryResidence::SurfaceLatched => "latched",
                    csp_sim::TelemetryResidence::Missing => "missing",
                }
                .to_string(),
            ),
        );
        m.insert("altitude".into(), f(t.altitude_m));
        m.insert("apoapsis".into(), f(t.apoapsis_altitude_m));
        m.insert("periapsis".into(), f(t.periapsis_altitude_m));
        m.insert("time_to_apoapsis".into(), f(t.time_to_apoapsis_s));
        m.insert("time_to_periapsis".into(), f(t.time_to_periapsis_s));
        m.insert("speed".into(), Dynamic::from_float(t.speed_mps as FLOAT));
        m.insert(
            "vertical_speed".into(),
            Dynamic::from_float(t.vertical_speed_mps as FLOAT),
        );
        m.insert(
            "horizontal_speed".into(),
            Dynamic::from_float(t.horizontal_speed_mps as FLOAT),
        );
        m.insert("throttle".into(), Dynamic::from_float(t.throttle as FLOAT));
        // Same NaN-for-`None` encoding as `f` above — `mass` is the
        // other optional numeric (unknown off-bubble or when the
        // resolver could not sum the weld).
        m.insert(
            "mass".into(),
            t.mass_kg
                .map_or(Dynamic::from_float(f64::NAN as FLOAT), |v| {
                    Dynamic::from_float(v as FLOAT)
                }),
        );
        m.insert("pos".into(), vec_map(t.pos_m));
        m.insert("vel".into(), vec_map(t.vel_mps));
        // The orientation quat `{w, x, y, z}` — what `hold_attitude()`
        // holds and `steer_quat` sets.
        let mut q = Map::new();
        q.insert("w".into(), Dynamic::from_float(t.rot.w as FLOAT));
        q.insert("x".into(), Dynamic::from_float(t.rot.x as FLOAT));
        q.insert("y".into(), Dynamic::from_float(t.rot.y as FLOAT));
        q.insert("z".into(), Dynamic::from_float(t.rot.z as FLOAT));
        m.insert("rot".into(), Dynamic::from_map(q));
        // Frame constants — the circular-orbit math a transfer or
        // circularization script does needs μ; `body` names the
        // frame's central body (`()` when unknown).
        m.insert("mu".into(), f(t.mu_m3_per_s2));
        m.insert(
            "body".into(),
            t.body
                .as_ref()
                .map_or(Dynamic::UNIT, |b| Dynamic::from(b.as_str().to_string())),
        );
        // The vessel ledger — `resources` (id → units aboard) and
        // `parts` (`#{node, part}` — the ids `stage`/`jettison`/
        // `deploy` target).
        let resources: Map = t
            .resources
            .iter()
            .map(|(k, v)| (k.clone().into(), Dynamic::from_float(*v as FLOAT)))
            .collect();
        m.insert("resources".into(), Dynamic::from_map(resources));
        let parts: rhai::Array = t
            .parts
            .iter()
            .map(|(node, part)| {
                let mut p = Map::new();
                p.insert("node".into(), Dynamic::from_int(*node as INT));
                p.insert("part".into(), Dynamic::from(part.clone()));
                Dynamic::from_map(p)
            })
            .collect();
        m.insert("parts".into(), Dynamic::from_array(parts));
        if let Some((nt, dv)) = t.maneuver_node {
            m.insert("node_tick".into(), Dynamic::from_int(nt as INT));
            m.insert("node".into(), vec_map(dv));
        }
        m
    }

    /// Register every `ControlApi` host function on `engine` — ONCE
    /// per engine, at construction. The closures read the tick's
    /// binding (vessel, hold-rotation, the telemetry tables) through
    /// `bound` and record effects into `fx`; `run_tick` installs both
    /// cells before `call_fn`, which is the only place the fns can
    /// run. (Same-name `register_fn` replaces rather than stacks, so
    /// the old per-tick re-registration was cost only — but cost it
    /// was.)
    #[allow(clippy::too_many_lines)]
    fn bind_fns(engine: &mut Engine, fx: Rc<RefCell<ScriptEffects>>, bound: Rc<RefCell<Bound>>) {
        let push = |fx: &Rc<RefCell<ScriptEffects>>, c: SimCommand| {
            fx.borrow_mut().commands.push(c);
        };
        let log = |fx: &Rc<RefCell<ScriptEffects>>, s: Dynamic| {
            fx.borrow_mut().logs.push(s.to_string());
        };
        let wake = |fx: &Rc<RefCell<ScriptEffects>>, r: Result<WakeCondition, ()>| {
            fx.borrow_mut().wake = Some(r);
        };

        let (b, fx0) = (bound.clone(), fx.clone());
        engine.register_fn("set_throttle", move |x: FLOAT| {
            push(
                &fx0,
                SimCommand::SetThrottle {
                    vessel: b.borrow().vessel,
                    throttle: x,
                },
            );
        });
        let (b, fx0) = (bound.clone(), fx.clone());
        engine.register_fn("steer", move |x: FLOAT, y: FLOAT, z: FLOAT| {
            let bound = b.borrow();
            let d = DVec3::new(x, y, z);
            let n = d.norm();
            let rot = if n > 0.0 {
                // `align_rotation(from, to)` — body +Z (the thrust
                // axis) onto the world direction.
                csp_sim::maneuver::align_rotation(DVec3::Z, d / n)
            } else {
                // Zero direction = hold: the same verb as
                // `IoControlApi::steer_dir`, whose fallback is the
                // telemetry rotation — `Bound::rot` is exactly that
                // snapshot.
                bound.rot
            };
            push(
                &fx0,
                SimCommand::SetAttitude {
                    vessel: bound.vessel,
                    rot,
                    ang_vel: DVec3::ZERO,
                },
            );
        });
        let (b, fx0) = (bound.clone(), fx.clone());
        engine.register_fn(
            "steer_quat",
            move |w: FLOAT, x: FLOAT, y: FLOAT, z: FLOAT| {
                // `DQuat::new` is (x, y, z, w) — the guest's convention
                // is scalar-first `(w, x, y, z)`, matching `ctx.vessel.rot`.
                push(
                    &fx0,
                    SimCommand::SetAttitude {
                        vessel: b.borrow().vessel,
                        rot: DQuat::new(x, y, z, w),
                        ang_vel: DVec3::ZERO,
                    },
                );
            },
        );
        // `hold_attitude()` — the ControlApi's 0-arg "hold current":
        // `SetAttitude` at the orientation the tick's telemetry
        // snapshot reports (`Bound::rot`).
        let (b, fx0) = (bound.clone(), fx.clone());
        engine.register_fn("hold_attitude", move || {
            let bound = b.borrow();
            push(
                &fx0,
                SimCommand::SetAttitude {
                    vessel: bound.vessel,
                    rot: bound.rot,
                    ang_vel: DVec3::ZERO,
                },
            );
        });
        let (b, fx0) = (bound.clone(), fx.clone());
        engine.register_fn("stage", move |node: INT| {
            push(
                &fx0,
                SimCommand::JettisonPart {
                    vessel: b.borrow().vessel,
                    node: PartNodeId(node as u64),
                },
            );
        });
        let (b, fx0) = (bound.clone(), fx.clone());
        engine.register_fn("jettison", move |node: INT| {
            push(
                &fx0,
                SimCommand::JettisonPart {
                    vessel: b.borrow().vessel,
                    node: PartNodeId(node as u64),
                },
            );
        });
        let (b, fx0) = (bound.clone(), fx.clone());
        engine.register_fn(
            "add_node",
            move |tick: INT, pro: FLOAT, norm: FLOAT, rad: FLOAT| {
                push(
                    &fx0,
                    SimCommand::SetManeuver {
                        vessel: b.borrow().vessel,
                        tick: tick as u64,
                        dv_prograde_mps: pro,
                        dv_normal_mps: norm,
                        dv_radial_mps: rad,
                    },
                );
            },
        );
        let (b, fx0) = (bound.clone(), fx.clone());
        engine.register_fn("delete_node", move || {
            push(
                &fx0,
                SimCommand::DeleteManeuver {
                    vessel: b.borrow().vessel,
                },
            );
        });
        let (b, fx0) = (bound.clone(), fx.clone());
        engine.register_fn("transfer", move |to: INT, resource: &str, amount: FLOAT| {
            push(
                &fx0,
                SimCommand::TransferResource {
                    from_vessel: b.borrow().vessel,
                    to_vessel: VesselId(to as u64),
                    resource: ResourceId(resource.to_string()),
                    amount,
                },
            );
        });
        let fx0 = fx.clone();
        engine.register_fn("add_alarm", move |tick: INT, label: &str| {
            fx0.borrow_mut().commands.push(SimCommand::SetAlarm {
                tick: tick as u64,
                label: label.to_string(),
            });
        });
        let fx0 = fx.clone();
        engine.register_fn("log", move |s: Dynamic| log(&fx0, s));
        let fx0 = fx.clone();
        engine.register_fn("print", move |s: Dynamic| log(&fx0, s));
        let fx0 = fx.clone();
        engine.register_fn("wait_ticks", move |n: INT| {
            wake(&fx0, Ok(WakeCondition::InTicks(n.max(0) as u64)));
        });
        let fx0 = fx.clone();
        engine.register_fn("wait_until_tick", move |t: INT| {
            wake(&fx0, Ok(WakeCondition::AtTick(t.max(0) as u64)));
        });
        let fx0 = fx.clone();
        engine.register_fn("wait_apoapsis", move || {
            wake(&fx0, Ok(WakeCondition::AtApoapsis));
        });
        let fx0 = fx.clone();
        engine.register_fn("wait_periapsis", move || {
            wake(&fx0, Ok(WakeCondition::AtPeriapsis));
        });
        let fx0 = fx.clone();
        engine.register_fn("wait_soi", move || {
            wake(&fx0, Ok(WakeCondition::AtNextSoiTransition));
        });
        let fx0 = fx.clone();
        engine.register_fn("wait_node", move || {
            wake(&fx0, Ok(WakeCondition::AtManeuverNode));
        });
        let fx0 = fx.clone();
        engine.register_fn("cancel_wait", move || {
            wake(&fx0, Err(()));
        });
        // `deploy(node)` — the decelerator verb (`DeployPart`): the
        // parachute action `stage`/`jettison` don't cover.
        let (b, fx0) = (bound.clone(), fx.clone());
        engine.register_fn("deploy", move |node: INT| {
            push(
                &fx0,
                SimCommand::DeployPart {
                    vessel: b.borrow().vessel,
                    node: PartNodeId(node as u64),
                },
            );
        });
        // `engage_autopilot(apo_m, pe_m)` / `disengage_autopilot()` —
        // the §2.11 `mission.ascend` verb: a script hands the stick
        // to the installed Rust driver (and takes it back). The
        // commands are control-gated like every other issue — the
        // player's automation passes under a vacant lease, and the
        // residence gate rejects honestly (latched/rails).
        let (b, fx0) = (bound.clone(), fx.clone());
        engine.register_fn("engage_autopilot", move |apo: FLOAT, pe: FLOAT| {
            push(
                &fx0,
                SimCommand::EngageAutopilot {
                    vessel: b.borrow().vessel,
                    program: csp_sim::AutopilotProgram::Ascent {
                        target_apoapsis_m: apo,
                        target_periapsis_m: pe,
                    },
                },
            );
        });
        let (b, fx0) = (bound.clone(), fx.clone());
        engine.register_fn("disengage_autopilot", move || {
            push(
                &fx0,
                SimCommand::DisengageAutopilot {
                    vessel: b.borrow().vessel,
                },
            );
        });
        // `wait_altitude(above_m, ascending)` — the §2.10 altitude
        // wake (closed-form crossing of the frame's conic).
        let fx0 = fx.clone();
        engine.register_fn("wait_altitude", move |above_m: FLOAT, ascending: bool| {
            wake(&fx0, Ok(WakeCondition::AtAltitude { above_m, ascending }));
        });
        // `telemetry_of(id)` / `vessels()` — the read surface the
        // frozen trait already declared but no guest name reached
        // (report-18 F4): precomputed per tick into `Bound` because a
        // Rhai callback can't reach the borrowed `io`. `telemetry_of`
        // on an unknown id returns `#{found: false}` — the same shape
        // `ctx.vessel` reports for a dead vessel.
        let b = bound.clone();
        engine.register_fn("telemetry_of", move |id: INT| -> Dynamic {
            b.borrow()
                .telemetry_by_id
                .get(&(id.max(0) as u64))
                .map_or_else(
                    || {
                        let mut m = Map::new();
                        m.insert("found".into(), Dynamic::from(false));
                        Dynamic::from_map(m)
                    },
                    |m| Dynamic::from_map(m.clone()),
                )
        });
        let b = bound.clone();
        engine.register_fn("vessels", move || -> Dynamic {
            Dynamic::from_array(b.borrow().telemetry_list.as_ref().clone())
        });
    }
}

/// What the host's registered functions read per tick — installed
/// into the shared cell by `run_tick` before `call_fn` (the fns
/// register once at construction, so this is the per-tick binding).
struct Bound {
    /// The bound vessel.
    vessel: VesselId,
    /// The bound vessel's orientation from this tick's telemetry
    /// snapshot — what `hold_attitude()` holds.
    rot: DQuat,
    /// Every vessel's telemetry map keyed by id — `telemetry_of`'s
    /// lookup table, precomputed this tick.
    telemetry_by_id: Rc<BTreeMap<u64, Map>>,
    /// Every vessel's telemetry map (with an `id` key) in arena
    /// order — `vessels()`'s return array.
    telemetry_list: Rc<rhai::Array>,
}

impl Bound {
    /// The construction-time cell content — never observed: every
    /// `run_tick` installs the tick's real bound before `call_fn`,
    /// and the registered fns only exist inside that call.
    fn placeholder() -> Self {
        Self {
            vessel: VesselId(0),
            rot: DQuat::IDENTITY,
            telemetry_by_id: Rc::new(BTreeMap::new()),
            telemetry_list: Rc::new(rhai::Array::new()),
        }
    }
}

impl Default for RhaiHost {
    fn default() -> Self {
        Self::new()
    }
}

fn vec_map(v: DVec3) -> Dynamic {
    let mut m = Map::new();
    m.insert("x".into(), Dynamic::from_float(v.x as FLOAT));
    m.insert("y".into(), Dynamic::from_float(v.y as FLOAT));
    m.insert("z".into(), Dynamic::from_float(v.z as FLOAT));
    Dynamic::from_map(m)
}

impl ScriptEngine for RhaiHost {
    fn prepare(&mut self, id: ScriptId, source: &str) -> Result<(), String> {
        preprocess(source)?;
        let ast = self.engine.compile(source).map_err(|e| e.to_string())?;
        // §2.10: scripts are tick-entry programs — `on_tick` must
        // exist AND take exactly one parameter (the `ctx` map). An
        // arity-mismatched definition compiled fine but faulted
        // `FunctionNotFound` at every call — five strikes later the
        // script was dead, and the load had accepted it anyway.
        // Check the signature at prepare so a wrong-arity script is
        // rejected like a syntax fault, never a stored dead script.
        match ast.iter_functions().find(|f| f.name == "on_tick") {
            None => return Err("script must define `fn on_tick(ctx)`".to_string()),
            Some(f) if f.params.len() != 1 => {
                return Err(format!(
                    "`fn on_tick(ctx)` must take exactly one parameter (found {})",
                    f.params.len()
                ));
            }
            _ => {}
        }
        self.asts.insert(id, ast);
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    fn run_tick(
        &mut self,
        script: &mut ScriptState,
        tick: u64,
        io: &mut dyn TickIo,
    ) -> ScriptRunReport {
        let ast = match self.asts.get(&script.id) {
            Some(a) => a.clone(),
            // The lazy path (AST cache lost across a restore)
            // recompiles the serialized `source` — it gets the same
            // §2.10 gate as `prepare`, never a preprocess-free pass.
            None => match preprocess(&script.source).and_then(|()| {
                self.engine
                    .compile(&script.source)
                    .map_err(|e| e.to_string())
            }) {
                Ok(a) => {
                    self.asts.insert(script.id, a.clone());
                    a
                }
                Err(e) => {
                    return ScriptRunReport {
                        ops_used: 0,
                        fault: Some(ScriptFault::Runtime(format!("recompile: {e}"))),
                    };
                }
            },
        };
        // One snapshot serves both the `ctx.vessel` map and the
        // rotation `hold_attitude()` binds against.
        let telemetry = io.telemetry(script.vessel);
        // The `telemetry_of`/`vessels` read surface: every live
        // vessel's telemetry map, keyed for the id lookup and listed
        // for the array return. Precomputed here — a registered Rhai
        // closure can't reach the borrowed `io`, and the maps are
        // this tick's frozen snapshot either way.
        let mut telemetry_by_id = BTreeMap::new();
        let mut telemetry_list = rhai::Array::new();
        for vid in io.vessels() {
            let m = Self::telemetry_to_map(&io.telemetry(vid), vid);
            telemetry_by_id.insert(vid.0, m.clone());
            telemetry_list.push(Dynamic::from_map(m));
        }
        // Install this tick's binding + a fresh effects cell — the
        // fns were registered once at construction and read both
        // through the shared cells.
        *self.bound.borrow_mut() = Bound {
            vessel: script.vessel,
            rot: telemetry.rot,
            telemetry_by_id: Rc::new(telemetry_by_id),
            telemetry_list: Rc::new(telemetry_list),
        };
        *self.fx.borrow_mut() = ScriptEffects::default();

        // Build ctx: tick + vessel telemetry + live persist — all
        // PLAIN values. `ctx` itself is the shared cell (`into_shared`
        // at the call below): the callee's `ctx` param binds to the
        // same cell, so every guest mutation writes through it and is
        // read back from our handle afterwards. `persist` must NOT be
        // the shared cell — a `Union::Shared` nested inside a map is
        // exactly what made any `ctx.<k> = …` assignment hit rhai
        // 1.27's `unreachable!("shared values discovered within data")`
        // in `calc_map_sizes` (eval/data_check.rs) once the
        // set_max_*_size limits engaged — one pasted line killed the
        // process (C26-F1). A shared cell at the ROOT is the supported
        // shape: `calc_data_sizes` reads a top-level `Union::Shared`
        // through its cell instead of panicking.
        let mut ctx = Map::new();
        ctx.insert("tick".into(), Dynamic::from_int(tick as INT));
        ctx.insert("script".into(), Dynamic::from_int(script.id.0 as INT));
        ctx.insert(
            "vessel".into(),
            Dynamic::from_map(Self::telemetry_to_map(&telemetry, script.vessel)),
        );
        ctx.insert(
            "persist".into(),
            Dynamic::from_map(persist_map_to_rhai(&script.persist)),
        );

        // §2.10's enforcement point: the engine's `max_operations`
        // (forwarded from the world's per-save budget via
        // `set_op_budget`) aborts the call; `on_progress` tallies the
        // ops for the report's poll detector.
        *self.ops.borrow_mut() = 0;
        let mut scope = Scope::new();
        let ctx_shared = Dynamic::from_map(ctx).into_shared();
        let arg = ctx_shared.clone();
        // Panic containment: the call runs inline on the game thread
        // and rhai internals CAN panic (C26-F1's calc_map_sizes — plus
        // any future one a limit or config change wakes up). Degrade
        // to a `Runtime` fault — a strike — instead of process death.
        let call = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.engine.call_fn(&mut scope, &ast, "on_tick", (arg,))
        }));
        let ops_used = *self.ops.borrow();
        let result: Result<Dynamic, _> = match call {
            Ok(r) => r,
            Err(payload) => {
                let msg = payload
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_string()))
                    .unwrap_or_else(|| "unknown panic".to_string());
                return ScriptRunReport {
                    ops_used,
                    fault: Some(ScriptFault::Runtime(format!("host-internal panic: {msg}"))),
                };
            }
        };

        // The post-call ctx, materialized through the shared cell —
        // every mutation the script made is visible here.
        let ctx_out = ctx_shared.flatten_clone().try_cast::<Map>();

        match result {
            Err(e) => {
                // Structured classification — `EvalAltResult` carries
                // the abort cause, so no message sniffing:
                // `ErrorTooManyOperations` is the `max_operations`
                // budget abort and `ErrorTerminated` the `on_progress`
                // token abort (this host's callback never returns
                // `Some`, so unreachable today — kept so a future
                // progress-token abort still classifies as the budget
                // abort it is). A guest `throw` is `ErrorRuntime`
                // whatever its message says — naming the budget in a
                // thrown string can no longer misclassify.
                let fault = if matches!(
                    *e,
                    EvalAltResult::ErrorTooManyOperations(..) | EvalAltResult::ErrorTerminated(..)
                ) {
                    ScriptFault::BudgetExceeded
                } else {
                    ScriptFault::Runtime(e.to_string())
                };
                ScriptRunReport {
                    ops_used,
                    fault: Some(fault),
                }
            }
            Ok(_) => {
                // §2.10's atomic-tick drain: EVERY fallible step
                // precedes EVERY mutation, so a reported fault lands
                // no effects at all — never a half-drained tick.
                //
                // 1. Convert the persist write-back — pure; a
                //    non-primitive value is a runtime fault before
                //    anything has moved. The persist slot itself must
                //    still be a map (see the ctx contract in the
                //    module doc): wholesale `ctx.persist = #{…}` is
                //    honored, but leaving it a scalar or deleting it
                //    breaks the contract — also a fault.
                let persist_next = match &ctx_out {
                    Some(m) => match m.get("persist").and_then(|v| v.clone().try_cast::<Map>()) {
                        Some(pm) => match rhai_map_to_persist(&pm) {
                            Ok(p) => Some(p),
                            Err(e) => {
                                return ScriptRunReport {
                                    ops_used,
                                    fault: Some(ScriptFault::Runtime(e)),
                                };
                            }
                        },
                        None => {
                            return ScriptRunReport {
                                ops_used,
                                fault: Some(ScriptFault::Runtime(
                                    "ctx.persist must remain a map of primitives".to_string(),
                                )),
                            };
                        }
                    },
                    // `ctx` itself no longer holds a map — the persist
                    // slot is unreachable, so the previous map is kept
                    // rather than faulting on a destroyed handle.
                    None => None,
                };
                // 2. Resolve the wake intent — the only remaining
                //    fallible step (§2.10: "refused at registration").
                //    `request_wake`/`cancel_wake` resolve before they
                //    mutate, so a refusal leaves the heap untouched.
                //    A refusal is a *diagnostic*, not a strike:
                //    conditions that can't resolve are a normal
                //    outcome (no node armed, hyperbolic apsis), and
                //    striking for one made an honest `wait_*` a
                //    disable risk. The refusal rides `script_log` so
                //    the player sees it — and the tick's other
                //    effects still apply.
                let fx = self.fx.borrow();
                if let Some(w) = &fx.wake {
                    let r = match w {
                        Ok(cond) => io.request_wake(script.id, cond).map(|_| ()),
                        Err(()) => io.cancel_wake(script.id),
                    };
                    if let Err(e) = r {
                        io.log_line(script.id, &format!("wake refused: {}", e.reason()));
                    }
                }
                // 3. Everything left is infallible: assign persist,
                //    enqueue commands for the next drain, surface logs.
                if let Some(p) = persist_next {
                    script.persist = p;
                }
                // `source_id` is a u32 channel key, not the full
                // script id (see `CommandKey::source_id`): every id
                // above `u32::MAX` collapses onto the shared
                // `(Script, u32::MAX)` channel — one pending FIFO and
                // one `source_seq` counter — while never aliasing a
                // smaller id's channel. `check_control_lease` clamps
                // `Lease::Script` the same way, so the collapse is
                // consistent end to end.
                let source_id = script.id.0.min(u64::from(u32::MAX)) as u32;
                for c in &fx.commands {
                    io.issue(SourceClass::Script, source_id, c.clone());
                }
                for l in &fx.logs {
                    io.log_line(script.id, l);
                }
                ScriptRunReport {
                    ops_used,
                    fault: None,
                }
            }
        }
    }

    fn unload(&mut self, id: ScriptId) {
        self.asts.remove(&id);
    }

    fn set_op_budget(&mut self, ops: u64) {
        self.engine.set_max_operations(ops);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use rhai::Array;

    /// The §2.10 compile-time gate: bare `wait(...)`/`until(...)`
    /// reject; the wake-service spellings pass.
    #[test]
    fn preprocess_rejects_blocking_waits() {
        assert!(preprocess("fn on_tick(ctx) { wait(10); }").is_err());
        assert!(preprocess("fn on_tick(ctx) { until(x); }").is_err());
        assert!(preprocess("fn on_tick(ctx) { wait_ticks(10); }").is_ok());
        assert!(preprocess("fn on_tick(ctx) { wait_apoapsis(); }").is_ok());
        // `waiting(` is a different identifier — not a `wait(` call.
        assert!(preprocess("fn on_tick(ctx) { waiting(); }").is_ok());
    }

    /// The gate is token-aware, not a substring scan: comments,
    /// string/char/raw literals, and the literal spans of backtick
    /// templates never reject — while an interpolation's `${…}` is
    /// real code and DOES, as does a bare call anywhere in code.
    #[test]
    fn preprocess_ignores_literals_and_member_calls() {
        // Comments.
        assert!(preprocess("// wait(10)\nfn on_tick(ctx) {}").is_ok());
        assert!(preprocess("/* until(x) */ fn on_tick(ctx) {}").is_ok());
        assert!(preprocess("fn on_tick(ctx) { /* wait(1) */ }").is_ok());
        // String / char / raw-string literals.
        assert!(preprocess("fn on_tick(ctx) { let s = \"wait(10)\"; }").is_ok());
        assert!(preprocess("fn on_tick(ctx) { let s = \"until(x)\"; }").is_ok());
        assert!(preprocess("fn on_tick(ctx) { let s = #\"wait(1)\"#; }").is_ok());
        assert!(preprocess("fn on_tick(ctx) { let c = '('; }").is_ok());
        // Backtick template literal text vs. its interpolation.
        assert!(preprocess("fn on_tick(ctx) { let s = `wait(1)`; }").is_ok());
        assert!(preprocess("fn on_tick(ctx) { let s = `${wait(1)}`; }").is_err());
        // Member calls — `x.until(`/`ctx.wait(` are resolved against
        // the receiver, never against a free blocking call.
        assert!(preprocess("fn on_tick(ctx) { ctx.until(10); }").is_ok());
        assert!(preprocess("fn on_tick(ctx) { x.wait(1); }").is_ok());
        assert!(preprocess("fn on_tick(ctx) { x . until(1); }").is_ok());
        // A free call still rejects — even mid-expression, and even
        // buried after other member syntax.
        assert!(preprocess("fn on_tick(ctx) { let t = until(3); }").is_err());
        assert!(preprocess("fn on_tick(ctx) { foo(); wait(1); }").is_err());
        // Non-call uses of the names are identifiers, not calls.
        assert!(preprocess("fn on_tick(ctx) { let wait = 1; }").is_ok());
    }

    /// C16-F1 regression: each `${` inside a backtick template
    /// recurses one native stack frame in `Scan::code` — an uncapped
    /// `` "`${" * n `` source overflowed the host stack (~20k reps
    /// abort the process; ~10k on wasm's 1 MiB stack) before Rhai's
    /// parser limits could engage. The depth cap rejects it as an
    /// ordinary compile error.
    #[test]
    fn preprocess_caps_template_interpolation_depth() {
        // The verifier's repro, scaled to the cap: unterminated input
        // still rejects — the depth check fires before EOF matters.
        let evil = "`${".repeat(MAX_TEMPLATE_DEPTH + 1);
        assert!(preprocess(&evil).is_err());
        // Nesting AT the cap still scans clean — the reject is the
        // depth, not the template syntax.
        let at_cap = format!(
            "{}x{}",
            "`${".repeat(MAX_TEMPLATE_DEPTH),
            "}`".repeat(MAX_TEMPLATE_DEPTH)
        );
        assert!(preprocess(&at_cap).is_ok());
        // One level past the cap, well-formed, rejects.
        let over = format!(
            "{}x{}",
            "`${".repeat(MAX_TEMPLATE_DEPTH + 1),
            "}`".repeat(MAX_TEMPLATE_DEPTH + 1)
        );
        assert!(preprocess(&over).is_err());
    }

    /// §2.10's primitive-map round-trip; nested structures refuse.
    #[test]
    fn persist_conversions_are_primitives_only() {
        assert_eq!(
            dynamic_to_persist(&Dynamic::from_bool(true)),
            Some(PersistValue::Bool(true))
        );
        assert_eq!(
            dynamic_to_persist(&Dynamic::from_int(3)),
            Some(PersistValue::Int(3))
        );
        assert_eq!(
            dynamic_to_persist(&Dynamic::from_float(1.5)),
            Some(PersistValue::Float(1.5))
        );
        assert_eq!(
            dynamic_to_persist(&Dynamic::from("s".to_string())),
            Some(PersistValue::Text("s".to_string()))
        );
        assert_eq!(dynamic_to_persist(&Dynamic::from_float(f64::NAN)), None);
        assert_eq!(dynamic_to_persist(&Dynamic::from_array(Array::new())), None);
    }

    /// The documented surface and the registered functions stay in
    /// lock step at NAME **and** ARITY — the §2.10 parity rule. Each
    /// entry's literal argument list is called through the real host:
    /// a missing function or a wrong arity is a `FunctionNotFound`
    /// runtime fault, so a drifted binding can never pass silently.
    #[test]
    fn control_api_surface_is_registered() {
        assert_eq!(CONTROL_API_SURFACE.len(), 25);
        let mut host = RhaiHost::new();
        for (id, (name, args)) in CONTROL_API_SURFACE.iter().enumerate() {
            let src = format!("fn on_tick(ctx) {{ {name}({args}); }}");
            host.prepare(ScriptId(id as u64), &src)
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            let mut s = script(&src);
            s.id = ScriptId(id as u64);
            let mut io = StubIo::new();
            let r = host.run_tick(&mut s, 1, &mut io);
            assert!(
                r.fault.is_none(),
                "{name}({args}) must resolve at its documented arity: {:?}",
                r.fault
            );
        }
        // Negative proof the arity check is real: a wrong-arity call
        // on a registered name MUST fault, not pass silently.
        let bad = "fn on_tick(ctx) { hold_attitude(1); }";
        host.prepare(ScriptId(99), bad).unwrap();
        let mut s = script(bad);
        s.id = ScriptId(99);
        let mut io = StubIo::new();
        let r = host.run_tick(&mut s, 1, &mut io);
        assert!(r.fault.is_some(), "hold_attitude(1) must not resolve");
    }

    /// A stub `TickIo` for host-level tests — no World needed.
    struct StubIo {
        telemetry: VesselTelemetry,
        commands: Vec<SimCommand>,
        logs: Vec<String>,
        wake: Option<WakeCondition>,
    }

    impl StubIo {
        fn new() -> Self {
            Self {
                telemetry: VesselTelemetry {
                    tick: 0,
                    found: true,
                    residence: csp_sim::TelemetryResidence::InBubble,
                    pos_m: DVec3::new(6.0e5, 0.0, 0.0),
                    vel_mps: DVec3::new(0.0, 10.0, 0.0),
                    rot: DQuat::IDENTITY,
                    ang_vel_mps: DVec3::ZERO,
                    throttle: 0.0,
                    altitude_m: Some(100.0),
                    speed_mps: 10.0,
                    vertical_speed_mps: 0.0,
                    horizontal_speed_mps: 10.0,
                    mass_kg: Some(1_000.0),
                    apoapsis_altitude_m: Some(50_000.0),
                    periapsis_altitude_m: Some(40_000.0),
                    time_to_apoapsis_s: Some(60.0),
                    time_to_periapsis_s: Some(300.0),
                    mu_m3_per_s2: Some(3.5e12),
                    body: None,
                    elements: None,
                    maneuver_node: None,
                    resources: std::collections::BTreeMap::new(),
                    parts: Vec::new(),
                },
                commands: Vec::new(),
                logs: Vec::new(),
                wake: None,
            }
        }
    }

    impl TickIo for StubIo {
        fn telemetry(&self, _v: VesselId) -> VesselTelemetry {
            self.telemetry.clone()
        }
        fn issue(&mut self, _c: SourceClass, _s: u32, cmd: SimCommand) {
            self.commands.push(cmd);
        }
        fn request_wake(&mut self, _s: ScriptId, cond: &WakeCondition) -> Result<u64, SimError> {
            self.wake = Some(cond.clone());
            Ok(99)
        }
        fn cancel_wake(&mut self, _s: ScriptId) -> Result<(), SimError> {
            Ok(())
        }
        fn vessels(&self) -> Vec<VesselId> {
            vec![VesselId(0)]
        }
        fn log_line(&mut self, _s: ScriptId, line: &str) {
            self.logs.push(line.to_string());
        }
    }

    fn script(source: &str) -> ScriptState {
        ScriptState {
            id: ScriptId(0),
            vessel: VesselId(0),
            source: source.to_string(),
            enabled: true,
            disabled_reason: None,
            strikes: 0,
            consecutive_polls: 0,
            poll_demoted: false,
            wake_tick: None,
            persist: PersistMap::new(),
        }
    }

    /// The full host path: prepare → run_tick → persist write-back +
    /// commands enqueued through the io.
    #[test]
    fn host_runs_on_tick_and_writes_persist() {
        let mut host = RhaiHost::new();
        let src = r#"
            fn on_tick(ctx) {
                ctx.persist["n"] = ctx.persist["n"] + 1;
                set_throttle(0.5);
            }
        "#;
        host.prepare(ScriptId(0), src).unwrap();
        let mut s = script(src);
        s.persist.insert("n".to_string(), PersistValue::Int(0));
        let mut io = StubIo::new();
        let r1 = host.run_tick(&mut s, 1, &mut io);
        assert!(r1.fault.is_none(), "{:?}", r1.fault);
        assert_eq!(s.persist.get("n"), Some(&PersistValue::Int(1)));
        let r2 = host.run_tick(&mut s, 2, &mut io);
        assert!(r2.fault.is_none());
        assert_eq!(s.persist.get("n"), Some(&PersistValue::Int(2)));
        assert!(io
            .commands
            .iter()
            .any(|c| matches!(c, SimCommand::SetThrottle { throttle, .. } if (*throttle - 0.5).abs() < 1e-9)));
    }

    /// §2.10's budget: a loop over `set_max_operations` aborts →
    /// `BudgetExceeded` fault.
    #[test]
    fn host_budget_abort() {
        let mut host = RhaiHost::new();
        host.set_op_budget(500);
        let src = "fn on_tick(ctx) { let i = 0; while true { i += 1; } }";
        host.prepare(ScriptId(0), src).unwrap();
        let mut s = script(src);
        let mut io = StubIo::new();
        let r = host.run_tick(&mut s, 1, &mut io);
        assert_eq!(r.fault, Some(ScriptFault::BudgetExceeded), "{:?}", r.fault);
    }

    /// C16-F2 regression: `set_max_operations` counts ops, not bytes —
    /// a doubling loop allocates 2ⁿ in ~n counted ops, so an uncapped
    /// guest string/array OOM-killed the host inside the op budget
    /// (~34 ops for 16 GiB). The `set_max_*_size` ceilings turn it
    /// into an ordinary runtime fault (a strike), not an abort.
    #[test]
    fn host_memory_limits_fault_not_oom() {
        let mut host = RhaiHost::new();
        // ~a few hundred counted ops doubling a 16-byte string past
        // the 1 MiB ceiling (crossed at iteration ~17).
        let src = "fn on_tick(ctx) { let s = \"0123456789abcdef\"; let i = 0; \
                   while i < 60 { s = s + s; i += 1; } }";
        host.prepare(ScriptId(0), src).unwrap();
        let mut s = script(src);
        let mut io = StubIo::new();
        let r = host.run_tick(&mut s, 1, &mut io);
        assert!(
            matches!(r.fault, Some(ScriptFault::Runtime(_))),
            "over-limit string growth must fault, not abort the host: {:?}",
            r.fault
        );
        // Same for arrays — `a += a` doubles the element count.
        let src = "fn on_tick(ctx) { let a = [1]; let i = 0; \
                   while i < 40 { a += a; i += 1; } }";
        host.prepare(ScriptId(1), src).unwrap();
        let mut s = script(src);
        s.id = ScriptId(1);
        let r = host.run_tick(&mut s, 1, &mut io);
        assert!(
            matches!(r.fault, Some(ScriptFault::Runtime(_))),
            "over-limit array growth must fault, not abort the host: {:?}",
            r.fault
        );
    }

    /// Telemetry reaches the script through `ctx.vessel`.
    #[test]
    fn host_telemetry_reaches_ctx() {
        let mut host = RhaiHost::new();
        let src = "fn on_tick(ctx) { ctx.persist[\"alt\"] = ctx.vessel.altitude; }";
        host.prepare(ScriptId(0), src).unwrap();
        let mut s = script(src);
        let mut io = StubIo::new();
        let r = host.run_tick(&mut s, 1, &mut io);
        assert!(r.fault.is_none(), "{:?}", r.fault);
        assert_eq!(s.persist.get("alt"), Some(&PersistValue::Float(100.0)));
    }

    /// §2.10's no-wall-clock rule is a build fact: under the pinned
    /// `no_std` feature set `BasicTimePackage` is never registered —
    /// `timestamp()`/`elapsed()` are `FunctionNotFound` faults, and
    /// `Instant` does not exist. (Proof for the audit: these calls
    /// evaluated under the previous `default-features = false` pin,
    /// reading `Instant::now()` into script-visible persist state.)
    #[test]
    fn no_wall_clock_reaches_the_guest() {
        let mut host = RhaiHost::new();
        for (i, call) in ["timestamp()", "elapsed(timestamp())"].iter().enumerate() {
            let src = format!("fn on_tick(ctx) {{ ctx.persist[\"t\"] = {call}; }}");
            host.prepare(ScriptId(i as u64), &src)
                .unwrap_or_else(|e| panic!("{call}: {e}"));
            let mut s = script(&src);
            s.id = ScriptId(i as u64);
            let mut io = StubIo::new();
            let r = host.run_tick(&mut s, 1, &mut io);
            assert!(
                matches!(r.fault, Some(ScriptFault::Runtime(ref m)) if m.contains("Function not found")),
                "{call} must be unknown to the guest: {:?}",
                r.fault
            );
            assert!(
                !s.persist.contains_key("t"),
                "a faulting call writes no persist"
            );
        }
    }

    /// Guest transcendentals resolve to libm — the cross-platform
    /// determinism pin `no_std` bundles (`num-traits/libm`):
    /// `sin(1.0)` in-script returns `libm::sin(1.0)` exactly.
    #[test]
    fn guest_math_is_libm_backed() {
        let mut host = RhaiHost::new();
        let src = r#"
            fn on_tick(ctx) {
                ctx.persist["s"] = sin(1.0);
                ctx.persist["c"] = cos(1.0);
                ctx.persist["r"] = sqrt(2.0);
                ctx.persist["e"] = exp(1.0);
            }
        "#;
        host.prepare(ScriptId(0), src).unwrap();
        let mut s = script(src);
        let mut io = StubIo::new();
        let r = host.run_tick(&mut s, 1, &mut io);
        assert!(r.fault.is_none(), "{:?}", r.fault);
        for (k, want) in [
            ("s", libm::sin(1.0)),
            ("c", libm::cos(1.0)),
            ("r", libm::sqrt(2.0)),
            ("e", libm::exp(1.0)),
        ] {
            assert_eq!(
                s.persist.get(k),
                Some(&PersistValue::Float(want)),
                "{k} must equal the libm value bit-for-bit"
            );
        }
    }

    /// `hold_attitude()` is the 0-arg ControlApi verb — it issues
    /// `SetAttitude` at the bound vessel's telemetry rotation, and
    /// `ctx.vessel.rot` carries the same quat to the guest.
    #[test]
    fn hold_attitude_holds_the_current_rotation() {
        let mut host = RhaiHost::new();
        let rot = DQuat::new(0.0, 0.0, libm::sin(0.5), libm::cos(0.5));
        let src = r#"
            fn on_tick(ctx) {
                hold_attitude();
                ctx.persist["w"] = ctx.vessel.rot.w;
            }
        "#;
        host.prepare(ScriptId(0), src).unwrap();
        let mut s = script(src);
        let mut io = StubIo::new();
        io.telemetry.rot = rot;
        let r = host.run_tick(&mut s, 1, &mut io);
        assert!(r.fault.is_none(), "{:?}", r.fault);
        assert!(
            io.commands
                .iter()
                .any(|c| matches!(c, SimCommand::SetAttitude { rot: q, .. } if *q == rot)),
            "hold_attitude() must SetAttitude the telemetry rotation: {:?}",
            io.commands
        );
        assert_eq!(
            s.persist.get("w"),
            Some(&PersistValue::Float(rot.w)),
            "ctx.vessel.rot.w must be the telemetry quat"
        );
    }

    /// §2.10's "refused at registration" is a diagnostic, not a
    /// strike (report-18 F5): a `wait_*` that can't resolve is a
    /// normal outcome — no node armed, a hyperbolic apsis — so the
    /// tick reports no fault, the refusal rides `script_log`, and
    /// the tick's other effects (commands, persist write) still
    /// apply. The old strike-on-refusal made an honest `wait_*` a
    /// disable risk.
    #[test]
    fn refused_wake_is_a_log_diagnostic_not_a_strike() {
        struct RefusingIo(StubIo);
        impl TickIo for RefusingIo {
            fn telemetry(&self, v: VesselId) -> VesselTelemetry {
                self.0.telemetry(v)
            }
            fn vessels(&self) -> Vec<VesselId> {
                self.0.vessels()
            }
            fn issue(&mut self, c: SourceClass, s: u32, cmd: SimCommand) {
                self.0.issue(c, s, cmd);
            }
            fn request_wake(
                &mut self,
                _s: ScriptId,
                _cond: &WakeCondition,
            ) -> Result<u64, SimError> {
                Err(SimError::Recoverable("wake: no such apsis ahead"))
            }
            fn cancel_wake(&mut self, s: ScriptId) -> Result<(), SimError> {
                self.0.cancel_wake(s)
            }
            fn log_line(&mut self, s: ScriptId, line: &str) {
                self.0.log_line(s, line);
            }
        }
        let mut host = RhaiHost::new();
        let src = r#"
            fn on_tick(ctx) {
                ctx.persist["n"] = 1;
                set_throttle(0.5);
                wait_ticks(5);
            }
        "#;
        host.prepare(ScriptId(0), src).unwrap();
        let mut s = script(src);
        let mut io = RefusingIo(StubIo::new());
        let r = host.run_tick(&mut s, 1, &mut io);
        assert!(r.fault.is_none(), "{:?}", r.fault);
        assert!(
            io.0.logs.iter().any(|l| l.starts_with("wake refused:")),
            "the refusal must ride script_log: {:?}",
            io.0.logs
        );
        assert!(
            io.0.commands
                .iter()
                .any(|c| matches!(c, SimCommand::SetThrottle { .. })),
            "the tick's other effects still apply"
        );
        assert_eq!(s.persist.get("n"), Some(&PersistValue::Int(1)));
    }

    /// C26-F1 regression: property assignment into `ctx` used to panic
    /// the process — `ctx.persist` was planted as a shared cell INSIDE
    /// the ctx map, and rhai 1.27's `calc_map_sizes` hits
    /// `unreachable!("shared values discovered within data")` on any
    /// `ctx.<k> = …` under the set_max_*_size limits. `ctx` itself is
    /// the shared cell now, so no `Union::Shared` is ever nested in a
    /// map. Every verified panic form must complete normally; the ok
    /// forms keep working; leaving `ctx.persist` a non-map is a
    /// `Runtime` strike per the ctx contract.
    #[test]
    fn ctx_property_assignment_never_panics() {
        let mut host = RhaiHost::new();
        // Verified panic forms — each must return a report, never
        // unwind. `want` is what script.persist must hold after the
        // tick (`Some` exact match, `None` = previous map untouched).
        for (i, (body, want)) in [
            // Wholesale reassignment to a map — honored, replaces.
            (
                "ctx.persist = #{ n: 1 };",
                Some(vec![("n", PersistValue::Int(1))]),
            ),
            // Indexed write then clear — the clear lands.
            ("ctx.persist[\"n\"] = 1; ctx.persist = #{};", Some(vec![])),
            // Adding an unrelated top-level property — inert.
            ("ctx.extra = 1;", None),
            // `let c = ctx` is a `let` binding — rhai flattens the
            // initializer, so `c` is a private copy, not an alias;
            // the write lands on the copy and is discarded with the
            // callee scope. No fault, persist untouched — the same
            // semantics as any guest map.
            ("let c = ctx; c.persist = #{ n: 1 };", None),
        ]
        .iter()
        .enumerate()
        {
            let src = format!("fn on_tick(ctx) {{ {body} }}");
            host.prepare(ScriptId(i as u64), &src)
                .unwrap_or_else(|e| panic!("{body}: {e}"));
            let mut s = script(&src);
            s.id = ScriptId(i as u64);
            s.persist.insert("keep".to_string(), PersistValue::Int(7));
            let mut io = StubIo::new();
            let r = host.run_tick(&mut s, 1, &mut io);
            assert!(r.fault.is_none(), "{body}: {r:?}");
            match want {
                Some(entries) => {
                    let mut expect = PersistMap::new();
                    for (k, v) in entries {
                        expect.insert(k.to_string(), v.clone());
                    }
                    assert_eq!(s.persist, expect, "{body}: persist mismatch");
                }
                None => assert_eq!(
                    s.persist.get("keep"),
                    Some(&PersistValue::Int(7)),
                    "{body}: persist must be untouched"
                ),
            }
        }
        // The ok forms — indexed write, remove, aliased read of the
        // persist value inside an array — never faulted.
        for (i, body) in [
            "ctx.persist[\"n\"] = 1;",
            "ctx.persist[\"n\"] = 1; ctx.persist.remove(\"n\");",
            "let a = [ctx.persist]; let x = ctx.persist[\"n\"];",
        ]
        .iter()
        .enumerate()
        {
            let src = format!("fn on_tick(ctx) {{ {body} }}");
            host.prepare(ScriptId(100 + i as u64), &src)
                .unwrap_or_else(|e| panic!("{body}: {e}"));
            let mut s = script(&src);
            s.id = ScriptId(100 + i as u64);
            let mut io = StubIo::new();
            let r = host.run_tick(&mut s, 1, &mut io);
            assert!(r.fault.is_none(), "{body}: {r:?}");
        }
        // Contract violation — `ctx.persist` left as a non-map is a
        // `Runtime` strike, not a silent pass.
        let bad = "fn on_tick(ctx) { ctx.persist = 5; }";
        host.prepare(ScriptId(200), bad).unwrap();
        let mut s = script(bad);
        s.id = ScriptId(200);
        let mut io = StubIo::new();
        let r = host.run_tick(&mut s, 1, &mut io);
        assert!(
            matches!(r.fault, Some(ScriptFault::Runtime(_))),
            "ctx.persist = 5 must strike, not silently pass: {r:?}"
        );
    }

    /// C26-F9 regression: the host fns register once at construction
    /// and read the tick's binding through the shared cell — a second
    /// script's `run_tick` MUST reinstall the bound vessel or its
    /// commands would stamp the previous script's.
    #[test]
    fn per_tick_binding_is_reinstalled() {
        let mut host = RhaiHost::new();
        for id in [0_u64, 1] {
            let src = "fn on_tick(ctx) { set_throttle(0.5); }";
            host.prepare(ScriptId(id), src).unwrap();
            let mut s = script(src);
            s.id = ScriptId(id);
            s.vessel = VesselId(id * 7); // distinct vessels: 0 and 7
            let mut io = StubIo::new();
            let r = host.run_tick(&mut s, 1, &mut io);
            assert!(r.fault.is_none(), "{:?}", r.fault);
            assert!(
                io.commands
                    .iter()
                    .any(|c| matches!(c, SimCommand::SetThrottle { vessel, .. } if *vessel == VesselId(id * 7))),
                "script {id}'s command must stamp vessel {}: {:?}",
                id * 7,
                io.commands
            );
        }
    }

    /// Panic containment around `engine.call_fn`: a panic anywhere
    /// inside the script call — rhai internals or a host function —
    /// degrades to a `Runtime` strike instead of unwinding to the game
    /// thread (scripts run inline; pre-fix a rhai-internal panic was
    /// process death — C26-F1). Simulated with a panicking host
    /// function; rhai's own panics unwind on the same path.
    #[test]
    fn host_fn_panic_degrades_to_runtime_fault() {
        let mut host = RhaiHost::new();
        host.engine
            .register_fn("boom", || -> () { panic!("simulated internal panic") });
        let src = "fn on_tick(ctx) { boom(); }";
        host.prepare(ScriptId(0), src).unwrap();
        let mut s = script(src);
        let mut io = StubIo::new();
        let r = host.run_tick(&mut s, 1, &mut io);
        assert!(
            matches!(r.fault, Some(ScriptFault::Runtime(ref m)) if m.contains("panic")),
            "a panic inside the call must become a Runtime strike: {r:?}"
        );
    }
}
