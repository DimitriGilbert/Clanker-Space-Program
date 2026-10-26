# PRD-21 Review — csp-bench: replay / perf / eval / scenarios

**Scope:** `crates/csp-bench/src/{replay,perf,scenarios,scenario,agent,economy,contracts,eval,lib}.rs`, `build.rs`, `bin/{csp-eval,csp-mcp,csp-perf,csp-replay}.rs`, `tests/eval_suite.rs`, plus the committed artifacts it runs (`data/eval-scenarios/*.ron`, `data/test-fixtures/*`).
**PRD reference:** PLAN §8.5 (e2e layer), §8.2 gates table, §8.3 fixture rules, §7.6 replay artifact, §2.12.1 eval contract, §3.2/§3.5/§3.6 (science/economy/contracts loops).

## Verdict

The harness is **mechanically excellent and honest about itself** — the replay artifact, fixture freeze, doctored-artifact checks, goal-less-scenario refusal, empty-suite refusal, and transcript attribution are all well-built and well-pinned. But the *content* the harness drives covers only the flight slice of the game. Of ~45 `SimCommand` variants, the committed goldens + eval suite exercise ~10. **The eval suite cannot express the player-shaped loop at all today**: the only fixture world (`flight-launch`) has no tech tree, no parts catalog, no experiments, no contracts, no colonies — so no `scenario.ron` can even *author* a science/contract/colony mission. §8.5's sim-e2e row names "launch→orbit→land, contract lifecycle, colony supply loop, agent completing a mission"; only the first is committed, and the "contract lifecycle" that does exist (contracts.rs) satisfies the contract by teleport-spawning a vessel into the required orbit and injecting tech facts through snapshot surgery — it proves the settlement machinery, not the loop.

This is exactly the shape the PRD-16 reviewer predicted: everything drives `World::enqueue`/`step_frame` directly, so the suite is blind to whether a player (or even an agent on the real tool surface) can *reach* the states it asserts.

---

## Findings

### F1 — The eval fixture structurally cannot host the program loop

**Severity:** High
**Location:** `crates/csp-bench/src/scenarios.rs:145-248` (`flight_initial`), `crates/csp-bench/src/scenario.rs:448-458` (`WorldSpec::Fixture` resolution), `data/test-fixtures/flight-launch/` (dir listing)
**Problem:** `WorldSpec::Fixture{name}` resolves only through `scenarios::find` (the golden recipe registry), and the single non-trivial fixture, `flight-launch`, builds a world that never calls `set_tech_tree` or `set_parts` and whose fixture dir contains no `experiments/`, `contracts/`, `recipes/`, `colonies.ron`, or `techtree/` (each `load_*` maps a missing dir to an empty set — `crates/csp-data/src/load.rs:634-647, 705-716, 570-580`).
**Evidence:** `find data/test-fixtures/flight-launch` → `balance.ron, sites.ron, system.ron, solen/, terrin/` only. `flight_initial` wires `set_experiments`/`set_contracts`/`set_recipes`/`found_authored_colonies` against dirs that don't exist (all resolve to empty) and omits `set_tech_tree`/`set_parts` entirely. Grep: `RunExperiment|ResearchTech|AcceptContract|FoundColony|TransferColonyResource` appear **nowhere** under `data/eval-scenarios/` or as committed `Log`/`Schedule` content.
**Impact:** §8.5's promised "contract lifecycle" and "colony supply loop" e2e coverage is not merely unwritten — it is *unauthorable* in the current format, because no fixture world carries the data those commands need. The eval suite's honest-looking green (5/5 scenarios pass) silently communicates "the game works" while the entire program layer (science → research → contracts → colonies → crew) is unexercised end-to-end. This is the harness-level equivalent of the PRD-16 reachability gap: nothing fails because nothing is asked.
**Suggestion:** Add a `WorldSpec::Shipping` (or a registered `full-data` recipe) that builds the world from the shipping `data/` set — the construction already exists as `contracts.rs::contract_world` (contracts.rs:69-112, currently test-private). Then author scenario files for: experiment→transmit→research→tech unlock; accept-contract→fly→complete→payout; colony resupply (`TransferColonyResource`/`JoinColonyInventory`→`ManufactureCraft`).

### F2 — `AgentSpec::Log` cannot represent the project's own simplest golden

**Severity:** Medium
**Location:** `crates/csp-bench/src/scenario.rs:424-439` (`validate`), `:94-100` (`LoggedStep`), `:581-608` (`LogAgent`)
**Problem:** `LoggedStep` carries exactly **one** command and `validate` enforces strictly-ascending ticks (`s.tick <= last` rejects equal ticks — and, since `last` starts at 0, also rejects a first step at tick 0).
**Evidence:** The committed `trivial` golden's log is `AdvanceTo` stamped at **tick 0**, then a **same-tick pair** (`Ui:0 SetWarp(4)` + `External:3 SetWarp(16)`) at tick 500 (`scenarios.rs:77-86`; asserted at `replay.rs:1813-1821`). Neither is expressible as `AgentSpec::Log`: tick 0 fails validation, and two commands at one tick require two `LoggedStep`s that must be strictly ascending. The `Log` source — which the §2.12.1 schema bills as `command_source: agent|script|log` and §8.5 calls the "command log source" — cannot re-issue the codebase's own recorded artifacts.
**Impact:** Same-tick ordering is a §2.2.1 semantic (class order, dequeue-stamp) that the eval format cannot probe; any "re-drive a recorded mission through the lockstep" workflow is blocked at validation. `log-replay.ron` works only because it was authored to avoid both cases.
**Suggestion:** Allow `tick == 0` for the first step and non-decreasing ticks with per-step command *lists* (`tick, commands: [SimCommand]`), matching §7.6's same-tick group semantics.

### F3 — Contract-lifecycle coverage bypasses the gameplay path it claims to prove

**Severity:** Medium
**Location:** `crates/csp-bench/src/contracts.rs:152-249` (`contract_run`), `:121-145` (`mark_facts`)
**Problem:** The probe accepts a contract then satisfies it by `SpawnVessel`-ing an elements-only vessel *directly into the drawn orbit band* (contracts.rs:208-225), and meets prereqs by mutating `snap.tech.researched`/`facts.completed` through snapshot surgery (contracts.rs:121-145). Both are documented as deliberate ("the probe's subject is the contract generator + lifecycle, not the research grind" / "put a matching vessel on rails").
**Evidence:** `SimCommand::SpawnVessel` at contracts.rs:215-222; the satellite band is read from `contract.params` and teleported into. No launch, no flight, no `RunExperiment`/`ResearchTech` on the path.
**Impact:** The lifecycle machinery (board generation, accept, advance, event-driven completion, payout, fact record, same-process determinism) is genuinely covered — but a *player-shaped* failure like "the drawn band is unreachable from the pad," "no part combination satisfies the prereq chain," or "the survey chain can't bootstrap the satellite contract" is invisible by construction. Also note the probe is **not a committed golden**: it compares two in-process runs, so contract-code *drift* reddens nothing (`expected-hash.txt` exists only for `trivial`/`flight-launch`, and the flight fixture's contract table is empty — `set_contracts` loads a missing dir).
**Suggestion:** Keep the mechanics probe, but add one honest-path variant: accept the survey contract, fly the satisfying trajectory (or `Log` a recorded one once F2 is fixed), assert payout — and consider recording the contract run as a third golden so §3.6 logic has committed-hash protection.

### F4 — Science loop has zero coverage in the entire crate

**Severity:** High (as a gap; the loop is the game's core progression)
**Location:** whole crate — `SimCommand::{RunExperiment, TransmitData, RecoverData, StartResearch, StopResearch, ResearchTech, ResearchUpgradeLevel}` (`crates/csp-sim/src/lib.rs:803-880`) appear in **no** csp-bench scenario, probe, or test.
**Evidence:** `grep -rn "RunExperiment|ResearchTech|TransmitData|RecoverData" crates/csp-bench/` → only doc comments. The flight fixture installs `ScienceConfig` and an empty experiment table (scenarios.rs:184-193) over a missing `experiments/` dir; the authored `flight_craft()` carries `experiments: Vec::new()` (scenarios.rs:262).
**Impact:** §3.2's loop (experiment aboard craft → collect → transmit/recover → science → research → tech → new parts) — the thing a player actually *does* — has no end-to-end witness anywhere in the harness. Compounds with PRD-23's content gap (1 experiment, no parachute): even a sim-side loop test would currently expose that recovery has no content path, which is precisely the kind of finding this suite exists to produce.
**Suggestion:** A `science-loop` scenario on a full-data world (per F1): craft with an `Experiment` module → `RunExperiment` → `RecoverData`/`TransmitData` → `ResearchTech` → assert `tech.researched` and `science`/`funds` deltas via goal predicates.

### F5 — Colony "supply loop" (§8.5's words) is absent; colony coverage is production-only

**Severity:** Medium
**Location:** `crates/csp-bench/src/economy.rs:201-311` (`colony_production_run`), `crates/csp-bench/src/perf.rs:899-940` (`colony_warp_run`)
**Problem:** Colony coverage exists but stops at "drill produces into store; `AbandonColony` salvages." Nothing exercises the *supply* direction — `TransferColonyResource`, `TransferColonyCrew`, `JoinColonyInventory`/`LeaveColonyInventory`, `ManufactureCraft`, `AssignCrewToColony`, `EvacuateColony`, `SetLifeSupportEnabled`, `DamageColonyModule`, `ScheduleColonyInspection` — nor any vessel↔colony interaction.
**Evidence:** Grep — those command names appear only in csp-sim definitions, never in csp-bench. `perf_colony_def` (perf.rs:852-893) builds a drill/converter colony purely as a warp event source.
**Impact:** §8.5's "colony supply loop" — the resupply mission a player flies — is untested; the M8 colony stack's player-facing half (stock a colony, crew it, keep it alive, manufacture from it) has no witness. Combined with F1, a `FoundColony`-via-UI or resupply-by-flight bug is invisible.
**Suggestion:** A colony-resupply scenario: found/seed a colony, `TransferColonyResource` from a landed/docked vessel, `SetLifeSupportEnabled` with crew assigned, assert store levels + no starvation events under `stop_when`/`goals`.

### F6 — §8.2's committed golden subset is incomplete; nightly-only scenarios don't exist

**Severity:** Medium
**Location:** `crates/csp-bench/src/scenarios.rs:50-63` (`SCENARIOS`), PLAN §8.2 gates table (PLAN.md:1758, 1768), §2.6.5 (PLAN.md:571)
**Problem:** §8.2's per-commit golden subset names **ascent, reentry, landing+settle, docking merge, ×10⁵ warp jump, ×1-vs-×10⁵ equivalence**. `SCENARIOS` contains two recipes: `trivial` (warp/scalar ordering only) and `flight-launch` (launch→orbit→reentry→land→recover). Missing: docking merge (`Undock`/capture path — `Undock` at csp-sim/lib.rs:747 exists but is never driven), a ×10⁵ warp golden, a ×1-vs-×10⁵ equivalence witness, and the §2.6.5 "pathological event density" nightly scenario (20 colonies × 6 converters + 400 debris).
**Evidence:** `SCENARIOS` pinned at 2 (replay.rs:1257-1264). `perf.rs::colony_warp_run` is the 20-colony scene but it is a *measurement* (ms/events), not a golden — its hashes are never pinned. The nightly rows in the §8.2 table have no corresponding CI job or recipe.
**Impact:** Docking — a whole subsystems block (capture windows, soft joints, `Undock`) — has no golden-level determinism pin; warp-ladder equivalence (the plan's "single most important property test" pairing) has no scenario-level witness; the pathological-density case that motivated §2.6.5's back-pressure is unmeasured by any committed artifact.
**Suggestion:** Register `docking-merge` and `warp-ladder` recipes (both are sim-mechanics, cheap to author); promote `colony_warp_run` into a `colony-century`/`pathological-density` golden for the nightly tier.

### F7 — A run whose commands were all rejected can still pass

**Severity:** Low-Medium
**Location:** `crates/csp-bench/src/scenario.rs:708` (`pass` computation), `:274-280` (`rejected_commands` doc)
**Problem:** `pass = goals.all(passed) && agent_faults.is_empty()`; `rejected_commands` is explicitly informational. A scenario whose goals don't depend on the rejected commands (e.g., `tick >= N`, `warp_level` untouched by a rejected `SetWarp`) reports pass while every issued command was refused.
**Evidence:** `lease-launch.ron` is safe because its goals read `throttle`/`altitude` — but the *format* doesn't require that coupling; `log-replay.ron`'s goals (`tick ≥ 512`, `warp_level == 4`) would still pass if the `SetWarp` had been rejected, since a rejected `AdvanceTo` … actually `tick ≥ 512` would fail there — but `SetWarp(4)` rejection on a goal-free-of-warp scenario would pass. The transcript records rejections faithfully; the verdict ignores them.
**Impact:** An agent that nominally "did everything" but was refused at the gate (wrong lease, wrong source class) can produce a green eval — the same class of silent-failure the suite exists to prevent. Reachability-adjacent: a rejected `TakeControl` doesn't fail the run unless a goal reads the lease.
**Suggestion:** Make `rejected_commands.is_empty()` a pass condition, or add a scenario-level `allow_rejections: bool` (default false) so probes that intentionally exercise rejection opt in.

### F8 — Suite cardinality is not pinned; scenarios can silently shrink

**Severity:** Low
**Location:** `crates/csp-bench/tests/eval_suite.rs:34-76` (`committed_eval_suite_all_pass`), `crates/csp-bench/src/eval.rs:214-237` (`run_suite`)
**Problem:** The test asserts the suite is non-empty, every entry passes, and `ascent-mission.ron` is present — but the *count* is unpinned. Contrast `every_registered_scenario_has_a_verified_golden` (replay.rs:1257-1264) which pins `SCENARIOS.len() == 2` precisely to prevent silent shrinkage.
**Evidence:** Deleting `lease-launch.ron` or `script-ascent.ron` leaves the test green.
**Impact:** An accidental `git rm` / bad rebase quietly removes coverage (e.g., the only `Script`-source scenario). Same failure class the golden pin was written against.
**Suggestion:** Assert `entries.len() == 5` (or iterate a `EXPECTED: &[&str]` name list) so membership, not just pass/fail, is pinned.

### F9 — `event_budget` trips after a boundary lands all its commands; `settle_ticks` counts boundaries

**Severity:** Low
**Location:** `crates/csp-bench/src/scenario.rs:686-689`, `:655-685` (settle countdown)
**Problem:** (a) `run.applied() > file.event_budget` is evaluated *after* `run.step()` — a single boundary's `Act` batch applies in full regardless of the budget, so the "max applied commands" bound is soft by up to one boundary's worth of commands (unbounded within one `Act`). (b) `settle_ticks` is decremented once per `run.step()` — i.e., per *boundary*, not per sim tick; a boundary holding a large `AdvanceTo` settles thousands of ticks in one decrement. The doc calls them boundaries (scenario.rs:367-372) so behavior is documented, but the name invites a tick interpretation.
**Evidence:** `event_budget_bounds_the_run` (eval_suite.rs:258-275) confirms post-step checking (`events_applied > 0` after budget 1).
**Impact:** Minor semantic looseness; a malicious/degenerate single boundary can blow the event budget arbitrarily. The name mismatch will produce mis-authored scenarios (a user writing `settle_ticks: 64` expecting 64 sim ticks gets 64 *drains*).
**Suggestion:** Document the boundary granularity in the `event_budget` field doc ("bounds boundaries, not commands — one boundary's batch always lands whole"); rename `settle_ticks` → `settle_boundaries` or decrement by elapsed ticks.

### F10 — `csp-mcp --fixture` is hardwired to one recipe

**Severity:** Low
**Location:** `crates/csp-bench/src/bin/csp-mcp.rs:145-149` (`build_fixture_world`)
**Problem:** `find("flight-launch")` — the flag takes a *dir* but the recipe is fixed, so the stdio host can only ever boot the flight world.
**Evidence:** `mcp_mission_agent_flies_ascent` (eval_suite.rs:398) is the only consumer and uses that recipe; fine today, but combined with F1 it means the MCP e2e surface can never host a contract/science world without code change.
**Impact:** Compounds F1: even if a full-data fixture dir were authored, `csp-mcp --fixture` would still build flight-launch's `initial` over it. The §2.12.1 "eval harness spawns csp-mcp" contract inherits the flight-only ceiling.
**Suggestion:** Take a recipe name (`--scenario <name>` or infer from dir) and resolve via `scenarios::find`.

### F11 — build.rs silently degrades the fingerprint on lockfile failure

**Severity:** Low
**Location:** `crates/csp-bench/build.rs:56` (`read_to_string(&lock).unwrap_or_default()`)
**Problem:** An unreadable/missing `Cargo.lock` yields empty text → every tracked dep resolves `None` → `fingerprint()` reports the deps as "absent" rather than failing. Advisory-only (fingerprint drift never gates), but a misdiagnosed fingerprint is worse than a loud failure in the tool whose whole job is diagnosability.
**Suggestion:** `panic!`/emit `cargo:warning=` when the lock is unreadable — the build script's output correctness is the feature.

### F12 — Economy harness solves its own input to sit inside the asserted band

**Severity:** Low (honest by its docs, but worth recording what it does *not* prove)
**Location:** `crates/csp-bench/src/economy.rs:88-97, 371-400`
**Problem:** `staffed_base_run` *solves* `salary_per_s` so the drain lands at `drain_fraction` of post-hire funds, then the test asserts the 2–5% band. This proves the drain machinery charges `salary × period` exactly and bounded — it cannot detect that *authored* balance values produce an out-of-band economy, since the input is reverse-engineered into the band by construction.
**Evidence:** `drain_fraction: 0.022` at economy.rs:375 feeds the solve at :92-97; the assertion then checks the ratio lands in `[0.02, 0.05]` — which it must, since the salary was derived from it. The real check is that the charge lands *at all* and deterministically, which is honestly what the code claims.
**Impact:** A balance.ron regression (e.g., `salary_period_days` drifting, or authored salaries not existing) that would put the *shipping* economy outside the §3.5 band is not caught — there are no authored crew salaries in `data/` to check against (content gap, PRD-23's territory, but the harness cannot bridge it).
**Suggestion:** A second probe that hires with *authored* salary data (when crew salary tables ship) and asserts the band — a content-in-band check rather than a solved-input check.

---

## What's genuinely good (worth preserving)

- **Replay harness is the strongest code in the cluster.** `record` self-checks before writing (replay.rs:687-703), refuses vacuous logs, `verify_golden` enforces fixture freeze + embedded-vs-sidecar fingerprint agreement (980-1010), `write_golden` anti-laundering lives in the library so every writer inherits it (1055-1082), and the test suite doctored-artifact coverage (stamp-behind-clock, same-tick reorder vs payload swap → StampMismatch vs HashMismatch split, trailing bytes, symlink/non-UTF8 refusal) is thorough.
- **Honest scope claims.** `agent.rs` docs state "agent runs are reproducible only as logs" (§2.12.1 verbatim) and the determinism test asserts byte-identical reruns *and* that the run wasn't degenerate (`final_tick > 1000`, `dv > 1000`, eval_suite.rs:91-94).
- **Anti-vacuous guards everywhere**: goal-less scenarios rejected (scenario.rs:396-404), empty suite dir is an error (eval.rs:223-228), zero-progress partial frames are `Fatal` (agent.rs:348-358, replay.rs:918-938).
- **Perf log honesty**: hardware-gated rows are recorded `HardwareGated`, never faked (perf.rs:341-350); measurable-row coverage is pinned by test (1117-1135); `regression_diff` flags unmeasured measurable rows and Pass→Fail crossings under the noise band (440-485); unreadable baseline is exit 2, not silent green (csp-perf.rs:131-138).
- **`mcp_mission_agent_flies_ascent`** (eval_suite.rs:397-503) is the closest thing to a player-shaped test: it drives the real stdio MCP surface (spawn → lease-rejection asserted → take_control → telemetry-feedback ascent → session.snapshot), proving the *wire path* an external agent uses — though it only covers the flight-tool slice.

## Summary vs §8.5's e2e table

| §8.5 tier | Status |
|---|---|
| Sim e2e — launch→orbit→land | Covered by `flight-launch` **golden**; eval `ascent-mission` stops at apoapsis (never deorbits/lands) |
| Sim e2e — contract lifecycle | Mechanics probe only (teleport-satisfy + snapshot-surgery prereqs); not a golden, not a scenario |
| Sim e2e — colony supply loop | **Absent** (production/abandon only) |
| Sim e2e — agent completing a mission | `ascent-mission` — partial (ascent only) |
| App smoke e2e (`csp --smoke --frames N`) | **Absent** — no such flag exists (`crates/csp-app/src/main.rs:1-16`) |
| Interface e2e (MCP/WS) | `csp-mcp` stdio host + one mission test; tool coverage limited to flight verbs |

**The reachability-gap answer:** even at full command coverage this suite cannot catch PRD-16-class gaps — every path enqueues `SimCommand`s directly against `World`, bypassing the UI shell where reachability dies. What it *could* catch (and currently doesn't): sim-side "no path exists" conditions — unreachable contract orbits, unsatisfiable prereq chains, colony states no command sequence can produce. That requires F1's full-data world plus the mission-level scenarios in F3–F5.
