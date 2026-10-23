//! M9c §2.12.1 — the MCP protocol engine, transport-free.
//!
//! JSON-RPC 2.0 + the MCP method surface, hand-rolled per the plan's
//! pinned revision — **protocol revision `2026-07-28`** (asserted by
//! test and recorded in `PROTOCOL.md`; `2025-11-25` is also accepted,
//! the 2024-11-05 HTTP+SSE transport is never implemented). Both
//! transports feed this one engine:
//!
//! - **stdio** (`csp-mcp` bin): newline-delimited JSON-RPC, one
//!   message per line, responses on stdout.
//! - **Streamable HTTP** (`csp-server`): a single POST endpoint that
//!   returns either `application/json` or a per-request
//!   `text/event-stream` upgrade (the transport's choice per §2.12).
//!
//! # Tools — `csp.mcp.tools.v1`
//!
//! The tool schema is versioned independently
//! ([`TOOL_SCHEMA_VERSION`]) with the plan's promise: **additive-only
//! within a major version; a tool never silently changes semantics**.
//! Every tool declares its class — `"class": "session"` on
//! session-class tools (save/load/snapshot + the app's editor/camera
//! family); those are *not replay-relevant*. Sim-class tools map
//! one-to-one onto `SimCommand`s / the frozen `ControlApi` verbs —
//! issued under `source_class = External`, they pay the same one-tick
//! latency and honour the same §2.2.1 control lease as every client.

use std::sync::Arc;

use csp_core::DVec3;
use csp_sim::{Lease, SimCommand, VesselId};
use serde_json::{Value, json};

use crate::backend::{BackendError, ControlBackend};

/// The pinned protocol revision — `PROTOCOL.md` is the narrative;
/// this constant is what the server actually negotiates.
pub const PROTOCOL_REVISION: &str = "2026-07-28";

/// Revisions the server accepts on `initialize` and tolerates on the
/// `MCP-Protocol-Version` header. `2025-11-25` is the previous
/// Streamable-HTTP revision clients may still offer.
pub const SUPPORTED_REVISIONS: &[&str] = &[PROTOCOL_REVISION, "2025-11-25"];

/// The tool-schema version token — additive-only within v1.
pub const TOOL_SCHEMA_VERSION: &str = "csp.mcp.tools.v1";

/// JSON-RPC codes in use (-32700 parse errors are built inline by the
/// transports, which own the framing).
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const TOOL_ERROR: i64 = -32000;

fn rpc_error(id: &Value, code: i64, message: impl Into<String>) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message.into()}})
}

fn tool_text(v: Value) -> Value {
    json!({
        "content": [{"type": "text", "text": serde_json::to_string_pretty(&v).unwrap_or_else(|_| v.to_string())}],
        "isError": false,
    })
}

fn tool_error(msg: impl Into<String>) -> Value {
    json!({
        "content": [{"type": "text", "text": msg.into()}],
        "isError": true,
    })
}

/// The MCP engine over a [`ControlBackend`] — one per server. Client
/// identity arrives resolved: `handle(client, msg)` takes the
/// §2.12.3 client index (the `source_id` its commands stamp under).
///
/// Generic over the backend: the stdio bin drives `Mcp<LocalGame>`
/// (the world is `!Send`, the bin is single-threaded); the axum
/// server holds `Mcp<dyn ControlBackend + Send + Sync>` — the
/// default type parameter — against `GameHost`.
pub struct Mcp<B: ControlBackend + ?Sized = dyn ControlBackend + Send + Sync> {
    backend: Arc<B>,
}

impl<B: ControlBackend + ?Sized> Mcp<B> {
    /// Wrap the host backend.
    pub fn new(backend: Arc<B>) -> Self {
        Self { backend }
    }

    /// The backend, for handlers that need it outside MCP (the WS
    /// telemetry path).
    pub fn backend(&self) -> &Arc<B> {
        &self.backend
    }

    /// Handle one JSON-RPC message for `client`. `None` = the message
    /// was a notification (or response) and must not be answered —
    /// the JSON-RPC rule both transports rely on.
    pub fn handle(&self, client: u32, msg: &Value) -> Option<Value> {
        let id = msg.get("id");
        let Some(method) = msg.get("method").and_then(Value::as_str) else {
            // Responses from the client carry no method — drop them.
            if msg.get("result").is_some() || msg.get("error").is_some() {
                return None;
            }
            return id.map(|i| rpc_error(i, INVALID_REQUEST, "not a JSON-RPC request"));
        };
        if method.starts_with("notifications/") {
            return None; // a notification is never answered
        }
        let Some(id) = id else {
            // A request-shaped message without an id is a notification
            // for every method we serve.
            return None;
        };
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let result = match method {
            "initialize" => Ok(self.initialize(&params)),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": tools_v1()})),
            "tools/call" => self.tool_call(id, client, &params),
            "resources/list" => Ok(json!({"resources": resources_v1()})),
            "resources/read" => self.resource_read(id, &params),
            _ => {
                return Some(rpc_error(
                    id,
                    METHOD_NOT_FOUND,
                    format!("no such method {method}"),
                ));
            }
        };
        Some(match result {
            Ok(v) => json!({"jsonrpc": "2.0", "id": id, "result": v}),
            Err(e) => e,
        })
    }

    /// `initialize`: negotiate the protocol revision (§2.12's
    /// revision table) and declare capabilities. If the client
    /// offered a revision we support, echo it; otherwise respond with
    /// ours and let the client decide (the spec's fallback).
    fn initialize(&self, params: &Value) -> Value {
        let offered = params
            .get("protocolVersion")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let negotiated = if SUPPORTED_REVISIONS.contains(&offered) {
            offered.to_string()
        } else {
            PROTOCOL_REVISION.to_string()
        };
        json!({
            "protocolVersion": negotiated,
            "capabilities": {
                "tools": {"listChanged": false},
                "resources": {"subscribe": false, "listChanged": false},
            },
            "serverInfo": {
                "name": "csp-server",
                "version": env!("CARGO_PKG_VERSION"),
            },
            "instructions": format!(
                "Tool schema {TOOL_SCHEMA_VERSION}. Sim-class tools map onto SimCommands \
                 (source_class=External, one-tick latency, §2.2.1 control lease) — take \
                 vessel.take_control before vessel.control. Session-class tools carry \
                 \"class\": \"session\" and are not replay-relevant."
            ),
        })
    }

    /// `resources/read` — the one snapshot resource.
    fn resource_read(&self, id: &Value, params: &Value) -> Result<Value, Value> {
        let uri = params.get("uri").and_then(Value::as_str).unwrap_or("");
        if uri != "csp://telemetry/snapshot" {
            return Err(rpc_error(
                id,
                INVALID_PARAMS,
                format!("unknown resource {uri}"),
            ));
        }
        let Some(snap) = self.backend.telemetry() else {
            return Err(rpc_error(id, TOOL_ERROR, "no snapshot published yet"));
        };
        let text =
            serde_json::to_string(&*snap).map_err(|e| rpc_error(id, TOOL_ERROR, e.to_string()))?;
        Ok(json!({
            "contents": [{
                "uri": uri,
                "mimeType": "application/json",
                "text": text,
            }],
        }))
    }

    /// `tools/call` — dispatch into the `csp.mcp.tools.v1` surface.
    /// The `Err` arm is a protocol-level failure (unknown tool, bad
    /// params); a tool that runs and fails reports `isError` content.
    fn tool_call(&self, id: &Value, client: u32, params: &Value) -> Result<Value, Value> {
        let name = params.get("name").and_then(Value::as_str).unwrap_or("");
        let args = params.get("arguments").cloned().unwrap_or(json!({}));
        let out = self.run_tool(client, name, &args);
        match out {
            Ok(v) => Ok(tool_text(v)),
            Err(ToolFail::Invalid(m)) => Err(rpc_error(id, INVALID_PARAMS, m)),
            Err(ToolFail::Unknown) => Err(rpc_error(
                id,
                INVALID_PARAMS,
                format!("unknown tool {name}"),
            )),
            Err(ToolFail::Tool(m)) => Ok(tool_error(m)),
        }
    }

    /// The tool dispatch — one arm per `csp.mcp.tools.v1` member.
    fn run_tool(&self, client: u32, name: &str, args: &Value) -> Result<Value, ToolFail> {
        match name {
            "telemetry.get" => self.t_telemetry_get(args),
            "vessel.list" => self.t_vessel_list(),
            "vessel.control" => self.t_vessel_control(client, args),
            "vessel.take_control" => self.t_take_control(client, args, false),
            "vessel.release_control" => self.t_take_control(client, args, true),
            "mission.nodes" => self.t_mission_nodes(client, args),
            "timewarp.set" => self.t_timewarp_set(client, args),
            "timewarp.advance_to" => self.t_advance_to(client, args),
            "sim.command" => self.t_sim_command(client, args),
            "session.list_saves" => self.t_list_saves(),
            "session.save" => self.t_save(args),
            "session.load" => self.t_load(args),
            "session.snapshot" => self.t_snapshot(),
            _ => Err(ToolFail::Unknown),
        }
    }

    // -- sim-class tools ---------------------------------------------------

    fn snapshot(&self) -> Result<Arc<crate::telemetry::TelemetrySnapshot>, ToolFail> {
        self.backend
            .telemetry()
            .ok_or_else(|| ToolFail::Tool("no snapshot published yet".into()))
    }

    /// `telemetry.get {vessel?}` — one vessel's telemetry, or the
    /// whole vessel table when `vessel` is absent.
    fn t_telemetry_get(&self, args: &Value) -> Result<Value, ToolFail> {
        let snap = self.snapshot()?;
        match get_u64(args, "vessel")? {
            Some(id) => {
                let v = snap
                    .vessels
                    .get(&id)
                    .ok_or_else(|| ToolFail::Tool(format!("unknown vessel {id}")))?;
                serde_json::to_value(v).map_err(|e| ToolFail::Tool(e.to_string()))
            }
            None => serde_json::to_value(&snap.vessels).map_err(|e| ToolFail::Tool(e.to_string())),
        }
    }

    /// `vessel.list` — the compact roster: id, residence, lease.
    fn t_vessel_list(&self) -> Result<Value, ToolFail> {
        let snap = self.snapshot()?;
        let list: Vec<Value> = snap
            .vessels
            .iter()
            .map(|(id, v)| {
                json!({
                    "vessel": id,
                    "residence": v.telemetry.residence,
                    "control_lease": v.control_lease,
                    "altitude_m": v.telemetry.altitude_m,
                    "speed_mps": v.telemetry.speed_mps,
                })
            })
            .collect();
        Ok(json!({"tick": snap.tick, "vessels": list}))
    }

    /// `vessel.control {vessel, throttle?, steer_dir?, attitude?, hold_attitude?}`
    /// — the frozen `ControlApi` verbs as commands. Fields apply in
    /// argument order: steer/attitude then throttle (the documented
    /// order — a combined call lands coherently).
    fn t_vessel_control(&self, client: u32, args: &Value) -> Result<Value, ToolFail> {
        let vessel = req_u64(args, "vessel").map(VesselId)?;
        let mut enqueued = 0_u32;
        let mut rejected: Vec<String> = Vec::new();
        let mut submit = |cmd: SimCommand| match self.backend.submit(client, cmd) {
            Ok(r) => {
                enqueued += 1;
                rejected.extend(r);
            }
            Err(e) => rejected.push(e.to_string()),
        };
        if let Some(dir) = get_vec3(args, "steer_dir")? {
            let rot = if dir.norm() > 0.0 {
                csp_sim::maneuver::align_rotation(DVec3::Z, dir / dir.norm())
            } else {
                csp_core::DQuat::IDENTITY
            };
            submit(SimCommand::SetAttitude {
                vessel,
                rot,
                ang_vel: DVec3::ZERO,
            });
        }
        if let Some(att) = args.get("attitude") {
            let rot = get_quat(att, "rot")?.unwrap_or(csp_core::DQuat::IDENTITY);
            let ang_vel = get_vec3(att, "ang_vel")?.unwrap_or(DVec3::ZERO);
            submit(SimCommand::SetAttitude {
                vessel,
                rot,
                ang_vel,
            });
        }
        if args.get("hold_attitude").and_then(Value::as_bool) == Some(true) {
            // SAS-style hold of the *current* orientation — read it off
            // the published telemetry (the script host's equivalent).
            let rot = self
                .snapshot()?
                .vessels
                .get(&vessel.0)
                .map(|v| v.telemetry.rot)
                .unwrap_or(csp_core::DQuat::IDENTITY);
            submit(SimCommand::SetAttitude {
                vessel,
                rot,
                ang_vel: DVec3::ZERO,
            });
        }
        if let Some(throttle) = get_f64(args, "throttle")? {
            submit(SimCommand::SetThrottle { vessel, throttle });
        }
        if enqueued == 0 {
            return Err(ToolFail::Invalid(
                "vessel.control needs at least one of throttle/steer_dir/attitude/hold_attitude"
                    .into(),
            ));
        }
        Ok(json!({"enqueued": enqueued, "rejected": rejected}))
    }

    /// `vessel.take_control {vessel}` / `vessel.release_control
    /// {vessel}` — the §2.2.1 lease handshake. Take is for the calling
    /// client only; release restores `Player` (the apply enforces
    /// holder-or-player).
    fn t_take_control(&self, client: u32, args: &Value, release: bool) -> Result<Value, ToolFail> {
        let vessel = req_u64(args, "vessel").map(VesselId)?;
        let lease = if release {
            Lease::Player
        } else {
            Lease::Agent(client)
        };
        let rejected = self.submit(client, SimCommand::TakeControl { vessel, lease })?;
        Ok(json!({"lease": lease, "rejected": rejected}))
    }

    /// `mission.nodes {vessel, set?, delete?}` — the one-node schedule.
    fn t_mission_nodes(&self, client: u32, args: &Value) -> Result<Value, ToolFail> {
        let vessel = req_u64(args, "vessel").map(VesselId)?;
        let mut rejected = Vec::new();
        if let Some(set) = args.get("set") {
            let tick = req_u64(set, "tick")?;
            let pro = get_f64(set, "dv_prograde_mps")?.unwrap_or(0.0);
            let norm = get_f64(set, "dv_normal_mps")?.unwrap_or(0.0);
            let rad = get_f64(set, "dv_radial_mps")?.unwrap_or(0.0);
            rejected.extend(self.submit(
                client,
                SimCommand::SetManeuver {
                    vessel,
                    tick,
                    dv_prograde_mps: pro,
                    dv_normal_mps: norm,
                    dv_radial_mps: rad,
                },
            )?);
        }
        if args.get("delete").and_then(Value::as_bool) == Some(true) {
            rejected.extend(self.submit(client, SimCommand::DeleteManeuver { vessel })?);
        }
        let node = self
            .snapshot()?
            .vessels
            .get(&vessel.0)
            .and_then(|v| v.telemetry.maneuver_node)
            .map(|(t, dv)| json!({"tick": t, "dv": [dv.x, dv.y, dv.z]}));
        Ok(json!({"node": node, "rejected": rejected}))
    }

    /// `timewarp.set {level}` — the stored ladder rung.
    fn t_timewarp_set(&self, client: u32, args: &Value) -> Result<Value, ToolFail> {
        let level = req_u64(args, "level")?;
        let rejected = self.submit(client, SimCommand::SetWarp(level as u32))?;
        Ok(json!({"warp_level": self.snapshot()?.warp_level, "rejected": rejected}))
    }

    /// `timewarp.advance_to {tick}` — a logged advance (the recorder
    /// contract: the run's real targets flow through `AdvanceTo`).
    fn t_advance_to(&self, client: u32, args: &Value) -> Result<Value, ToolFail> {
        let tick = req_u64(args, "tick")?;
        let rejected = self.submit(client, SimCommand::AdvanceTo(tick))?;
        Ok(json!({"tick": self.snapshot()?.tick, "rejected": rejected}))
    }

    /// `sim.command {command}` / `{commands: [...]}` — the raw
    /// `SimCommand` escape hatch: every variant the surface doesn't
    /// name yet, same stamping and the same lease gate. `commands`
    /// submits a batch in one drain window (the form under which the
    /// §2.2.1 per-client budget is reachable).
    fn t_sim_command(&self, client: u32, args: &Value) -> Result<Value, ToolFail> {
        let mut commands = Vec::new();
        if let Some(raw) = args.get("command") {
            commands.push(
                serde_json::from_value::<SimCommand>(raw.clone())
                    .map_err(|e| ToolFail::Invalid(format!("command does not decode: {e}")))?,
            );
        }
        if let Some(raw) = args.get("commands") {
            commands.extend(
                serde_json::from_value::<Vec<SimCommand>>(raw.clone())
                    .map_err(|e| ToolFail::Invalid(format!("commands does not decode: {e}")))?,
            );
        }
        if commands.is_empty() {
            return Err(ToolFail::Invalid(
                "sim.command needs {command} or {commands}".into(),
            ));
        }
        let rejected = self
            .backend
            .submit_batch(client, commands)
            .map_err(|e| ToolFail::Tool(e.to_string()))?;
        Ok(json!({"rejected": rejected}))
    }

    fn submit(&self, client: u32, cmd: SimCommand) -> Result<Vec<String>, ToolFail> {
        self.backend.submit(client, cmd).map_err(|e| match e {
            BackendError::Ingress(i) => ToolFail::Tool(i.to_string()),
            BackendError::Rejected(r) => ToolFail::Tool(r.join("; ")),
            BackendError::Session(m) => ToolFail::Tool(m),
        })
    }

    // -- session-class tools ----------------------------------------------

    fn t_list_saves(&self) -> Result<Value, ToolFail> {
        let saves = self
            .backend
            .list_saves()
            .map_err(|e| ToolFail::Tool(e.to_string()))?;
        serde_json::to_value(json!({"saves": saves})).map_err(|e| ToolFail::Tool(e.to_string()))
    }

    fn t_save(&self, args: &Value) -> Result<Value, ToolFail> {
        let name = req_str(args, "name")?;
        let info = self
            .backend
            .store_save(name)
            .map_err(|e| ToolFail::Tool(e.to_string()))?;
        serde_json::to_value(info).map_err(|e| ToolFail::Tool(e.to_string()))
    }

    fn t_load(&self, args: &Value) -> Result<Value, ToolFail> {
        let name = req_str(args, "name")?;
        let summary = self
            .backend
            .load_save(name)
            .map_err(|e| ToolFail::Tool(e.to_string()))?;
        serde_json::to_value(summary).map_err(|e| ToolFail::Tool(e.to_string()))
    }

    fn t_snapshot(&self) -> Result<Value, ToolFail> {
        let summary = self
            .backend
            .summary()
            .map_err(|e| ToolFail::Tool(e.to_string()))?;
        serde_json::to_value(summary).map_err(|e| ToolFail::Tool(e.to_string()))
    }
}

/// Tool-call failures: `Unknown`/`Invalid` map to JSON-RPC errors,
/// `Tool` to an `isError` content result (the spec's split).
enum ToolFail {
    Unknown,
    Invalid(String),
    Tool(String),
}

fn get_u64(args: &Value, key: &str) -> Result<Option<u64>, ToolFail> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_u64()
            .map(Some)
            .ok_or_else(|| ToolFail::Invalid(format!("{key} must be a non-negative integer"))),
    }
}

fn req_u64(args: &Value, key: &str) -> Result<u64, ToolFail> {
    get_u64(args, key)?.ok_or_else(|| ToolFail::Invalid(format!("missing {key}")))
}

fn req_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, ToolFail> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| ToolFail::Invalid(format!("missing {key}")))
}

fn get_f64(args: &Value, key: &str) -> Result<Option<f64>, ToolFail> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_f64()
            .map(Some)
            .ok_or_else(|| ToolFail::Invalid(format!("{key} must be a number"))),
    }
}

fn get_vec3(args: &Value, key: &str) -> Result<Option<DVec3>, ToolFail> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(a)) if a.len() == 3 => {
            let f = |i: usize| {
                a[i].as_f64()
                    .ok_or_else(|| ToolFail::Invalid(format!("{key}[{i}] must be a number")))
            };
            Ok(Some(DVec3::new(f(0)?, f(1)?, f(2)?)))
        }
        _ => Err(ToolFail::Invalid(format!("{key} must be [x, y, z]"))),
    }
}

fn get_quat(args: &Value, key: &str) -> Result<Option<csp_core::DQuat>, ToolFail> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(a)) if a.len() == 4 => {
            let f = |i: usize| {
                a[i].as_f64()
                    .ok_or_else(|| ToolFail::Invalid(format!("{key}[{i}] must be a number")))
            };
            // DQuat's serde order is (w, x, y, z).
            Ok(Some(csp_core::DQuat::new(f(1)?, f(2)?, f(3)?, f(0)?)))
        }
        _ => Err(ToolFail::Invalid(format!("{key} must be [w, x, y, z]"))),
    }
}

/// The `csp.mcp.tools.v1` descriptor list — `"class": "session"`
/// marks the session-class tools per §2.12 ("session-class tools are
/// declared as such ... and are not replay-relevant"). Everything
/// else is sim-class: it maps to `SimCommand`s or telemetry reads.
pub fn tools_v1() -> Value {
    let sim = |name: &str, desc: &str, schema: Value| json!({"name": name, "description": desc, "class": "sim", "inputSchema": schema});
    let ses = |name: &str, desc: &str, schema: Value| json!({"name": name, "description": desc, "class": "session", "inputSchema": schema});
    let obj = |props: Value, req: &[&str]| json!({"type": "object", "properties": props, "required": req, "additionalProperties": false});
    let u = || json!({"type": "integer", "minimum": 0});
    let f = || json!({"type": "number"});
    let b = || json!({"type": "boolean"});
    let v3 = || json!({"type": "array", "items": {"type": "number"}, "minItems": 3, "maxItems": 3});
    let v4 = || json!({"type": "array", "items": {"type": "number"}, "minItems": 4, "maxItems": 4});
    json!([
        sim(
            "telemetry.get",
            "Vessel telemetry — one vessel by id, or the whole table when `vessel` is omitted.",
            obj(json!({"vessel": u()}), &[])
        ),
        sim(
            "vessel.list",
            "The vessel roster: id, residence, control lease, altitude, speed.",
            obj(json!({}), &[])
        ),
        sim(
            "vessel.control",
            "Control verbs for one vessel: throttle, steer_dir (world dir), explicit \
             attitude, or hold_attitude. Requires the caller's lease (take_control first).",
            obj(
                json!({
                    "vessel": u(),
                    "throttle": f(),
                    "steer_dir": v3(),
                    "attitude": {"type": "object", "properties": {"rot": v4(), "ang_vel": v3()}},
                    "hold_attitude": b(),
                }),
                &["vessel"]
            )
        ),
        sim(
            "vessel.take_control",
            "Acquire the vessel's §2.2.1 control lease for this client (Lease::Agent). \
             Vacant vessels only — a held lease cannot be stolen.",
            obj(json!({"vessel": u()}), &["vessel"])
        ),
        sim(
            "vessel.release_control",
            "Release the caller's lease back to Player (holder-only, per the apply).",
            obj(json!({"vessel": u()}), &["vessel"])
        ),
        sim(
            "mission.nodes",
            "Read the vessel's maneuver node; `set` schedules one, `delete` removes it.",
            obj(
                json!({
                    "vessel": u(),
                    "set": {"type": "object", "properties": {
                        "tick": u(), "dv_prograde_mps": f(),
                        "dv_normal_mps": f(), "dv_radial_mps": f()},
                        "required": ["tick"]},
                    "delete": b(),
                }),
                &["vessel"]
            )
        ),
        sim(
            "timewarp.set",
            "Store a warp ladder rung (the multiplier is presentation's business).",
            obj(json!({"level": u()}), &["level"])
        ),
        sim(
            "timewarp.advance_to",
            "Log an AdvanceTo target — the clock runs to it under §2.6.5 budgets.",
            obj(json!({"tick": u()}), &["tick"])
        ),
        sim(
            "sim.command",
            "Raw SimCommand passthrough {command} or {commands: [...]} — the escape \
             hatch for variants not yet named by the surface.",
            obj(
                json!({"command": {"type": "object"}, "commands": {"type": "array"}}),
                &[]
            )
        ),
        ses(
            "session.list_saves",
            "List saves on the host's save directory (session-class; not replay-relevant).",
            obj(json!({}), &[])
        ),
        ses(
            "session.save",
            "Write the world's §7.2 snapshot under {name}.cspsave.",
            obj(json!({"name": {"type": "string"}}), &["name"])
        ),
        ses(
            "session.load",
            "Restore the named save, replacing the running world (session-class).",
            obj(json!({"name": {"type": "string"}}), &["name"])
        ),
        ses(
            "session.snapshot",
            "The world's (tick, state-hash) summary (session-class).",
            obj(json!({}), &[])
        ),
    ])
}

/// The resource list — one snapshot resource for now.
fn resources_v1() -> Value {
    json!([{
        "uri": "csp://telemetry/snapshot",
        "name": "telemetry-snapshot",
        "description": "The latest published TelemetrySnapshot (JSON).",
        "mimeType": "application/json",
    }])
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // test assertions
#[allow(clippy::arc_with_non_send_sync)] // LocalGame is single-thread by design
mod tests {
    use super::*;
    use crate::backend::LocalGame;
    use csp_sim::World;

    fn mcp() -> (Mcp<LocalGame>, Arc<LocalGame>) {
        let game = Arc::new(LocalGame::new(
            World::new(),
            std::env::temp_dir().join("csp-mcp-test-saves"),
        ));
        (Mcp::new(game.clone()), game)
    }

    #[test]
    fn protocol_revision_is_the_pinned_one() {
        // The deliberate-commit pin — bumping the constant means
        // updating PROTOCOL.md in the same change.
        assert_eq!(PROTOCOL_REVISION, "2026-07-28");
        assert!(SUPPORTED_REVISIONS.contains(&"2025-11-25"));
    }

    #[test]
    fn initialize_negotiates_the_pinned_revision() {
        let (m, _) = mcp();
        let out = m
            .handle(
                0,
                &json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                        "params": {"protocolVersion": "2026-07-28",
                                   "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}}}),
            )
            .expect("response");
        assert_eq!(out["result"]["protocolVersion"], "2026-07-28");
        // An unknown offered revision falls back to ours.
        let out = m
            .handle(
                0,
                &json!({"jsonrpc": "2.0", "id": 2, "method": "initialize",
                        "params": {"protocolVersion": "1999-01-01"}}),
            )
            .expect("response");
        assert_eq!(out["result"]["protocolVersion"], "2026-07-28");
    }

    #[test]
    fn tools_list_marks_session_class() {
        let tools = tools_v1();
        let arr = tools.as_array().expect("tool array");
        let session: Vec<&str> = arr
            .iter()
            .filter(|t| t["class"] == "session")
            .map(|t| t["name"].as_str().expect("name"))
            .collect();
        assert_eq!(
            session,
            [
                "session.list_saves",
                "session.save",
                "session.load",
                "session.snapshot"
            ]
        );
        assert!(arr.iter().all(|t| t["inputSchema"].is_object()));
    }

    #[test]
    fn notifications_are_never_answered() {
        let (m, _) = mcp();
        assert!(
            m.handle(
                0,
                &json!({"jsonrpc": "2.0", "method": "notifications/initialized"})
            )
            .is_none()
        );
        assert!(
            m.handle(0, &json!({"jsonrpc": "2.0", "method": "ping"}))
                .is_none()
        );
        assert!(
            m.handle(0, &json!({"jsonrpc": "2.0", "id": 7, "result": {}}))
                .is_none()
        );
    }

    #[test]
    fn tool_call_round_trip() {
        let (m, game) = mcp();
        let out = m
            .handle(
                0,
                &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                        "params": {"name": "timewarp.set", "arguments": {"level": 4}}}),
            )
            .expect("response");
        assert_eq!(out["result"]["isError"], false);
        game.with_world(|w| {
            // The world stepped one tick — the command applied.
            assert_eq!(w.warp_level(), 4);
            assert_eq!(w.clock().ticks(), 1);
            let (key, _) = w.command_log().last().expect("a command");
            assert_eq!(key.source_class, csp_sim::SourceClass::External);
        });
        // telemetry snapshot republished at tick 1.
        let snap = m.backend().telemetry().expect("snapshot");
        assert_eq!(snap.tick, 1);
        assert_eq!(snap.warp_level, 4);
    }

    #[test]
    fn unknown_tool_is_invalid_params() {
        let (m, _) = mcp();
        let out = m
            .handle(
                0,
                &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                        "params": {"name": "nope", "arguments": {}}}),
            )
            .expect("response");
        assert_eq!(out["error"]["code"], INVALID_PARAMS);
    }
}
