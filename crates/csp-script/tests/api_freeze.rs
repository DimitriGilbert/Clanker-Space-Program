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
