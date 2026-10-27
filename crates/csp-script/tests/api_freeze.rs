//! M9c §6 — the `ControlApi` freeze test.
//!
//! The `ControlApi` trait is **frozen** at the end of M9c: the MCP
//! tool surface (`csp.mcp.tools.v1`) and the Rhai guest surface are
//! declared against it, so adding, removing, or reshaping a method is
//! a deliberate compatibility event requiring a tool-schema major
//! version bump. This test pins the method set by *reading the
//! source* — a hand-maintained list elsewhere in the tree could drift
//! silently; the trait's own declarations cannot.

/// The frozen `ControlApi` method set — `(name, normalized-signature
/// fragment)`. Signatures are compared after whitespace collapse, so
/// rustfmt line-wrapping is not a false positive; a parameter or
/// return-type change IS a caught drift.
const FROZEN_METHODS: &[(&str, &str)] = &[
    ("telemetry", "(&mut self) -> VesselTelemetry"),
    (
        "telemetry_of",
        "(&mut self, vessel: VesselId) -> VesselTelemetry",
    ),
    ("set_throttle", "(&mut self, fraction: f64)"),
    ("steer_dir", "(&mut self, dir: DVec3)"),
    ("hold_attitude", "(&mut self)"),
    ("jettison", "(&mut self, node: u64)"),
    ("add_node", "(&mut self, tick: u64, dv: DVec3)"),
    ("delete_node", "(&mut self)"),
    (
        "transfer_resource",
        "(&mut self, to: VesselId, resource: &str, amount: f64)",
    ),
    ("add_alarm", "(&mut self, tick: u64, label: String)"),
    (
        "request_wake",
        "(&mut self, condition: WakeCondition) -> Result<u64, SimError>",
    ),
    ("cancel_wake", "(&mut self) -> Result<(), SimError>"),
    ("log", "(&mut self, line: String)"),
    ("deploy", "(&mut self, node: u64)"),
    (
        "engage_autopilot",
        "(&mut self, target_apoapsis_m: f64, target_periapsis_m: f64)",
    ),
    ("disengage_autopilot", "(&mut self)"),
    ("vessels", "(&mut self) -> Vec<VesselId>"),
];

/// The frozen guest-callable surface — the `(name, arg-list)` pairs
/// of `CONTROL_API_SURFACE`, in declaration order.
const FROZEN_GUEST_SURFACE: &[(&str, &str)] = &[
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

/// Whitespace-collapsed source — `"fn  foo ( &mut"` and
/// `"fn foo(&mut"` compare equal.
fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Extract the `pub trait ControlApi { ... }` block's `fn`
/// declarations as `(name, collapsed "(&mut self ...) [-> T]")`.
fn trait_methods(source: &str) -> Vec<(String, String)> {
    let start = source
        .find("pub trait ControlApi {")
        .unwrap_or_else(|| panic!("ControlApi trait not found"));
    let body = &source[start..];
    // The trait ends at the first line that is exactly `}` — its
    // methods are indented, so a column-0 `}` closes the block.
    let end = body
        .find("\n}\n")
        .unwrap_or_else(|| panic!("ControlApi trait block unterminated"));
    let mut out = Vec::new();
    for line in body[..end].lines() {
        let line = line.trim();
        if !line.starts_with("fn ") {
            continue;
        }
        // `fn name(&mut self, ...) -> Ret;` — split name from the rest
        // at the first `(`.
        let sig = &line[3..];
        let Some(paren) = sig.find('(') else {
            continue;
        };
        let name = sig[..paren].trim().to_string();
        let rest = collapse(sig[paren..].trim_end_matches(';'));
        out.push((name, rest));
    }
    out
}

#[test]
fn control_api_trait_surface_is_frozen() {
    let source = include_str!("../src/lib.rs");
    let methods = trait_methods(source);
    let got: Vec<(&str, &str)> = methods
        .iter()
        .map(|(n, s)| (n.as_str(), s.as_str()))
        .collect();
    assert_eq!(
        got,
        FROZEN_METHODS.to_vec(),
        "ControlApi drifted from the M9c freeze — changing this surface \
         requires a csp.mcp.tools major-version bump (§6)"
    );
}

#[test]
fn guest_surface_const_is_frozen() {
    assert_eq!(
        csp_script::CONTROL_API_SURFACE,
        FROZEN_GUEST_SURFACE,
        "CONTROL_API_SURFACE drifted from the M9c freeze (§6)"
    );
}

/// The frozen surface is also complete against the trait: every
/// `ControlApi` mutation verb has a guest-callable name, so the Rhai
/// host and the MCP tools see one surface. (Telemetry reads have no
/// host-fn name — `ctx.vessel` carries them — and are exempt by
/// name-list below.)
#[test]
fn frozen_surface_covers_the_trait_verbs() {
    // Trait methods that are pure reads — they surface through `ctx`,
    // not through a callable host function.
    const READ_ONLY: &[&str] = &["telemetry", "telemetry_of"];
    // Verb → guest name where they differ (`transfer_resource` is
    // `transfer`, `steer_dir` is `steer`/`steer_quat`, `request_wake`
    // is the `wait_*` family, `cancel_wake` is `cancel_wait`,
    // `hold_attitude` is `hold_attitude`, `log` is `log`/`print`).
    let mut uncovered = Vec::new();
    for (name, _) in FROZEN_METHODS {
        if READ_ONLY.contains(name) {
            continue;
        }
        let covered = match *name {
            "steer_dir" => FROZEN_GUEST_SURFACE
                .iter()
                .any(|(g, _)| *g == "steer" || *g == "steer_quat"),
            "transfer_resource" => FROZEN_GUEST_SURFACE.iter().any(|(g, _)| *g == "transfer"),
            "request_wake" => FROZEN_GUEST_SURFACE
                .iter()
                .any(|(g, _)| g.starts_with("wait_")),
            "cancel_wake" => FROZEN_GUEST_SURFACE
                .iter()
                .any(|(g, _)| *g == "cancel_wait"),
            "log" => FROZEN_GUEST_SURFACE
                .iter()
                .any(|(g, _)| *g == "log" || *g == "print"),
            other => FROZEN_GUEST_SURFACE.iter().any(|(g, _)| *g == other),
        };
        if !covered {
            uncovered.push(*name);
        }
    }
    assert!(
        uncovered.is_empty(),
        "frozen guest surface lost coverage of ControlApi verbs: {uncovered:?}"
    );
}

// ---------------------------------------------------------------------------
// The `ctx` schema — the map contract is part of the same frozen
// surface as the method set (a renamed key breaks scripts and MCP
// docs exactly like a renamed verb). Pinned BEHAVIORALLY: a stub
// `TickIo` drives the real `RhaiHost` and the guest itself logs the
// keys it sees, so the pin covers the wire the script actually
// reads, not a secondary description of it.
// ---------------------------------------------------------------------------

/// The frozen top-level `ctx` key set (sorted — a Rhai `Map` is a
/// `BTreeMap`, so `keys()` enumerates in byte order).
const FROZEN_CTX_KEYS: &[&str] = &["persist", "script", "tick", "vessel"];

/// The frozen `ctx.vessel` telemetry-map key set (sorted). Shared by
/// `ctx.vessel`, `telemetry_of(id)`, and every `vessels()` entry —
/// all three are built by the one `telemetry_to_map`. The
/// `node`/`node_tick` pair is CONDITIONAL (present only while a
/// maneuver node is armed) — the stub io here reports none, so they
/// are correctly absent from this list.
const FROZEN_CTX_VESSEL_KEYS: &[&str] = &[
    "altitude",
    "apoapsis",
    "body",
    "found",
    "horizontal_speed",
    "id",
    "mass",
    "mu",
    "parts",
    "periapsis",
    "pos",
    "residence",
    "resources",
    "rot",
    "speed",
    "throttle",
    "tick",
    "time_to_apoapsis",
    "time_to_periapsis",
    "vel",
    "vertical_speed",
];

/// A stub `TickIo` — the smallest `VesselTelemetry` that exercises
/// the optional-field encoding (`apoapsis: None` → the NaN sentinel
/// the ctx contract documents).
struct StubIo {
    telemetry: csp_sim::VesselTelemetry,
    commands: Vec<csp_sim::SimCommand>,
    logs: Vec<String>,
}

impl StubIo {
    fn new() -> Self {
        Self {
            telemetry: csp_sim::VesselTelemetry {
                tick: 0,
                found: true,
                residence: csp_sim::TelemetryResidence::InBubble,
                pos_m: csp_core::DVec3::new(6.0e5, 0.0, 0.0),
                vel_mps: csp_core::DVec3::new(0.0, 10.0, 0.0),
                rot: csp_core::DQuat::IDENTITY,
                ang_vel_mps: csp_core::DVec3::ZERO,
                throttle: 0.0,
                altitude_m: Some(100.0),
                speed_mps: 10.0,
                vertical_speed_mps: 0.0,
                horizontal_speed_mps: 10.0,
                mass_kg: Some(1_000.0),
                apoapsis_altitude_m: None, // open-conic → NaN sentinel
                periapsis_altitude_m: Some(40_000.0),
                time_to_apoapsis_s: None,
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
        }
    }
}

impl csp_sim::TickIo for StubIo {
    fn telemetry(&self, _v: csp_sim::VesselId) -> csp_sim::VesselTelemetry {
        self.telemetry.clone()
    }
    fn vessels(&self) -> Vec<csp_sim::VesselId> {
        vec![csp_sim::VesselId(0)]
    }
    fn issue(&mut self, _c: csp_sim::SourceClass, _s: u32, cmd: csp_sim::SimCommand) {
        self.commands.push(cmd);
    }
    fn request_wake(
        &mut self,
        _s: csp_sim::ScriptId,
        _cond: &csp_sim::WakeCondition,
    ) -> Result<u64, csp_core::SimError> {
        Ok(99)
    }
    fn cancel_wake(&mut self, _s: csp_sim::ScriptId) -> Result<(), csp_core::SimError> {
        Ok(())
    }
    fn log_line(&mut self, _s: csp_sim::ScriptId, line: &str) {
        self.logs.push(line.to_string());
    }
}

/// Run `body` as `fn on_tick(ctx)` for one tick against a fresh
/// host + stub io; returns the report and the guest's `log` lines.
fn tick_script(body: &str) -> (csp_sim::ScriptRunReport, StubIo) {
    use csp_sim::ScriptEngine as _;
    let src = format!("fn on_tick(ctx) {{ {body} }}");
    let mut host = csp_script::RhaiHost::new();
    host.prepare(csp_sim::ScriptId(0), &src)
        .unwrap_or_else(|e| panic!("{body}: prepare: {e}"));
    let mut state = csp_sim::ScriptState {
        id: csp_sim::ScriptId(0),
        vessel: csp_sim::VesselId(0),
        source: src,
        enabled: true,
        disabled_reason: None,
        strikes: 0,
        consecutive_polls: 0,
        poll_demoted: false,
        wake_tick: None,
        persist: csp_sim::PersistMap::new(),
    };
    let mut io = StubIo::new();
    let report = host.run_tick(&mut state, 1, &mut io);
    (report, io)
}

#[test]
fn ctx_top_level_key_set_is_frozen() {
    let (r, io) = tick_script(r#"for k in ctx.keys() { log(k); }"#);
    assert!(r.fault.is_none(), "{:?}", r.fault);
    assert_eq!(
        io.logs,
        FROZEN_CTX_KEYS.to_vec(),
        "ctx top-level keys drifted — a rename/add/remove is a \
         csp.mcp.tools major-version event (§6)"
    );
}

#[test]
fn ctx_vessel_key_set_is_frozen() {
    let (r, io) = tick_script(r#"for k in ctx.vessel.keys() { log(k); }"#);
    assert!(r.fault.is_none(), "{:?}", r.fault);
    assert_eq!(
        io.logs,
        FROZEN_CTX_VESSEL_KEYS.to_vec(),
        "ctx.vessel keys drifted — a rename/add/remove is a \
         csp.mcp.tools major-version event (§6)"
    );
}

/// `telemetry_of(id)` and every `vessels()` entry carry the SAME
/// frozen key set as `ctx.vessel` — one builder, one schema.
#[test]
fn vessels_and_telemetry_of_share_the_vessel_schema() {
    let (r, io) = tick_script(
        r#"
        for k in vessels()[0].keys() { log(`l:${k}`); }
        for k in telemetry_of(ctx.vessel.id).keys() { log(`o:${k}`); }
        "#,
    );
    assert!(r.fault.is_none(), "{:?}", r.fault);
    let listed: Vec<String> = io
        .logs
        .iter()
        .filter(|l| l.starts_with("l:"))
        .map(|l| l[2..].to_string())
        .collect();
    let of: Vec<String> = io
        .logs
        .iter()
        .filter(|l| l.starts_with("o:"))
        .map(|l| l[2..].to_string())
        .collect();
    assert_eq!(listed, FROZEN_CTX_VESSEL_KEYS.to_vec(), "vessels() keys");
    assert_eq!(of, FROZEN_CTX_VESSEL_KEYS.to_vec(), "telemetry_of() keys");
}

/// The documented NaN contract, pinned at both ends: an unknown
/// optional numeric reads as NaN to the guest (`v.is_nan()`), and a
/// NaN copied into `ctx.persist` faults at write-back — the
/// `dynamic_to_persist` trap the ctx contract warns about.
#[test]
fn ctx_nan_contract_is_pinned() {
    // Stub telemetry has `apoapsis: None` — the sentinel value must
    // reach the guest as NaN (the guest test is `is_nan()`; Rhai's
    // total-order `==` makes `v != v` false even for NaN).
    let (r, io) = tick_script(r#"log(ctx.vessel.apoapsis.is_nan());"#);
    assert!(r.fault.is_none(), "{:?}", r.fault);
    assert_eq!(io.logs, vec!["true"], "apoapsis must read as NaN");
    // Copying that NaN into persist is a Runtime fault (a strike) —
    // "unsupported or non-finite value", never a silent save poison.
    let (r, _io) = tick_script(r#"ctx.persist["apo"] = ctx.vessel.apoapsis;"#);
    assert!(
        matches!(r.fault, Some(csp_sim::ScriptFault::Runtime(ref m)) if m.contains("non-finite")),
        "NaN into persist must fault as documented: {:?}",
        r.fault
    );
}
