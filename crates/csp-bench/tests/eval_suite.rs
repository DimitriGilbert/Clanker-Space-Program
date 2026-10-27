#![allow(clippy::unwrap_used, clippy::expect_used)]
//! M9d §2.12.1 eval-tier e2e — the committed scenario suite, the
//! lockstep determinism pin, the format's failure surface, and the
//! `csp-eval`/`csp-mcp` binary paths (the harness an external eval
//! driver spawns — §2.12.1's "an eval harness spawns `csp-mcp`").

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use csp_bench::eval::run_suite;
use csp_bench::scenario::{self, EvalError, ScenarioFile};
use serde_json::{Value, json};

fn suite_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/eval-scenarios")
}

fn fixtures_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/test-fixtures")
}

fn data_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data")
}

fn load(name: &str) -> ScenarioFile {
    scenario::load_scenario(&suite_dir().join(format!("{name}.ron"))).expect("scenario parses")
}

// ---------------------------------------------------------------------------
// The committed suite
// ---------------------------------------------------------------------------

/// Every scenario committed to `data/eval-scenarios/` runs and passes —
/// the suite is additive-by-file, so a red scenario fails this test by
/// name.
#[test]
fn committed_eval_suite_all_pass() {
    let entries = run_suite(&suite_dir(), &fixtures_root(), &data_root()).expect("suite reads");
    assert!(!entries.is_empty(), "suite dir has no .ron files");
    let mut failures = Vec::new();
    for entry in &entries {
        match &entry.outcome {
            Ok(report) => {
                if !report.pass {
                    let missed: Vec<String> = report
                        .goals
                        .iter()
                        .filter(|g| !g.passed)
                        .map(|g| {
                            format!(
                                "{} {:?} {:?}",
                                g.predicate.field, g.predicate.op, g.observed
                            )
                        })
                        .collect();
                    failures.push(format!(
                        "{}: goals unmet {missed:?} faults {:?} stop {:?}",
                        report.scenario, report.agent_faults, report.stop_reason
                    ));
                }
            }
            Err(e) => failures.push(format!("{}: {e}", entry.path.display())),
        }
    }
    assert!(
        failures.is_empty(),
        "eval suite failures:\n{}",
        failures.join("\n")
    );
    // The flagship is in the suite — the "agent flies a mission"
    // scenario must be present, not just any passing file.
    assert!(
        entries
            .iter()
            .any(|e| e.path.ends_with("ascent-mission.ron")),
        "ascent-mission.ron missing from the suite"
    );
}

/// §2.12.1's honest reproducibility scope: the format's deterministic
/// sources (Schedule/Program/Log — not a live LLM) replay byte-for-
/// byte — same scenario twice ⇒ same tick, same state hash, same
/// transcript.
#[test]
fn lockstep_determinism_byte_identical() {
    let file = load("ascent-mission");
    let (a, ta) =
        scenario::run_scenario_full(&file, &fixtures_root(), &data_root()).expect("run a");
    let (b, tb) =
        scenario::run_scenario_full(&file, &fixtures_root(), &data_root()).expect("run b");
    assert_eq!(a.final_tick, b.final_tick);
    assert_eq!(a.final_hash, b.final_hash);
    assert_eq!(ta.len(), tb.len());
    assert_eq!(a.score.dv_used_mps, b.score.dv_used_mps);
    // The mission actually flew — a degenerate zero-tick agreement
    // would be a false pass.
    assert!(a.final_tick > 1_000);
    assert!(a.score.dv_used_mps > 1_000.0);
}

/// The `Schedule` source twice — the cheaper determinism pin for the
/// non-physical path.
#[test]
fn schedule_scenario_deterministic() {
    let file = load("idle-warp");
    let a = scenario::run_scenario(&file, &fixtures_root(), &data_root()).expect("run a");
    let b = scenario::run_scenario(&file, &fixtures_root(), &data_root()).expect("run b");
    assert_eq!((a.final_tick, a.final_hash), (b.final_tick, b.final_hash));
    assert!(a.pass);
}

/// §2.6.5 inside `setup`: an `AdvanceTo` past `MAX_TICKS_PER_FRAME`
/// (69 120) splits into partial frames — the setup drain must
/// re-issue the effective target until the run lands, not drop the
/// tail at the first partial.
#[test]
fn setup_advance_to_survives_frame_budget_partials() {
    let text = r#"(
        schema_version: 1,
        name: "long-setup",
        world: Empty,
        setup: [Command(AdvanceTo(200_000))],
        agent: Idle,
        goals: [(field: "tick", op: ge, value: 200_000)],
        tick_budget: 1_000_000,
        event_budget: 16,
    )"#;
    let file = scenario::parse_scenario(text).expect("parses");
    let report = scenario::run_scenario(&file, &fixtures_root(), &data_root()).expect("runs");
    // The setup advance landed the full 200 000 — the Idle agent then
    // gets its one boundary before `Done`, so the clock stands one
    // past it.
    assert_eq!(report.final_tick, 200_001);
    assert!(report.pass, "setup partial was dropped: {report:?}");
}

/// §2.7: `save_seed` is consumed at world build — two scenarios
/// differing only in the seed hash differently (the seed rides the
/// hashed state; a never-applied seed would collide them).
#[test]
fn save_seed_changes_final_hash() {
    let make = |seed: u64| {
        let text = format!(
            r#"(
                schema_version: 1,
                name: "seeded",
                world: Empty,
                save_seed: {seed},
                agent: Schedule([[SetWarp(2)]]),
                goals: [(field: "tick", op: ge, value: 0)],
                tick_budget: 1_000,
                event_budget: 16,
            )"#
        );
        scenario::parse_scenario(&text).expect("parses")
    };
    let a = scenario::run_scenario(&make(0), &fixtures_root(), &data_root()).expect("run a");
    let b = scenario::run_scenario(&make(42), &fixtures_root(), &data_root()).expect("run b");
    assert!(a.pass && b.pass);
    assert_ne!(a.final_hash, b.final_hash);
}

// ---------------------------------------------------------------------------
// The format's failure surface
// ---------------------------------------------------------------------------

/// Malformed RON is a `Parse` error; a valid file with a wrong
/// schema_version is `Invalid`; unknown fixture/program is `Setup`.
/// None of them is a verdict — a verdict is `pass: false`.
#[test]
fn scenario_format_failure_classes() {
    assert!(matches!(
        scenario::parse_scenario("not ron at all ((("),
        Err(EvalError::Parse(_))
    ));
    let bad_version = r#"(
        schema_version: 99,
        name: "v", world: Empty, agent: Idle,
        tick_budget: 1, event_budget: 1,
    )"#;
    assert!(matches!(
        scenario::parse_scenario(bad_version),
        Err(EvalError::Invalid(_))
    ));
    let unknown_field = r#"(
        schema_version: 1,
        name: "v", world: Empty, agent: Idle, bogus_key: 3,
        tick_budget: 1, event_budget: 1,
    )"#;
    // deny_unknown_fields → Parse, not a silent default.
    assert!(matches!(
        scenario::parse_scenario(unknown_field),
        Err(EvalError::Parse(_))
    ));
    let unknown_fixture = r#"(
        schema_version: 1,
        name: "v", world: Fixture(name: "no-such"), agent: Idle,
        goals: [(field: "tick", op: ge, value: 0)],
        tick_budget: 1, event_budget: 1,
    )"#;
    let file = scenario::parse_scenario(unknown_fixture).expect("parses");
    assert!(matches!(
        scenario::run_scenario(&file, &fixtures_root(), &data_root()),
        Err(EvalError::Setup(_))
    ));
    let unknown_program = r#"(
        schema_version: 1,
        name: "v", world: Empty,
        agent: Program(name: "no-such-program", args: {}),
        goals: [(field: "tick", op: ge, value: 0)],
        tick_budget: 1, event_budget: 1,
    )"#;
    let file = scenario::parse_scenario(unknown_program).expect("parses");
    assert!(matches!(
        scenario::run_scenario(&file, &fixtures_root(), &data_root()),
        Err(EvalError::Setup(_))
    ));
}

/// A goal-less scenario is refused at `validate()` — `pass` on an
/// empty goal list is vacuously true, so the format rejects it
/// outright rather than green-lighting any run.
#[test]
fn zero_goal_scenario_is_rejected() {
    let text = r#"(
        schema_version: 1,
        name: "vacuous",
        world: Empty,
        agent: Idle,
        tick_budget: 2,
        event_budget: 2,
    )"#;
    assert!(matches!(
        scenario::parse_scenario(text),
        Err(EvalError::Invalid(_))
    ));
}

/// An unmet goal is `pass: false` on an `Ok` report — the
/// verdict-vs-error split the exit codes map onto.
#[test]
fn unmet_goal_is_a_verdict_not_an_error() {
    let text = r#"(
        schema_version: 1,
        name: "verdict-fail",
        world: Empty,
        agent: Schedule([[AdvanceTo(64)]]),
        goals: [(field: "tick", op: ge, value: 100000)],
        tick_budget: 1000,
        event_budget: 16,
    )"#;
    let file = scenario::parse_scenario(text).expect("parses");
    let report = scenario::run_scenario(&file, &fixtures_root(), &data_root()).expect("runs");
    assert!(!report.pass);
    assert_eq!(report.goals.len(), 1);
    assert!(!report.goals[0].passed);
}

/// `event_budget` stops the run mid-agent — the bound is a verdict
/// input (`stop_reason`), never a panic.
#[test]
fn event_budget_bounds_the_run() {
    let text = r#"(
        schema_version: 1,
        name: "budgeted",
        world: Empty,
        agent: Schedule([[SetWarp(1)], [SetWarp(2)], [SetWarp(3)]]),
        goals: [(field: "tick", op: ge, value: 0)],
        tick_budget: 1_000,
        event_budget: 1,
    )"#;
    let file = scenario::parse_scenario(text).expect("parses");
    let report = scenario::run_scenario(&file, &fixtures_root(), &data_root()).expect("runs");
    assert_eq!(
        report.stop_reason,
        csp_bench::agent::StopReason::EventBudget
    );
    assert!(report.events_applied > 0);
}

// ---------------------------------------------------------------------------
// csp-eval — the bin path the plan's "`→ exit code + hash + score`" names
// ---------------------------------------------------------------------------

/// `csp-eval <scenario>` exits 0 on a passing scenario and prints the
/// report JSON the plan's contract describes.
#[test]
fn csp_eval_bin_reports_and_exits() {
    let out = Command::new(env!("CARGO_BIN_EXE_csp-eval"))
        .arg(suite_dir().join("idle-warp.ron"))
        .arg("--fixtures-root")
        .arg(fixtures_root())
        .arg("--data-root")
        .arg(data_root())
        .arg("--transcript")
        .arg(std::env::temp_dir().join(format!("csp-eval-t-{}", std::process::id())))
        .output()
        .expect("spawn csp-eval");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report: Value = serde_json::from_slice(&out.stdout).expect("report json");
    assert_eq!(report["pass"], json!(true));
    assert!(report["final_hash"].as_str().unwrap().len() == 64);
    assert!(report["score"]["sim_time_s"].as_f64().unwrap() > 0.0);
}

/// C30-F4: the §2.12.1 score covers `setup` — the
/// `contract-lifecycle` scenario's setup `AdvanceTo(640)` spends
/// 10 sim-seconds before the agent loop binds, and its
/// `ManufactureCraft` debits funds before the loop too. Both must
/// land inside `score.sim_time_s`/`score.funds_spent_micros` — a
/// score that measured only the post-setup lockstep would bill the
/// run's setup work to nobody.
#[test]
fn the_score_covers_setup_work() {
    let file = scenario::load_scenario(&suite_dir().join("contract-lifecycle.ron")).expect("parse");
    let report = scenario::run_scenario(&file, &fixtures_root(), &data_root()).expect("runs");
    assert!(report.pass, "scenario failed: {:?}", report.stop_reason);
    // 10 s of setup advance alone, plus the loop's time.
    assert!(
        report.score.sim_time_s >= 10.0,
        "setup's advance must bill into sim_time_s: {:?}",
        report.score
    );
    // Funds moved — manufacture's debit is inside the delta (the
    // contract pays it back plus profit, so the sign is negative;
    // zero would mean setup spent nothing).
    assert_ne!(report.score.funds_spent_micros, 0);
}

/// `csp-eval --suite` runs the whole directory; a passing suite is
/// exit 0 (the committed suite is asserted green by
/// `committed_eval_suite_all_pass` — this pins the bin's verdict
/// mapping).
#[test]
fn csp_eval_bin_suite_exit_code() {
    let out = Command::new(env!("CARGO_BIN_EXE_csp-eval"))
        .arg("--suite")
        .arg(suite_dir())
        .arg("--fixtures-root")
        .arg(fixtures_root())
        .arg("--data-root")
        .arg(data_root())
        .output()
        .expect("spawn csp-eval");
    assert!(
        out.status.success(),
        "suite run failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

// ---------------------------------------------------------------------------
// csp-mcp — the §2.12.1 stdio eval host, driven end-to-end
// ---------------------------------------------------------------------------

/// A spawned `csp-mcp` process — newline-delimited JSON-RPC on
/// stdin/stdout (the bin's documented wire).
struct McpProc {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl McpProc {
    fn spawn(fixture: &PathBuf, save_dir: &PathBuf) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_csp-mcp"))
            .arg("--fixture")
            .arg(fixture)
            .arg("--save-dir")
            .arg(save_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn csp-mcp");
        Self {
            stdin: child.stdin.take().expect("stdin"),
            stdout: BufReader::new(child.stdout.take().expect("stdout")),
            child,
            next_id: 0,
        }
    }

    /// One request → one response (per-request response discipline is
    /// the stdio bin's contract — notifications get none).
    fn rpc(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let msg = json!({"jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params});
        writeln!(self.stdin, "{msg}").expect("write");
        self.stdin.flush().expect("flush");
        let mut line = String::new();
        self.stdout.read_line(&mut line).expect("read");
        serde_json::from_str(&line).expect("response json")
    }

    /// A `tools/call` → the tool's typed JSON result payload (the
    /// `content[0].text` JSON the surface wraps results in). A
    /// non-JSON payload is a test failure, not a value — the mission
    /// asserts on typed fields, and a `{"raw": ...}` escape hatch
    /// would let any malformed response pass them.
    fn tool(&mut self, name: &str, args: Value) -> Value {
        let resp = self.rpc("tools/call", json!({"name": name, "arguments": args}));
        let text = resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("tool {name} failed: {resp}"));
        serde_json::from_str(text)
            .unwrap_or_else(|e| panic!("tool {name} returned non-JSON payload {text:?}: {e}"))
    }
}

impl Drop for McpProc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The §2.12.1 end-to-end mission: an agent drives `csp-mcp` (the
/// stdio MCP host over its own headless sim) through the frozen tool
/// surface — spawn, lease, telemetry-feedback ascent — and the sim
/// answers with a real climb. The agent here is the test (the eval
/// harness's role); the lockstep is the tool-call → drain →
/// `telemetry.get` round-trip.
#[test]
fn mcp_mission_agent_flies_ascent() {
    let fixture = fixtures_root().join("flight-launch");
    let save_dir = std::env::temp_dir().join(format!("csp-mcp-mission-{}", std::process::id()));
    let mut mcp = McpProc::spawn(&fixture, &save_dir);

    // initialize → pinned revision.
    let init = mcp.rpc(
        "initialize",
        json!({"protocolVersion": "2026-07-28", "capabilities": {}}),
    );
    assert_eq!(init["result"]["protocolVersion"], "2026-07-28");

    // tools/list — the eval surface exists and is versioned.
    let tools = mcp.rpc("tools/list", json!({}));
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    for want in [
        "telemetry.get",
        "vessel.control",
        "vessel.take_control",
        "timewarp.advance_to",
    ] {
        assert!(names.contains(&want), "missing tool {want}");
    }

    // Spawn the recipe's authored booster on the pad — the agent's
    // world setup, through `sim.command` (the escape hatch).
    let world_probe = {
        let recipe = csp_bench::scenarios::find("flight-launch").expect("recipe");
        (recipe.initial)(&fixture).expect("fixture world")
    };
    let env = world_probe.environment().expect("env").clone();
    let pad = csp_core::DVec3::new(env.radius_m + 3.5, 0.0, 0.0);
    let spawn = csp_sim::SimCommand::SpawnPhysicalVessel {
        craft: csp_bench::scenarios::flight_craft(),
        pos: pad,
        rot: csp_core::DQuat::from_axis_angle(csp_core::DVec3::Y, core::f64::consts::FRAC_PI_2),
        vel: env.body_spin.cross(pad - env.body_center),
        ang_vel: csp_core::DVec3::ZERO,
    };
    let r = mcp.tool(
        "sim.command",
        json!({"command": serde_json::to_value(spawn).unwrap()}),
    );
    assert_eq!(r["rejected"], json!([]));

    // The lease handshake — `vessel.control` without it must fail,
    // then `take_control` fixes it. The gate is exercised, not
    // assumed.
    let denied = mcp.tool("vessel.control", json!({"vessel": 0, "throttle": 1.0}));
    assert!(
        denied["rejected"].as_array().is_some_and(|r| !r.is_empty()),
        "lease-less control should reject: {denied}"
    );
    mcp.tool("vessel.take_control", json!({"vessel": 0}));
    let list = mcp.tool("vessel.list", json!({}));
    assert_eq!(list["vessels"][0]["control_lease"], json!({"Agent": 0}));

    // The ascent: telemetry-feedback steering through `vessel.control`
    // + chunked `timewarp.advance_to` — the lockstep round-trip a real
    // agent driver performs. Target: apoapsis ≥ 40 km (the full
    // 120 km mission is the lockstep suite's job — this proves the
    // wire path).
    let mut telemetry = mcp.tool("telemetry.get", json!({"vessel": 0}));
    let mut chunks = 0;
    loop {
        let tick = telemetry["tick"].as_u64().expect("tick");
        let apo = telemetry["apoapsis_altitude_m"].as_f64().unwrap_or(-1.0);
        if apo >= 40_000.0 {
            break;
        }
        assert!(
            chunks < 200,
            "agent never reached 40 km apoapsis: {telemetry}"
        );
        // The pitch program — same profile as the lockstep suite's
        // `ascent-apoapsis` program, computed here by the "agent".
        let pos = vec3(&telemetry["pos_m"]);
        let vel = vec3(&telemetry["vel_mps"]);
        let alt = telemetry["altitude_m"].as_f64().unwrap_or(0.0);
        let dir = ascent_steer(pos, vel, alt);
        mcp.tool(
            "vessel.control",
            json!({"vessel": 0, "steer_dir": dir, "throttle": 1.0}),
        );
        // Chunk the advance — the held absolute attitude covers the
        // span (the M2 control surface is absolute until re-commanded).
        mcp.tool("timewarp.advance_to", json!({"tick": tick + 64}));
        telemetry = mcp.tool("telemetry.get", json!({"vessel": 0}));
        chunks += 1;
    }
    mcp.tool("vessel.control", json!({"vessel": 0, "throttle": 0.0}));
    let done = mcp.tool("telemetry.get", json!({"vessel": 0}));
    assert_eq!(done["throttle"], json!(0.0));
    assert!(done["apoapsis_altitude_m"].as_f64().unwrap() >= 40_000.0);
    // The session-class tail: snapshot reads the world's summary.
    let summary = mcp.tool("session.snapshot", json!({}));
    assert_eq!(
        summary["tick"].as_u64().unwrap(),
        done["tick"].as_u64().unwrap()
    );

    // C30-F3: `session.save`/`session.load` round-trip through the
    // bin's `with_reconfigure` hook — the closure that re-installs
    // the script engine, autopilot driver, and op budget on the
    // restored world (the gap in MCP e2e coverage: every prior run
    // stopped at `session.snapshot`). Save the flown state, load it
    // back, and prove the restored world is still usable — a
    // missing reconfigure would surface here as an advance/script
    // rejection, not a quiet divergence.
    let save = mcp.tool("session.save", json!({"name": "mission-tail"}));
    assert!(save["name"].as_str().is_some(), "save failed: {save}");
    let loaded = mcp.tool("session.load", json!({"name": "mission-tail"}));
    assert_eq!(
        loaded["tick"].as_u64().unwrap(),
        done["tick"].as_u64().unwrap(),
        "loaded world dropped the clock"
    );
    // The restored world still advances — the reconfigure-installed
    // channels (engine, budget, autopilot) are live, not just the
    // serialized state.
    let tick0 = loaded["tick"].as_u64().unwrap();
    mcp.tool("timewarp.advance_to", json!({"tick": tick0 + 64}));
    let after = mcp.tool("session.snapshot", json!({}));
    assert_eq!(after["tick"].as_u64().unwrap(), tick0 + 64);
    let list = mcp.tool("vessel.list", json!({}));
    assert_eq!(
        list["vessels"].as_array().map(Vec::len),
        Some(1),
        "the vessel survived the round-trip"
    );
}

fn vec3(v: &Value) -> [f64; 3] {
    [
        v["x"].as_f64().unwrap_or(0.0),
        v["y"].as_f64().unwrap_or(0.0),
        v["z"].as_f64().unwrap_or(0.0),
    ]
}

/// The test-agent's ascent steering — `up·sin(pitch)+prog·cos(pitch)`
/// over the gravity-turn band (the lockstep `ascent-apoapsis`
/// program's math, standalone so the MCP path is exercised by an
/// independent computation).
fn ascent_steer(pos: [f64; 3], vel: [f64; 3], alt: f64) -> [f64; 3] {
    let r = (pos[0] * pos[0] + pos[1] * pos[1] + pos[2] * pos[2]).sqrt();
    let up = if r > 0.0 {
        [pos[0] / r, pos[1] / r, pos[2] / r]
    } else {
        [1.0, 0.0, 0.0]
    };
    let vr = vel[0] * up[0] + vel[1] * up[1] + vel[2] * up[2];
    let vh = [
        vel[0] - vr * up[0],
        vel[1] - vr * up[1],
        vel[2] - vr * up[2],
    ];
    let hn = (vh[0] * vh[0] + vh[1] * vh[1] + vh[2] * vh[2]).sqrt();
    let prog = if hn > 1e-3 {
        [vh[0] / hn, vh[1] / hn, vh[2] / hn]
    } else {
        [0.0, up[2], -up[1]] // any horizontal axis
    };
    let f = ((alt - 1500.0) / 58500.0).clamp(0.0, 1.0);
    let pitch = core::f64::consts::FRAC_PI_2 * (1.0 - f);
    let (sp, cp) = (pitch.sin(), pitch.cos());
    [
        up[0] * sp + prog[0] * cp,
        up[1] * sp + prog[1] * cp,
        up[2] * sp + prog[2] * cp,
    ]
}
