//! M9d §2.12.1 — the lockstep agent loop.
//!
//! The eval harness's core contract, quoted from the plan: "**the sim
//! does not advance until every registered agent has returned for the
//! current tick; a per-agent tick-count timeout counts as a no-op.**"
//! [`Lockstep`] is the synchronous realization of that rule: each
//! boundary builds the published [`TelemetrySnapshot`], polls every
//! registered agent in registration order, enqueues their commands
//! stamped `SourceClass::External` under the agent's own client index
//! (the §2.12.3 contract — agents get the same one-tick latency and
//! the same determinism contract as UI input), then calls
//! [`World::step_frame`] exactly once.
//!
//! An agent answers [`Agent::act`] with:
//!
//! - [`AgentReply::Act`] — commands enqueued at this boundary;
//! - [`AgentReply::Wait`] — "still thinking": the plan's tick-count
//!   timeout, a no-op by definition (the boundary proceeds without the
//!   agent). `tick_timeout: Some(n)` drops the agent on the *n*-th
//!   consecutive `Wait` — its replies are ignored from then on and the
//!   transcript records the drop;
//! - [`AgentReply::Done`] — the agent withdraws; the run ends when
//!   *every* registered agent is done (or dropped).
//!
//! # Reproducibility (the honest scope, §2.12.1)
//!
//! "**logged runs are byte-reproducible; agent runs are reproducible
//! only as logs** — the sim is deterministic, the agent is not."
//! [`Lockstep::transcript`] is that log: every boundary's per-agent
//! outcome (commands verbatim / wait / drop / done) plus the drain's
//! applied and rejected counts — enough to reconstruct *what the
//! agent decided*, while the sim side replays through `csp-replay`'s
//! command log. A run driven by a deterministic [`Agent`] (the
//! scenario format's schedule/program sources) IS byte-reproducible —
//! the suite's `lockstep_determinism` test pins that.

use csp_core::{SimError, TICK_HZ};
use csp_server::telemetry::TelemetrySnapshot;
use csp_sim::{AdvanceReport, RejectedCommand, SimCommand, SourceClass, World};

/// What one agent returns for the current boundary — see the module
/// doc for the §2.12.1 semantics of each variant.
#[derive(Clone, Debug, PartialEq)]
pub enum AgentReply {
    /// Enqueue these commands at this boundary (empty = a no-op that
    /// still counts as "the agent returned").
    Act(Vec<SimCommand>),
    /// No return this boundary — counts as a no-op; the streak feeds
    /// the agent's `tick_timeout` drop rule.
    Wait,
    /// The agent withdraws from the run (its final state reached or
    /// its schedule exhausted).
    Done,
    /// The agent faulted — retired, the message surfaced in the
    /// report's `agent_faults` (a faulted run is a failed eval even
    /// when the goals incidentally hold).
    Fail(String),
}

/// A run's command source — one registered agent. `act` must be a
/// fast, synchronous decision: the loop calls it once per boundary on
/// the world-owning thread, so a blocking agent stalls the sim it is
/// meant to observe (for a real LLM driver the harness's contract is
/// that `act` returns `Wait` until the model's reply is in — the
/// world keeps moving, which is exactly why agent runs are only
/// log-reproducible).
pub trait Agent {
    /// Called once at registration — the agent's §2.12.3 client
    /// index, both the `External` `source_id` its commands stamp
    /// under and the `Lease::Agent(id)` its take-control must use.
    fn bind(&mut self, _client: u32) {}

    /// Decide this boundary's commands from the published snapshot —
    /// the same projection the WS stream ships, so an agent sees
    /// exactly what a dashboard would.
    fn act(&mut self, snapshot: &TelemetrySnapshot) -> AgentReply;
}

/// One agent's slot in the loop — name, client index, timeout state.
struct Slot {
    /// The transcript/report name.
    name: String,
    /// The §2.12.3 client index (registration order).
    client: u32,
    /// Consecutive `Wait` count at which the agent is dropped; `None`
    /// = never drops (the schedule/script sources never wait anyway —
    /// the cap exists for genuinely-async drivers). `Some(n)` drops on
    /// the n-th consecutive `Wait` — that boundary's transcript kind is
    /// `timeout`, not `wait`.
    tick_timeout: Option<u64>,
    agent: Box<dyn Agent>,
    /// Consecutive `Wait` streak — reset by `Act`/`Done`.
    waits: u64,
    /// `act` is no longer called — dropped (timeout) or done.
    retired: bool,
}

/// One boundary's record in the run log — the "reproducible as logs"
/// artifact. `commands` holds the agent's verbatim `SimCommand`s on
/// an `Act` boundary (they serialize for the JSON transcript).
#[derive(Clone, Debug, serde::Serialize)]
pub struct TranscriptEntry {
    /// The sim tick the boundary drained at (`clock + 1` at poll time).
    pub tick: u64,
    /// The agent's registration name.
    pub agent: String,
    /// What the agent returned: `act`, `wait`, `timeout` (the drop
    /// boundary), `done`, `fail`, or `rejected` (the drain-level entry
    /// carrying rejections no live agent entry owns).
    pub kind: &'static str,
    /// The agent's commands on an `act` entry — empty otherwise.
    pub commands: Vec<SimCommand>,
    /// This boundary's drain rejections attributed to the agent —
    /// `"Class:id dequeue#n: error"` lines per §2.2.1's
    /// "rejected on the originating channel". A rejected command left
    /// no state and no log entry; the transcript is the only place its
    /// refusal is recorded, so it is never silently dropped.
    pub rejected: Vec<String>,
}

/// The loop's end state for one [`Lockstep::run`] — reported into the
/// scenario verdict (the `stop_reason` line).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// Every registered agent returned `Done` (or timed out).
    AgentsDone,
    /// A `stop_when` predicate held on the post-drain snapshot.
    StopPredicate,
    /// `clock - start` reached the scenario's `tick_budget`.
    TickBudget,
    /// Applied commands reached the scenario's `event_budget`.
    EventBudget,
}

/// The running loop — a `World` plus its registered agents. Built by
/// [`Lockstep::new`]/[`Lockstep::add_agent`], driven one boundary at a
/// time by [`Lockstep::step`] or to completion by the scenario runner
/// (`crate::scenario::run_scenario`, which adds the budgets and goal
/// predicates the raw loop stays agnostic of).
pub struct Lockstep {
    world: World,
    agents: Vec<Slot>,
    transcript: Vec<TranscriptEntry>,
    /// The tick the run started at (`World::clock` at construction —
    /// budgets measure elapsed, not absolute).
    start_tick: u64,
    /// Total commands the drains applied across the run — the
    /// `event_budget` counter.
    applied: u64,
    /// The score's propulsive-Δv integrator: per boundary,
    /// `throttle · (Σ Thrust rated_n / mass) · Δt` over bubble-resident
    /// vessels — commanded Δv expenditure, NOT achieved orbital Δv
    /// (steering losses are intentionally inside it: an agent that
    /// burns off-axis pays the same propellant).
    dv_used_mps: f64,
    /// `AgentReply::Fail` messages collected this run — surfaced in
    /// the eval report.
    agent_faults: Vec<String>,
    /// Every drain rejection across the run as `"Class:id dequeue#n:
    /// error"` lines — the flat list the eval report carries (the
    /// transcript attributes each to its agent's entry).
    rejected_commands: Vec<String>,
}

impl Lockstep {
    /// A loop over `world`; agents join via [`Self::add_agent`].
    pub fn new(world: World) -> Self {
        let start_tick = world.clock().ticks();
        Self {
            world,
            agents: Vec::new(),
            transcript: Vec::new(),
            start_tick,
            applied: 0,
            dv_used_mps: 0.0,
            agent_faults: Vec::new(),
            rejected_commands: Vec::new(),
        }
    }

    /// Register `agent` under `name`; returns the client index it was
    /// bound with (registration order — the `ClientRegistry` rule).
    /// `tick_timeout: Some(n)` drops the agent on the `n`-th
    /// consecutive `Wait`; `None` never drops.
    pub fn add_agent(
        &mut self,
        name: impl Into<String>,
        tick_timeout: Option<u64>,
        mut agent: Box<dyn Agent>,
    ) -> u32 {
        let client = self.agents.len() as u32;
        agent.bind(client);
        self.agents.push(Slot {
            name: name.into(),
            client,
            tick_timeout,
            agent,
            waits: 0,
            retired: false,
        });
        client
    }

    /// The world under the loop — read access for the runner's
    /// telemetry projections and goal predicates.
    pub fn world(&self) -> &World {
        &self.world
    }

    /// The run log so far — the "agent runs are reproducible only as
    /// logs" artifact (§2.12.1).
    pub fn transcript(&self) -> &[TranscriptEntry] {
        &self.transcript
    }

    /// Commands the drains applied since construction — the
    /// `event_budget` counter.
    pub fn applied(&self) -> u64 {
        self.applied
    }

    /// `AgentReply::Fail` messages collected so far (`"name: msg"`).
    pub fn agent_faults(&self) -> Vec<String> {
        self.agent_faults.clone()
    }

    /// Every drain rejection across the run — `"Class:id dequeue#n:
    /// error"` lines in drain order, the eval report's
    /// `rejected_commands` field. §2.2.1's rejections are *refusals*,
    /// not faults: a rejected command applied nothing, so these ride
    /// the report rather than failing the run.
    pub fn rejected_commands(&self) -> &[String] {
        &self.rejected_commands
    }

    /// The score's commanded-Δv integral (m/s) — see the field doc.
    pub fn dv_used_mps(&self) -> f64 {
        self.dv_used_mps
    }

    /// `true` when every registered agent is done or dropped — the
    /// loop's natural end (an agent-less loop is done immediately).
    pub fn all_done(&self) -> bool {
        self.agents.iter().all(|s| s.retired)
    }

    /// Ticks elapsed since construction — the `tick_budget` measure.
    pub fn elapsed_ticks(&self) -> u64 {
        self.world.clock().ticks().saturating_sub(self.start_tick)
    }

    /// The observation the agents poll — the same `TelemetrySnapshot`
    /// shape §2.12.2 publishes (fresh per boundary, never cached).
    pub fn snapshot(&self) -> TelemetrySnapshot {
        TelemetrySnapshot::from_world(&self.world)
    }

    /// One lockstep boundary: snapshot → poll every live agent →
    /// enqueue → `step_frame` → score/apply bookkeeping. Returns the
    /// boundary's merged drain report (rejections ride it, §2.2.1 —
    /// they are also attributed onto the transcript and the run's
    /// [`Self::rejected_commands`] list, never dropped).
    ///
    /// The plan's lockstep rule *is* the synchronous call order: the
    /// world cannot advance between snapshot and drain because the
    /// agents have all returned before `step_frame` runs.
    ///
    /// §2.6.5's back-pressure can split the boundary's advance into
    /// partial frames; the agents are NOT re-polled mid-advance (they
    /// already answered for the boundary) — the report's
    /// `effective_target` is re-issued until the frame lands, covering
    /// a truncated `AdvanceTo`. A continuation frame that makes zero
    /// progress is a sim/harness bug (`Fatal`), not a normal partial.
    pub fn step(&mut self) -> Result<AdvanceReport, SimError> {
        let tick = self.world.clock().ticks().saturating_add(1);
        let snap = TelemetrySnapshot::from_world(&self.world);
        let mut pending: Vec<(u32, SimCommand)> = Vec::new();
        // `(client, transcript index)` for this boundary's `act`
        // entries — the drain's rejections attribute back onto them.
        let mut act_entries: Vec<(u32, usize)> = Vec::new();
        for slot in &mut self.agents {
            if slot.retired {
                continue;
            }
            match slot.agent.act(&snap) {
                AgentReply::Act(commands) => {
                    slot.waits = 0;
                    for c in &commands {
                        pending.push((slot.client, c.clone()));
                    }
                    self.transcript.push(TranscriptEntry {
                        tick,
                        agent: slot.name.clone(),
                        kind: "act",
                        commands,
                        rejected: Vec::new(),
                    });
                    act_entries.push((slot.client, self.transcript.len() - 1));
                }
                AgentReply::Wait => {
                    slot.waits = slot.waits.saturating_add(1);
                    let dropped = slot.tick_timeout.is_some_and(|t| slot.waits >= t);
                    self.transcript.push(TranscriptEntry {
                        tick,
                        agent: slot.name.clone(),
                        kind: if dropped { "timeout" } else { "wait" },
                        commands: Vec::new(),
                        rejected: Vec::new(),
                    });
                    if dropped {
                        slot.retired = true;
                    }
                }
                AgentReply::Done => {
                    slot.retired = true;
                    self.transcript.push(TranscriptEntry {
                        tick,
                        agent: slot.name.clone(),
                        kind: "done",
                        commands: Vec::new(),
                        rejected: Vec::new(),
                    });
                }
                AgentReply::Fail(message) => {
                    slot.retired = true;
                    self.agent_faults.push(format!("{}: {message}", slot.name));
                    self.transcript.push(TranscriptEntry {
                        tick,
                        agent: slot.name.clone(),
                        kind: "fail",
                        commands: Vec::new(),
                        rejected: Vec::new(),
                    });
                }
            }
        }
        for (client, command) in pending {
            self.world.enqueue(SourceClass::External, client, command);
        }
        let mut report = self.world.step_frame(tick)?;
        while report.partial {
            let before = report.reached_tick;
            let next = self.world.step_frame(report.effective_target)?;
            // A partial is back-pressure, never a stall: it advanced
            // the clock, fired events, or applied/drained commands.
            // Zero activity on a continuation frame is a bug.
            let stalled = next.reached_tick <= before
                && next.events_processed == 0
                && next.commands_applied == 0
                && next.rejected.is_empty()
                && next.degraded.is_empty();
            merge_report(&mut report, next);
            if stalled {
                return Err(SimError::Fatal(
                    "lockstep: a partial advance made zero progress",
                ));
            }
        }
        self.applied = self.applied.saturating_add(report.commands_applied);
        self.accumulate_dv(tick - 1, report.reached_tick);
        self.record_rejections(tick, &act_entries, &report.rejected);
        Ok(report)
    }

    /// Attribute the boundary's drain rejections (§2.2.1's "rejected
    /// on the originating channel") onto the transcript: an
    /// `External`-class rejection lands on the acting agent's `act`
    /// entry; one from any other class (a script's verb, an
    /// autopilot's command — the lockstep's agents only ever stamp
    /// `External`) collects onto a drain-level `rejected` entry so the
    /// refusal is recorded rather than laundered. Every line also
    /// lands in [`Self::rejected_commands`].
    fn record_rejections(
        &mut self,
        tick: u64,
        act_entries: &[(u32, usize)],
        rejected: &[RejectedCommand],
    ) {
        let mut stray: Vec<String> = Vec::new();
        for r in rejected {
            let line = format!(
                "{:?}:{} dequeue#{}: {}",
                r.key.source_class, r.key.source_id, r.dequeue_index, r.error
            );
            self.rejected_commands.push(line.clone());
            let entry = if r.key.source_class == SourceClass::External {
                act_entries
                    .iter()
                    .find(|(client, _)| *client == r.key.source_id)
                    .map(|(_, idx)| *idx)
            } else {
                None
            };
            match entry {
                Some(idx) => self.transcript[idx].rejected.push(line),
                None => stray.push(line),
            }
        }
        if !stray.is_empty() {
            self.transcript.push(TranscriptEntry {
                tick,
                agent: "<drain>".into(),
                kind: "rejected",
                commands: Vec::new(),
                rejected: stray,
            });
        }
    }

    /// The score's Δv accumulator: `throttle · thrust/mass · Δt` per
    /// vessel over the boundary the report covers (`from` = the
    /// pre-drain clock, `to` = where it landed — `step_frame` can run
    /// past `clock+1` on an `AdvanceTo`). Reads the *post-drain*
    /// throttle (the commanded state for the span — the M2 control
    /// surface holds it absolute until re-commanded).
    fn accumulate_dv(&mut self, from: u64, to: u64) {
        let dt_s = to.saturating_sub(from) as f64 / TICK_HZ as f64;
        if dt_s <= 0.0 {
            return;
        }
        for vessel in self.world.vessels().values() {
            let Some(body) = vessel.body() else { continue };
            let throttle = body.throttle.clamp(0.0, 1.0);
            if throttle <= 0.0 || body.mass_kg <= 0.0 {
                continue;
            }
            // The weld already aggregated the part sites into body
            // frame — `craft` needn't be re-walked.
            let thrust_n: f64 = body
                .force_sites
                .iter()
                .filter(|s| s.kind == csp_phys::ForceSiteKind::Thrust)
                .map(|s| s.rated_n)
                .sum();
            self.dv_used_mps += throttle * (thrust_n / body.mass_kg) * dt_s;
        }
    }
}

/// Fold a §2.6.5 partial-continuation's report into `acc` — one
/// boundary's view across however many `step_frame`s the budgets split
/// it into: counts sum, the ordered channels concatenate in order, and
/// the terminal `reached_tick`/`effective_target`/`partial` come from
/// the last call.
fn merge_report(acc: &mut AdvanceReport, next: AdvanceReport) {
    acc.reached_tick = next.reached_tick;
    acc.commands_applied += next.commands_applied;
    acc.events_processed += next.events_processed;
    acc.fired_events.extend(next.fired_events);
    acc.rejected.extend(next.rejected);
    acc.degraded.extend(next.degraded);
    acc.diagnostics.extend(next.diagnostics);
    acc.partial = next.partial;
    acc.effective_target = next.effective_target;
    acc.script_log.extend(next.script_log);
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // test assertions
mod tests {
    use super::*;
    use serde_json::json;

    /// A fixed script of replies — the deterministic test driver.
    struct ScriptAgent {
        replies: std::collections::VecDeque<AgentReply>,
    }

    impl Agent for ScriptAgent {
        fn act(&mut self, _snap: &TelemetrySnapshot) -> AgentReply {
            self.replies.pop_front().unwrap_or(AgentReply::Done)
        }
    }

    fn scripted(replies: Vec<AgentReply>) -> Box<dyn Agent> {
        Box::new(ScriptAgent {
            replies: replies.into(),
        })
    }

    #[test]
    fn act_stamps_external_client_index() {
        let mut run = Lockstep::new(World::new());
        let client = run.add_agent(
            "a0",
            None,
            scripted(vec![AgentReply::Act(vec![SimCommand::SetWarp(4)])]),
        );
        assert_eq!(client, 0);
        run.step().unwrap();
        let (key, _) = run.world().command_log().last().expect("log entry");
        assert!(matches!(key.source_class, SourceClass::External));
        assert_eq!(key.source_id, 0);
    }

    /// `tick_timeout: Some(n)` drops on the *n*-th consecutive `Wait`
    /// — that boundary records `timeout`, not `wait`.
    #[test]
    fn wait_then_timeout_drops_the_agent() {
        let mut run = Lockstep::new(World::new());
        run.add_agent(
            "slow",
            Some(2),
            scripted(vec![
                AgentReply::Wait,
                AgentReply::Wait, // streak 2 = the timeout → dropped here
                AgentReply::Act(vec![SimCommand::SetWarp(9)]),
            ]),
        );
        run.step().unwrap();
        run.step().unwrap();
        let kinds: Vec<&str> = run.transcript().iter().map(|e| e.kind).collect();
        assert_eq!(kinds, ["wait", "timeout"]);
        // The dropped agent's later Act never landed — the rung stays
        // the fresh world's default (1×), never the dropped 9.
        run.step().unwrap();
        assert_eq!(run.world().warp_level(), 1);
        assert!(run.all_done());
    }

    #[test]
    fn two_agents_lockstep_same_boundary() {
        let mut run = Lockstep::new(World::new());
        run.add_agent(
            "first",
            None,
            scripted(vec![AgentReply::Act(vec![SimCommand::SetWarp(2)])]),
        );
        run.add_agent(
            "second",
            None,
            scripted(vec![AgentReply::Act(vec![SimCommand::SetWarp(8)])]),
        );
        run.step().unwrap();
        // Both commands drained at the same boundary, in registration
        // order (client 0 then 1) — the §2.2.1 ordering rule.
        let log = run.world().command_log();
        let ids: Vec<u32> = log.iter().map(|(k, _)| k.source_id).collect();
        assert_eq!(ids, [0, 1]);
        // Neither retired on Act — `Done` arrives on the next poll.
        assert!(!run.all_done());
        run.step().unwrap();
        assert!(run.all_done());
    }

    /// §2.6.5 back-pressure inside the lockstep: an agent's
    /// `AdvanceTo` past `MAX_TICKS_PER_FRAME` partials — `step`
    /// re-issues the effective target until the frame lands instead
    /// of dropping the tail, and the merged report reflects it.
    #[test]
    fn step_resumes_partial_advances() {
        let mut run = Lockstep::new(World::new());
        run.add_agent(
            "hop",
            None,
            scripted(vec![AgentReply::Act(vec![SimCommand::AdvanceTo(
                csp_sim::MAX_TICKS_PER_FRAME + 100,
            )])]),
        );
        let report = run.step().unwrap();
        assert!(!report.partial);
        assert_eq!(
            run.world().clock().ticks(),
            csp_sim::MAX_TICKS_PER_FRAME + 100
        );
        // The boundary merged every partial's applied count.
        assert_eq!(run.applied(), 1);
    }

    /// A rejected agent command lands on the `act` entry's `rejected`
    /// list and the run-level feed — §2.2.1's refusal is recorded,
    /// never silently dropped.
    #[test]
    fn rejected_commands_ride_the_transcript_and_report() {
        let mut run = Lockstep::new(World::new());
        run.add_agent(
            "bad",
            None,
            scripted(vec![AgentReply::Act(vec![SimCommand::SetThrottle {
                vessel: csp_sim::VesselId(99),
                throttle: 0.5,
            }])]),
        );
        run.step().unwrap();
        assert_eq!(run.rejected_commands().len(), 1);
        assert!(run.rejected_commands()[0].contains("External:0"));
        let entry = &run.transcript()[0];
        assert_eq!(entry.kind, "act");
        assert_eq!(entry.rejected.len(), 1);
        // A rejected command left no log entry.
        assert!(run.world().command_log().is_empty());
    }

    #[test]
    fn transcript_serializes_for_the_run_log() {
        let mut run = Lockstep::new(World::new());
        run.add_agent(
            "a",
            None,
            scripted(vec![AgentReply::Act(vec![SimCommand::SetWarp(4)])]),
        );
        run.step().unwrap();
        let v = serde_json::to_value(run.transcript()).expect("json");
        assert_eq!(v[0]["kind"], json!("act"));
        assert!(v[0]["commands"][0].is_object());
    }
}
