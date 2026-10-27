# Review Report C30 — Bench: scenarios + economy/contracts harness + csp-mcp bin

Session `session-full-20261026`, cluster C30. Reviewed against the CURRENT working tree
(HEAD `7dd4b86`). All in-scope tests were run and pass (`cargo test -p csp-bench --lib` → 53 ok;
`--test eval_suite` → 12 ok; contracts probe → 2 ok). Findings below are PRD-coverage,
harness-honesty and DRY issues — nothing is red today, which is itself the problem: the suite
is green because it never touches the new surface.

Files reviewed: `crates/csp-bench/src/{scenarios,scenario,economy,contracts}.rs`,
`crates/csp-bench/src/bin/csp-mcp.rs`, `crates/csp-bench/tests/eval_suite.rs`, plus
`agent.rs`/`eval.rs` and the 5 files in `data/eval-scenarios/` (harness side; content owned by C34).

---

### [HIGH] Finding 1: The eval-scenario tier still proves only the M2 flight arc — the new player loop has no scenario, and the format's world surface cannot host one

**File**: `crates/csp-bench/src/scenario.rs:62-72`, `crates/csp-bench/src/scenarios.rs:50-63`, `data/eval-scenarios/*`

**Problem**: PLAN §8.5 (Sim e2e row) defines what the scenario suite must prove: *"Whole systems
interact correctly over mission-scale time: launch→orbit→land, **contract lifecycle**,
**colony supply loop**, agent completing a mission"*. The committed suite (5 scenarios) contains
three ascent variants (`ascent-mission`, `lease-launch`, `script-ascent`) and two plumbing probes
(`idle-warp`, `log-replay`). None exercises manufacture, contracts, colonies, crew, science, or
save/load. This is not just missing files — the format physically cannot express them:

- `WorldSpec` has exactly two variants: `Empty` (no data) and `Fixture{name}`, and the recipe
  registry (`SCENARIOS`, scenarios.rs:50-63) contains only `trivial` and `flight-launch`.
- The flight-launch fixture dir (`data/test-fixtures/flight-launch/`) ships `balance.ron`,
  `sites.ron`, `system.ron`, bodies — but **no** `contracts/`, `recipes/`, `experiments/`, or
  `colonies.ron`; `flight_initial` (scenarios.rs:186-247) loads each of those as empty and never
  calls `set_tech_tree`/`set_parts`. A scenario author today can issue `AcceptContract` /
  `ManufactureCraft` / `RunExperiment` via `SetupSpec::Command`, but every one rejects: the board
  never publishes (empty `contract_defs`), `RecipeId`s don't resolve, experiments don't validate.
- Grep across csp-bench: `ManufactureCraft`, `BoardCrew`, `JoinColonyInventory`,
  `ResourceTransfer`, `RunExperiment` appear in zero non-comment code.

**Evidence for "the game moved, the harness didn't"**: the full shipping-data wiring exists in
this very crate — `contracts.rs:69-112 contract_world` ("the `csp-view` wiring distilled") builds
system + tech tree + experiments + sites + contracts — but it is reachable only from the Rust
probe, never from `scenario.ron`.

**Impact**: the eval tier is the automated playability proof. Phase2–4 + W2.6 wired
manufacture-costed launch, colonies, contracts, crew, save/revert as the player surface — a
regression in any of that glue (e.g. a manufacture debit path broken by a balance change, a
resupply predicate that stopped firing) leaves `committed_eval_suite_all_pass` green. §6.4's
cut-list sanctions none of this (it cuts render tier/life support/third body/docking/web polish —
not e2e coverage).

**Suggestion**: add a `WorldSpec::Data(root)` (or a third recipe) that reuses
`contracts.rs::contract_world`; then three scenarios close the §8.5 gaps:
(1) manufacture launch — `ManufactureCraft` in setup, goal on `program.funds_micros` delta +
the delivered vessel's `residence == "SurfaceLatched"`; (2) contract lifecycle — accept a drawn
offer, satisfy it, goal on funds/facts; (3) colony resupply — `FoundColony` +
`JoinColonyInventory` + transfer, goal on `program.colonies.0.stores`. The goal-path surface
already exists: `TelemetrySnapshot::program` exposes funds/offers/active/crew/colonies/sites
(`csp-server/src/telemetry.rs:52-73`), so predicates need no new plumbing.

---

### [HIGH] Finding 2: Contracts harness verifies 1 of 6 shipping templates' completion semantics — rescue and colony_resupply (the new ones) are verified nowhere

**File**: `crates/csp-bench/src/contracts.rs:152-249`

**Problem**: `contract_run` drives only `ContractParams::SatelliteOrbit` (accept → spawn into the
drawn band → completion). The board-gate test (`the_board_respects_the_fact_gate`) only checks
that `contract.survey` *offers*. The remaining shipping templates — `contract.rescue`,
`contract.colony_resupply`, `contract.survey.data`, `contract.part_test` — have no lifecycle
exercise anywhere in csp-bench. Their predicates are the riskiest code in §3.6: `Rescue` is
transition-keyed on `VesselRecovered(body)` (contracts.rs sim:988-990), `ColonyResupply` sums the
`colony_deliveries` fact keyed by colony arena ids (sim:997-1016), `SurveyData` computes
returned = collected − pooled − escrowed (sim:1020+). The exploration map records that two
templates were uncompletable at seed and rescue/colony_resupply were just added — exactly the
failure class a lifecycle harness exists to catch.

**Honesty note (positive, for the record)**: the satellite path *is* faithful to game semantics —
`apply_spawn_vessel` fires the same `ContractTrigger::OrbitAchieved`
(`csp-sim/src/lib.rs:5870-5872`) that the auto bubble→rails edge fires (lib.rs:8568), and the
predicate reads live state, so the probe tests the real evaluation path, just not the
player-flown route into it.

**Impact**: a broken rescue trigger or a resupply fact that never accrues ships silently.

**Suggestion**: extend `contract_run` into a per-kind switch (it already reads `contract.params`):
rescue — spawn + latch on the drawn body + `RecoverVessel`; resupply — found a colony, deliver
into stores, watch the fact accrue; part-test — spawn a craft with the module in-regime. Fold
into scenario files once Finding 1's world spec lands.

---

### [MEDIUM] Finding 3: csp-mcp bin's `session.save`/`session.load` + `with_reconfigure` path has zero automated exercise

**File**: `crates/csp-bench/src/bin/csp-mcp.rs:73-94`, `crates/csp-bench/tests/eval_suite.rs:398-503`

**Problem**: §2.12.1 names the stdio bin as the thing "an eval harness spawns". The e2e mission
test drives initialize/tools/spawn/lease/ascent/`session.snapshot` and stops. The `session.save`
and `session.load` tools (registered at `csp-server/src/mcp.rs:314-315`) are never called by any
bench test, and the bin's only save-hardening logic — the `with_reconfigure` closure that
re-installs the Rhai host, ascent autopilot, and fixture `script_op_budget` after a load
(csp-mcp.rs:80-94) — is dead code as far as the test suite can tell. Phase4 made CSPSAVE the
player save format and revert a player verb; the agent-side proxy of that flow (save mid-mission,
load, continue) is untested.

**Impact**: a regression in `LocalGame::load_save`'s reconfigure hook or in the bin's fixture
reload (e.g. the balance-read failure branch at csp-mcp.rs:85-89) would not redden anything in
csp-bench. (csp-server's backend has its own unit tests — the *bin glue* is the untested part.)

**Suggestion**: extend `mcp_mission_agent_flies_ascent`: mid-climb `session.save` →
`session.load` → assert telemetry `tick`/`apoapsis` match the pre-save values, the summary's
`warnings` are empty (`missing_config_channels` clean), and steering still works afterwards
(proves the engines were re-installed).

---

### [MEDIUM] Finding 4: `EvalScore` excludes setup-phase funds and time — manufacture debits would be invisible to the score

**File**: `crates/csp-bench/src/scenario.rs:627-629, 717-721`

**Problem**: `funds_start` is captured *after* `build_world` (which drains all of `setup`), and
`sim_time_s` uses `Lockstep`'s `start_tick`, also post-setup. §2.12.1's score contract is
"(Δv used, sim-time elapsed, funds spent)" for the run. Setup is part of the run: the
`setup_advance_to_survives_frame_budget_partials` test itself shows setup advancing 200 000 ticks
that the score would report as 0 s. Once a manufacture scenario exists (Finding 1), the entire
manufacture charge — the new economy's most important number — lands in setup and the score
reads 0 funds spent.

**Impact**: cross-scenario score comparison is defined on a boundary that hides exactly the
costs §2.12.1 wants measured.

**Suggestion**: have `build_world` return the pre-setup funds (and record the pre-setup clock),
or capture both before the setup drain and thread them into `EvalScore`.

---

### [MEDIUM] Finding 5: Colony economy probe runs a synthetic world, not the shipping colony; §8.3's "plots the currency curves" is not implemented

**File**: `crates/csp-bench/src/economy.rs:201-312`

**Problem**: §8.3 says "The economy harness is a shipping-data tool". `colony_production_run`
authors a fake `BodyId("terra")`/`SiteId("pad")`/inline `ColonyDef` (generator+drill+store) with
a synthetic 21 600 s rotation, instead of loading `data/systems/home` and the authored Rask
outbreak (`colonies.ron`, schema v2 — the tidally-locked EC balance C34 is verifying). The
staffed-base probe likewise never touches facilities/upkeep (§3.5's recurring drain is "salaries
**+ facility upkeep**"; `StaffAttrition`/`FacilityDormant` do not exist in the sim — C08's gap —
so the harness cannot include them either). Also §8.3's "plots the currency curves" has no
artifact: `EconomyReport::months` never leaves the crate (no bin renders it; the four csp-bench
bins are eval/mcp/perf/replay only).

**Impact**: a balance regression in the *authored* colony content (Rask power math, drill rates)
is invisible to the §8.3 nightly gate; the drain curve exists only inside a test assertion.

**Suggestion**: add a shipping-data mode to the colony probe (load home system + colonies.ron,
run the authored outpost for N months, assert stores accrue and state stays `Nominal`), and emit
the month curve as JSON from a small bin or `csp-perf`-style report so "plots" has a substrate.
(The parallel-verified self-solving-salary finding for `staffed_base_run` is acknowledged and
not re-derived here.)

---

### [MEDIUM] Finding 6: DRY — steering math, Rhai source, pad-spawn pose, and data-set wiring are hand-copied 2–4×; `install_config` exists precisely to prevent the wiring copies

**File**: `crates/csp-bench/src/eval.rs:109-131` vs `tests/eval_suite.rs:517-544`;
`src/scenarios.rs:996-1027` vs `data/eval-scenarios/script-ascent.ron`;
`src/scenarios.rs:352-372` vs `tests/eval_suite.rs:433-441`;
`src/scenarios.rs:145-249` vs `src/contracts.rs:69-112` vs `src/replay.rs:783-821`;
`src/contracts.rs:128-144`

**Problem**:
- The gravity-turn steering (`f = (alt−1500)/58500`, `pitch = π/2·(1−f)`, `up·sin + prograde·cos`)
  is implemented twice in Rust — `AscentAgent::steer_dir` (DVec3) and the test's `ascent_steer`
  ([f64;3]) — verbatim. The test's "independent computation" comment is undermined by it being a
  transcription; a fix to one silently leaves the other.
- The Rhai ascent script exists verbatim in `scenarios.rs`'s test and in
  `data/eval-scenarios/script-ascent.ron`. Drift means the committed scenario and the "same
  profile" unit test fly different controllers with no signal.
- The pad spawn pose (`pad = (R+3.5, 0, 0)`, `rot0 = Y(π/2)`, `vel = spin×(pad−center)`) is
  duplicated between `spawn_pad` and the eval_suite MCP test (which hand-builds the
  `SpawnPhysicalVessel` because `spawn_pad` needs `&mut World`).
- World wiring is hand-rolled three times with different channel subsets: `flight_initial`
  (~15 setters incl. caps/docking/transfer/colonies), `contract_world` (tech tree, no physics),
  and `replay.rs`'s restore-reinstall (7 `set_*` calls). The sim now ships
  `World::config()`/`install_config` whose doc (`csp-sim/src/lib.rs:2706-2711`) says hosts
  hand-rolling this "dropped channels (the MCP `session.load` path re-installed 3 of ~10 and
  silently diverged)". `contracts.rs mark_facts` (128-144) is another hand-rolled copy — it
  reinstalls 5 of the 7 `install_config` channels (misses `parts`, `colony_env`; unused there
  today, one balance change away from mattering).

**Impact**: four copies of load-bearing math/wiring; three have already diverged in coverage.

**Suggestion**: make `eval::steer_dir` the single implementation (the test calls it);
make the test read the Rhai source from `data/eval-scenarios/script-ascent.ron` (or share a
`pub const ASCENT_SCRIPT`); extract `pub fn pad_pose(env) -> (DVec3, DQuat, DVec3)` from
`spawn_pad`; replace `mark_facts`' five setters and replay's seven with
`w.install_config(&artifact.config)`; add a `csp_bench::shipping_world(root)` builder shared by
`contract_world`, `flight_initial`, and Finding 1's `WorldSpec::Data`.

---

### [LOW] Finding 7: `event_budget` is enforced off-by-one and only after a whole boundary

**File**: `crates/csp-bench/src/scenario.rs:686-689`

**Problem**: the check `run.applied() > file.event_budget` runs after `run.step()`, so a run
applies `event_budget + (last boundary's batch)` commands before stopping; the field doc
("Max applied commands before the loop stops", scenario.rs:375-376) oversells by one batch. For
per-boundary-bounded sources this is one command; for a Schedule step carrying N commands it is N.

**Impact**: score/budget comparisons between runs are computed on a slightly undefined budget
semantics; cosmetic today.

**Suggestion**: either document "stops once the budget is exceeded" or check `>=` before the
step and clamp the final boundary.

---

### [LOW] Finding 8: `terrin_circular` hardcodes Terrin's radius and carries a self-arguing doc; the probe is seed-fragile when no satellite offer is on the board

**File**: `crates/csp-bench/src/contracts.rs:49-64, 186-192`

**Problem**: `terrin_circular` bakes `600_000.0` in (it matches `data/systems/home/terrin/body.ron:16`
and the template pins `bodies: ["terrin"]`, so it is correct today — but the function takes the
contract's drawn `body` at the call site while ignoring it for the datum). The doc comment
literally argues with itself ("satisfies both for the authored ranges? No — …"). Separately,
`contract_run` falls back to `probe.offer` when no satellite offer is on the board; with such a
seed the accepted contract is not a satellite, the `if let` spawn is skipped, completion never
fires, and `the_lifecycle_is_deterministic` fails on the funds assertion with no diagnosis.

**Impact**: latent trap for anyone re-seeding the probe or adding bodies to the template's
`bodies` list.

**Suggestion**: pass the body's radius from the loaded system (`contract_world` has `sys`);
replace the confused comment with one sentence; make the no-satellite-offer case an explicit
`ReplayError` ("seed produced no satellite offer").

---

### [LOW] Finding 9: Two dead/misleading assertions and an unused knob

**File**: `crates/csp-bench/src/contracts.rs:298-301`; `crates/csp-bench/src/economy.rs:57`

**Problem**: (a) `assert!(funds_after_complete > funds_after_accept || a.completed.is_empty(),
"completion paid only when the contract completed")` — `completed` always contains the injected
survey fact, so the right disjunct is dead and the message claims a discrimination the assert
does not perform. (b) `EconomyProbe::band_months` is read only by tests, never by
`staffed_base_run` — a probe knob that does nothing in the probe.

**Impact**: reader trust; no functional risk.

**Suggestion**: drop the vacuous disjunct; move `band_months` into the test (or honor it in the
probe's month recording).

---

## PRD coverage table

§8.5 Sim-e2e row ("launch→orbit→land, contract lifecycle, colony supply loop, agent completing
a mission") + the new player loop, vs what csp-bench actually exercises:

| Player flow | Eval scenario (.ron) | Probe/test in csp-bench | Verdict |
|---|---|---|---|
| Launch → climb → apoapsis cut | `ascent-mission`, `lease-launch`, `script-ascent` | — | WIRED |
| Launch → orbit → reentry → land → recover | none | `flight_run` golden recipe + test | PARTIAL (golden, not eval tier) |
| Contract lifecycle — satellite | none | `contracts.rs` probe | PARTIAL (probe only, spawn-into-band) |
| Contract lifecycle — survey / rescue / colony_resupply / part_test | none | **none** | MISSING |
| Colony production / abandon | none | `economy.rs` synthetic probe | PARTIAL (synthetic world) |
| Colony supply loop (resupply delivery) | none | **none** | MISSING (§8.5 names it) |
| Manufacture-costed launch (`ManufactureCraft`) | none | **none** (grep: zero issuers) | MISSING — the new launch hot path |
| Save / revert (agent-side `session.save`/`load`) | none | **none** in bench (backend unit tests in csp-server) | MISSING |
| Crew hire | none | economy probe (staffing only) | PARTIAL |
| Crew board / vessel transfer | none | **none** | MISSING |
| Science run / transmit / recover data | none | **none** in bench (sim unit tests only) | MISSING |
| Agent completes a mission (§2.12.1) | `ascent-mission` (a climb, not a mission) | — | PARTIAL |
| Lease gate, scripting, warp plumbing | `lease-launch`, `script-ascent`, `idle-warp`, `log-replay` | agent.rs, mcp e2e | WIRED |
| Autopilot ascent | none (not a .ron) | scenarios.rs test | PARTIAL |
| Economy bands / save-size / insolvency (§8.3) | n/a | economy.rs tests | WIRED (salary self-solve caveat is the parallel-verified finding) |

## Top 3 issues

1. **Finding 1 (HIGH)** — the scenario format has no data-rich world spec, so the §8.5 proofs
   "contract lifecycle" and "colony supply loop" cannot be authored, and manufacture / save-revert /
   crew / science have no automated playability exercise at all. Fix: `WorldSpec::Data` reusing
   `contract_world`, plus three scenarios.
2. **Finding 2 (HIGH)** — contracts harness covers 1 of 6 templates; rescue and colony_resupply
   (new, historically uncompletable class) verified nowhere.
3. **Finding 3 (MEDIUM)** — csp-mcp's `session.save`/`session.load` and its `with_reconfigure`
   fixture-reload hook are dead code to the test suite; the agent-side save/revert proxy is untested.
