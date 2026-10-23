//! M9d §2.12.1 — the declarative scenario format: `scenario.ron`.
//!
//! The plan's contract, verbatim:
//!
//! ```text
//! scenario.ron { initial_snapshot, data_fixture, command_source:
//! agent|script|log, goal_predicate, tick_budget, event_budget }
//!   → exit code + final state hash + score (Δv used, sim-time
//!     elapsed, funds spent).
//! ```
//!
//! A [`ScenarioFile`] is **data, not code**: the world it runs
//! (`world`), the setup the world needs before the loop (`setup` —
//! `PadLaunch` is the recipe's authored booster, `Command` a raw
//! `SimCommand` stamped `(Ui, 0)`), the command source driving the
//! lockstep ([`AgentSpec`]), the goal predicates evaluated on the
//! final published snapshot (`goals` — the §2.12.2 field-path
//! selectors, shared with the WS subscription protocol), and the two
//! budgets (`tick_budget` on elapsed sim ticks, `event_budget` on
//! applied commands).
//!
//! `initial_snapshot` and `data_fixture` from the plan's schema map
//! onto `world`: `Empty` is the bare `World::new` snapshot;
//! `Fixture{name}` is the golden recipe's fixture dir under the
//! fixtures root — the same construction `csp-replay record <name>`
//! uses, so an eval scenario and its golden share the starting state.
//!
//! [`run_scenario`] is the plan's "exit code + final state hash +
//! score" — the [`EvalReport`] the `csp-eval` bin renders.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};

use csp_core::{SimError, TICK_HZ};
use csp_server::telemetry::TelemetrySnapshot;
use csp_sim::{SimCommand, SourceClass, World};

use crate::agent::{Agent, AgentReply, Lockstep, StopReason};
use crate::replay::ReplayError;

/// The scenario format's schema version — bumps are deliberate.
pub const SCENARIO_SCHEMA_VERSION: u32 = 1;

/// Caps — the format's "no send me everything" equivalents: a
/// scenario file is a bounded declaration, never a program.
pub const MAX_GOALS: usize = 32;
/// `setup` entries cap.
pub const MAX_SETUP: usize = 64;
/// `Schedule` steps cap.
pub const MAX_SCHEDULE_STEPS: usize = 256;
/// `Log` entries cap — a scenario log is a probe, not a replay
/// artifact (`csp-replay` owns the big ones).
pub const MAX_LOG_STEPS: usize = 4_096;
/// Scenario name bound — it rides the report and test output.
pub const MAX_NAME: usize = 64;

/// The world the scenario runs on.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum WorldSpec {
    /// `World::new()` — the no-data world (scalar/warp scenarios).
    Empty,
    /// A registered recipe's fixture world — `name` is the recipe
    /// name and its directory under the fixtures root (`flight-launch`
    /// is the only non-trivial recipe today).
    Fixture {
        /// `crate::scenarios::find(name)` and `fixtures_root/name`.
        name: String,
    },
}

/// Pre-loop world mutation — applied in order before the agents
/// bind. `Command`s enqueue `(Ui, 0)` and drain at one trailing
/// boundary; `PadLaunch` is the flight recipe's authored pad spawn
/// (the megabytes of `WeldPart` stay in the recipe, not the file).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum SetupSpec {
    /// A raw `SimCommand`, enqueued `(Ui, 0)` — scenario authors get
    /// the full command surface (`SpawnVessel`, `SetAlarm`, …).
    /// Boxed: `SimCommand` is large and `PadLaunch` carries nothing.
    Command(Box<SimCommand>),
    /// `crate::scenarios::spawn_pad` — the authored booster on the
    /// Terrin pad (requires a fixture world; `Empty` rejects at run
    /// time with `EvalError::Setup`).
    PadLaunch,
}

/// One `Log` entry — re-issue `command` so it drains at `tick`
/// (§7.6's stamped-drain semantics, scenario-declared).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LoggedStep {
    /// The drain tick the command must land on.
    pub tick: u64,
    /// The command to enqueue for that boundary.
    pub command: SimCommand,
}

/// The scenario's command source — the plan's `agent|script|log`.
/// Every variant is an [`Agent`] under the lockstep contract; the
/// variants differ in *how commands are decided*.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum AgentSpec {
    /// No agent: setup drains, `stop_when`/goals evaluate. For a
    /// world that should still advance, put the `AdvanceTo` in
    /// `setup` — Idle is "observe only".
    Idle,
    /// A boundary schedule: each inner list is one boundary's
    /// commands; the agent returns `Done` after the last. The
    /// deterministic scripted agent — most eval scenarios.
    Schedule(Vec<Vec<SimCommand>>),
    /// A feedback-control program from `crate::eval::programs` —
    /// `args` are program-defined (see the registry doc).
    Program {
        /// The registry key (`"ascent-apoapsis"`, …).
        name: String,
        /// Program-defined numeric args (e.g. `target_apoapsis_m`).
        args: BTreeMap<String, f64>,
    },
    /// A Rhai guest script — the in-sim command source (M9a). The
    /// `LoadScript` lands *setup-stamped* (`Ui`) after `setup`: under
    /// §2.2.1's lease table an `External` client cannot load a script
    /// onto a `Player`-held vessel, and taking the lease first would
    /// then block the script's own verbs — the player installs
    /// control programs, scripts are the agent. The lockstep then
    /// `Wait`s on telemetry while the script ticks; termination is
    /// `stop_when`'s job (or `tick_budget`).
    Script {
        /// The vessel `LoadScript` binds.
        vessel: u64,
        /// The Rhai source (`on_tick` entry point, §2.10).
        source: String,
    },
    /// A verbatim command log — `LoggedStep`s re-issued so each
    /// drains at its recorded tick. Steps must be strictly ascending
    /// and ahead of the post-setup clock; a step the clock has
    /// passed is an agent `Fail` (a clock cannot retreat, §2.3).
    Log(Vec<LoggedStep>),
}

/// A comparison over a snapshot field path — both `goals` entries
/// (end-of-run assertions) and `stop_when` (early termination)
/// use it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GoalPredicate {
    /// The §2.12.2 field path (`"tick"`, `"vessels.0.altitude_m"`).
    pub field: String,
    /// The comparison.
    pub op: GoalOp,
    /// The expected side — numbers compare as f64, strings/bools
    /// structurally.
    pub value: GoalValue,
}

/// The comparison operator — lowercase in the file (`op: ge`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalOp {
    /// `observed > value` (numeric only).
    Gt,
    /// `observed >= value`.
    Ge,
    /// `observed < value`.
    Lt,
    /// `observed <= value`.
    Le,
    /// `observed == value` — numeric, string, or bool.
    Eq,
    /// `observed != value`.
    Ne,
}

/// The predicate's expected value.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum GoalValue {
    /// A numeric threshold.
    Num(f64),
    /// A boolean state.
    Bool(bool),
    /// A string state (`"OnRails"`, `"InBubble"`, …).
    Str(String),
}

impl GoalPredicate {
    /// Evaluate against the published snapshot — `Some(passed)` when
    /// the field resolved; `None` = the path does not exist (a
    /// missing field is a *failed* goal with `observed: None`, never
    /// a skipped one — the WS layer's `unknown_fields` rule).
    pub fn evaluate(&self, snap: &TelemetrySnapshot) -> (Option<serde_json::Value>, bool) {
        let observed = snap.field(&self.field);
        let pass = match (&observed, &self.value) {
            (Some(v), GoalValue::Num(want)) => v.as_f64().is_some_and(|got| self.cmp(got, *want)),
            (Some(v), GoalValue::Bool(want)) => match self.op {
                GoalOp::Eq => v.as_bool() == Some(*want),
                GoalOp::Ne => v.as_bool() == Some(!*want),
                _ => false,
            },
            (Some(v), GoalValue::Str(want)) => v.as_str().is_some_and(|got| match self.op {
                GoalOp::Eq => got == want,
                GoalOp::Ne => got != want,
                _ => false,
            }),
            _ => false,
        };
        (observed, pass)
    }

    fn cmp(&self, got: f64, want: f64) -> bool {
        match self.op {
            GoalOp::Gt => got > want,
            GoalOp::Ge => got >= want,
            GoalOp::Lt => got < want,
            GoalOp::Le => got <= want,
            GoalOp::Eq => got == want,
            GoalOp::Ne => got != want,
        }
    }
}

/// One goal's outcome in the report — the predicate, what the
/// snapshot actually held, and the verdict.
#[derive(Clone, Debug, Serialize)]
pub struct GoalOutcome {
    /// The predicate as declared.
    pub predicate: GoalPredicate,
    /// `TelemetrySnapshot::field(field)` — `None` = unknown path.
    pub observed: Option<serde_json::Value>,
    /// The verdict.
    pub passed: bool,
}

/// The plan's score triple — "(Δv used, sim-time elapsed, funds
/// spent)".
#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct EvalScore {
    /// Commanded-Δv expenditure: `∫ throttle·(Σ thrust/mass) dt` over
    /// bubble-resident vessels (m/s). *Commanded*, not achieved —
    /// off-axis burns pay full propellant.
    pub dv_used_mps: f64,
    /// Sim seconds elapsed over the run (`ticks / TICK_HZ`).
    pub sim_time_s: f64,
    /// `funds_micros` delta across the run (negative = earned).
    pub funds_spent_micros: i64,
}

/// The run's report — `csp-eval`'s stdout payload and the suite's
/// verdict record.
#[derive(Clone, Debug, Serialize)]
pub struct EvalReport {
    /// `ScenarioFile::name`.
    pub scenario: String,
    /// `true` = every goal held and no agent faulted.
    pub pass: bool,
    /// Why the loop stopped.
    pub stop_reason: StopReason,
    /// `World::clock` at evaluation time.
    pub final_tick: u64,
    /// `World::state_hash` — lowercase hex (the wire rendering).
    pub final_hash: String,
    /// The score triple.
    pub score: EvalScore,
    /// Commands applied across the run (the `event_budget` counter).
    pub events_applied: u64,
    /// Per-goal outcomes, in file order.
    pub goals: Vec<GoalOutcome>,
    /// Agent `Fail` replies surfaced at run end (a faulted agent is
    /// a failed run even if the goals incidentally hold).
    pub agent_faults: Vec<String>,
}

/// Harness-side failures — distinct from an honest `pass: false`
/// verdict (same split as `ReplayError`: malformed input and sim
/// faults are errors; unmet goals are a verdict).
#[derive(Debug)]
pub enum EvalError {
    /// The RON text did not parse.
    Parse(ron::error::SpannedError),
    /// The parsed file failed validation (schema version, caps).
    Invalid(String),
    /// `world`/`setup`/`agent` could not be built (unknown fixture
    /// recipe, `PadLaunch` on a physics-less world, unknown program).
    Setup(String),
    /// IO failure reading the scenario file.
    Io(std::io::Error),
    /// The sim faulted mid-run (§2.13).
    Sim(SimError),
}

impl fmt::Display for EvalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(e) => write!(f, "scenario parse: {e}"),
            Self::Invalid(m) => write!(f, "invalid scenario: {m}"),
            Self::Setup(m) => write!(f, "scenario setup: {m}"),
            Self::Io(e) => write!(f, "scenario file: {e}"),
            Self::Sim(e) => write!(f, "sim: {e}"),
        }
    }
}

impl std::error::Error for EvalError {}

impl From<SimError> for EvalError {
    fn from(e: SimError) -> Self {
        Self::Sim(e)
    }
}

/// Parse the RON text into a validated [`ScenarioFile`].
pub fn parse_scenario(text: &str) -> Result<ScenarioFile, EvalError> {
    let file: ScenarioFile = ron::de::from_str(text).map_err(EvalError::Parse)?;
    file.validate()?;
    Ok(file)
}

/// Read + parse a scenario file.
pub fn load_scenario(path: &Path) -> Result<ScenarioFile, EvalError> {
    let text = std::fs::read_to_string(path).map_err(EvalError::Io)?;
    parse_scenario(&text)
}

/// The top-level scenario declaration — see the module doc for the
/// plan mapping. `deny_unknown_fields`: a misspelled key is a parse
/// error, not a silently-defaulted field.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScenarioFile {
    /// Must equal [`SCENARIO_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// The scenario's name — report/test label.
    pub name: String,
    /// The §2.7 save seed the run rides (mirrors
    /// `ScenarioRecipe::save_seed`; M0's `World` draws no seeded
    /// streams yet — it is carried, not consumed).
    #[serde(default)]
    pub save_seed: u64,
    /// The world construction.
    pub world: WorldSpec,
    /// Pre-loop world mutation, applied in order.
    #[serde(default)]
    pub setup: Vec<SetupSpec>,
    /// The command source.
    pub agent: AgentSpec,
    /// End-of-run assertions over the final snapshot.
    #[serde(default)]
    pub goals: Vec<GoalPredicate>,
    /// Early termination: when this holds on a post-drain snapshot
    /// the loop stops with `StopReason::StopPredicate`. The
    /// `Script` source's natural end ("the script reached apoapsis").
    #[serde(default)]
    pub stop_when: Option<GoalPredicate>,
    /// Boundaries to keep stepping *after* `stop_when` first holds
    /// before goals evaluate — lets a reactive command source (a
    /// script's next `on_tick`, an agent's trailing cleanup) land
    /// its response before the assertions read state. Default 0.
    #[serde(default)]
    pub settle_ticks: u64,
    /// Max elapsed sim ticks before the loop stops (`StopReason::TickBudget`).
    pub tick_budget: u64,
    /// Max applied commands before the loop stops (`StopReason::EventBudget`).
    pub event_budget: u64,
}

impl ScenarioFile {
    /// The file's self-check — version pin + caps. Field-path
    /// validity is NOT checked here (a bad path is a failed goal
    /// with `observed: None`, matching the WS `unknown_fields`
    /// rule), but malformed structure is caught eagerly.
    pub fn validate(&self) -> Result<(), EvalError> {
        if self.schema_version != SCENARIO_SCHEMA_VERSION {
            return Err(EvalError::Invalid(format!(
                "schema_version {} != {SCENARIO_SCHEMA_VERSION}",
                self.schema_version
            )));
        }
        if self.name.is_empty() || self.name.len() > MAX_NAME {
            return Err(EvalError::Invalid(format!(
                "name must be 1-{MAX_NAME} chars"
            )));
        }
        if self.goals.len() > MAX_GOALS {
            return Err(EvalError::Invalid(format!(
                "goals {} > {MAX_GOALS}",
                self.goals.len()
            )));
        }
        if self.setup.len() > MAX_SETUP {
            return Err(EvalError::Invalid(format!(
                "setup {} > {MAX_SETUP}",
                self.setup.len()
            )));
        }
        match &self.agent {
            AgentSpec::Schedule(steps) if steps.len() > MAX_SCHEDULE_STEPS => {
                return Err(EvalError::Invalid(format!(
                    "schedule {} > {MAX_SCHEDULE_STEPS}",
                    steps.len()
                )));
            }
            AgentSpec::Log(steps) => {
                if steps.len() > MAX_LOG_STEPS {
                    return Err(EvalError::Invalid(format!(
                        "log {} > {MAX_LOG_STEPS}",
                        steps.len()
                    )));
                }
                let mut last = 0_u64;
                for s in steps {
                    if s.tick <= last {
                        return Err(EvalError::Invalid(
                            "log ticks must be strictly ascending".into(),
                        ));
                    }
                    last = s.tick;
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Build the world (`world` + `setup`) — the recipe-frozen half
    /// of the run.
    fn build_world(&self, fixtures_root: &Path) -> Result<World, EvalError> {
        let mut world = match &self.world {
            WorldSpec::Empty => World::new(),
            WorldSpec::Fixture { name } => {
                let recipe = crate::scenarios::find(name)
                    .ok_or_else(|| EvalError::Setup(format!("unknown fixture recipe {name:?}")))?;
                let dir = fixtures_root.join(name);
                (recipe.initial)(&dir)
                    .map_err(|e: ReplayError| EvalError::Setup(format!("fixture {name:?}: {e}")))?
            }
        };
        let mut pending: Vec<SimCommand> = Vec::new();
        for spec in &self.setup {
            match spec {
                SetupSpec::Command(c) => pending.push((**c).clone()),
                // PadLaunch drains its own boundary inside the recipe
                // (spawn at `clock+1`) — flush anything queued first
                // so ordering stays the file's.
                SetupSpec::PadLaunch => {
                    drain_pending(&mut world, &mut pending, &self.name)?;
                    crate::scenarios::spawn_pad(&mut world)
                        .map_err(|e| EvalError::Setup(format!("pad launch: {e}")))?;
                }
            }
        }
        // The `Script` source loads its program here — `Ui`-stamped,
        // after the user setup (see the `AgentSpec::Script` doc for
        // the lease reasoning).
        if let AgentSpec::Script { vessel, source } = &self.agent {
            pending.push(SimCommand::LoadScript {
                vessel: csp_sim::VesselId(*vessel),
                source: source.clone(),
            });
        }
        drain_pending(&mut world, &mut pending, &self.name)?;
        Ok(world)
    }
}

/// Enqueue `(Ui, 0)` + drain one boundary — the setup rule. A
/// rejected setup command is a scenario-authoring bug, surfaced as
/// `EvalError::Setup` (never silently dropped: a spawn that failed
/// mid-file would otherwise present as a mysterious goal miss).
fn drain_pending(
    world: &mut World,
    pending: &mut Vec<SimCommand>,
    scenario: &str,
) -> Result<(), EvalError> {
    if pending.is_empty() {
        return Ok(());
    }
    for c in pending.drain(..) {
        world.enqueue(SourceClass::Ui, 0, c);
    }
    let t = world.clock().ticks();
    let report = world.step_frame(t + 1)?;
    if !report.rejected.is_empty() {
        let msgs: Vec<String> = report
            .rejected
            .iter()
            .map(|r| format!("{}", r.error))
            .collect();
        return Err(EvalError::Setup(format!(
            "{scenario}: setup command rejected: {}",
            msgs.join("; ")
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The AgentSpec -> Agent adapters — each source variant is a `dyn Agent`.
// ---------------------------------------------------------------------------

/// `Schedule` — pop the next step per boundary; `Done` when empty.
struct ScheduleAgent {
    steps: std::collections::VecDeque<Vec<SimCommand>>,
}

impl Agent for ScheduleAgent {
    fn act(&mut self, _snap: &TelemetrySnapshot) -> AgentReply {
        match self.steps.pop_front() {
            Some(commands) => AgentReply::Act(commands),
            None => AgentReply::Done,
        }
    }
}

/// `Script`'s loop-side placeholder — the script already ticks
/// in-sim (loaded during `build_world`); this agent `Wait`s so the
/// loop stays alive for `stop_when`/`tick_budget`.
struct WaitAgent;

impl Agent for WaitAgent {
    fn act(&mut self, _snap: &TelemetrySnapshot) -> AgentReply {
        AgentReply::Wait
    }
}

/// `Log` — `AdvanceTo(tick-1)` hops between logged ticks, then the
/// logged commands on their drain tick. A step behind the clock is
/// `Fail` — the log cannot re-enter the past (§2.3).
struct LogAgent {
    steps: std::collections::VecDeque<LoggedStep>,
}

impl Agent for LogAgent {
    fn act(&mut self, snap: &TelemetrySnapshot) -> AgentReply {
        let next_tick = snap.tick.saturating_add(1);
        let Some(step) = self.steps.front() else {
            return AgentReply::Done;
        };
        match next_tick.cmp(&step.tick) {
            std::cmp::Ordering::Equal => {
                // `front()` held Some, so the pop cannot miss.
                let Some(step) = self.steps.pop_front() else {
                    return AgentReply::Fail("log queue lost a step".into());
                };
                AgentReply::Act(vec![step.command])
            }
            std::cmp::Ordering::Less => AgentReply::Act(vec![SimCommand::AdvanceTo(step.tick - 1)]),
            std::cmp::Ordering::Greater => AgentReply::Fail(format!(
                "log step at tick {} is behind the clock ({next_tick} next)",
                step.tick
            )),
        }
    }
}

/// The runner: build the world, bind the agent, step the loop under
/// the budgets, evaluate `goals` on the final snapshot.
///
/// `fixtures_root` is the directory the `Fixture{name}` worlds live
/// under (`data/test-fixtures` in this repo — a CLI flag on
/// `csp-eval`).
pub fn run_scenario(file: &ScenarioFile, fixtures_root: &Path) -> Result<EvalReport, EvalError> {
    run_scenario_full(file, fixtures_root).map(|(report, _)| report)
}

/// [`run_scenario`] plus the lockstep transcript — the "agent runs
/// are reproducible only as logs" artifact `csp-eval --transcript`
/// writes.
pub fn run_scenario_full(
    file: &ScenarioFile,
    fixtures_root: &Path,
) -> Result<(EvalReport, Vec<crate::agent::TranscriptEntry>), EvalError> {
    let world = file.build_world(fixtures_root)?;
    let funds_start = world.funds_micros() as i64;
    let mut run = Lockstep::new(world);

    // The command source -> registered agent (single-agent runs are
    // the format's v1; the lockstep's multi-agent contract is
    // exercised by agent.rs's own tests).
    let agent: Box<dyn Agent> = match &file.agent {
        AgentSpec::Idle => Box::new(ScheduleAgent {
            steps: std::collections::VecDeque::new(),
        }),
        AgentSpec::Schedule(steps) => Box::new(ScheduleAgent {
            steps: steps.clone().into(),
        }),
        AgentSpec::Program { name, args } => crate::eval::program(name, args)
            .ok_or_else(|| EvalError::Setup(format!("unknown agent program {name:?}")))?,
        // Loaded Ui-stamped in build_world — the loop just waits on
        // the in-sim agent (see the `Script` doc).
        AgentSpec::Script { .. } => Box::new(WaitAgent),
        AgentSpec::Log(steps) => Box::new(LogAgent {
            steps: steps.clone().into(),
        }),
    };
    // `tick_timeout`: only genuinely-async drivers set one — every
    // format source is synchronous, so `None` (never drops).
    run.add_agent(&file.name, None, agent);

    let mut stop = StopReason::AgentsDone;
    // `settle_ticks` countdown — `Some(n)` once `stop_when` has held;
    // the loop stops when it reaches 0 (the predicate itself is only
    // sampled before it first trips).
    let mut settling: Option<u64> = None;
    loop {
        if settling.is_none() {
            let snap = run.snapshot();
            if let Some(pred) = &file.stop_when
                && pred.evaluate(&snap).1
            {
                settling = Some(file.settle_ticks);
                if file.settle_ticks == 0 {
                    stop = StopReason::StopPredicate;
                    break;
                }
            }
        } else if let Some(0) = settling {
            stop = StopReason::StopPredicate;
            break;
        }
        if settling.is_none() && run.all_done() {
            break;
        }
        if run.elapsed_ticks() >= file.tick_budget {
            stop = StopReason::TickBudget;
            break;
        }
        run.step()?;
        if let Some(n) = settling.as_mut() {
            *n = n.saturating_sub(1);
        }
        if run.applied() > file.event_budget {
            stop = StopReason::EventBudget;
            break;
        }
    }

    let final_snap = run.snapshot();
    let goals: Vec<GoalOutcome> = file
        .goals
        .iter()
        .map(|p| {
            let (observed, passed) = p.evaluate(&final_snap);
            GoalOutcome {
                predicate: p.clone(),
                observed,
                passed,
            }
        })
        .collect();
    let agent_faults = run.agent_faults();
    let hash = run.world().state_hash().map_err(EvalError::Sim)?;
    let pass = goals.iter().all(|g| g.passed) && agent_faults.is_empty();
    let transcript = run.transcript().to_vec();
    Ok((
        EvalReport {
            scenario: file.name.clone(),
            pass,
            stop_reason: stop,
            final_tick: run.world().clock().ticks(),
            final_hash: crate::replay::hex(&hash),
            score: EvalScore {
                dv_used_mps: run.dv_used_mps(),
                sim_time_s: run.elapsed_ticks() as f64 / TICK_HZ as f64,
                funds_spent_micros: run.world().funds_micros() as i64 - funds_start,
            },
            events_applied: run.applied(),
            goals,
            agent_faults,
        },
        transcript,
    ))
}
