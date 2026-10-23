# Cluster C20 — Deep review: `csp-bench` determinism gate

Scope: `crates/csp-bench/src/{lib,agent,contracts,economy,eval,perf,replay,scenario,scenarios}.rs`
+ `crates/csp-bench/tests/eval_suite.rs`. Read fully; `csp-sim` internals (`drain_at`,
`step_frame`, `control_tick`, `WorldTickIo::issue`, `WorldSnapshot`) cross-checked
to validate the harness contracts.

---

### [HIGH] Finding 1: `replay()` re-enqueues sim-produced (Script/Autopilot) log entries — double-application guarantees StampMismatch on any scripted/autopiloted recording

**File:** `crates/csp-bench/src/replay.rs:660-680` (group enqueue loop)

**Problem:**
`replay()` treats every `command_log` entry as an external input and re-issues it:
`world.enqueue(key.source_class, key.source_id, command.clone())` for *all* entries
in a same-tick group. But the command log also contains **internally produced**
commands: `step_bubble_range`'s per-tick `control_tick` runs scripts/autopilots whose
`TickIo::issue` pushes into `self.pending` (csp-sim `lib.rs:7367-7372`), and the
next `drain_at` logs them stamped `SourceClass::Script`/`Autopilot` (asserted by the
csp-sim test at `lib.rs:16861`). During replay, the `advance_to(world, tick)` call
(step 1) re-runs those control passes, so the sim re-produces the same command into
`pending` *before* the group is re-enqueued. At the tick-T drain the source FIFO
holds `[sim-produced copy, replayed copy]` → two log entries where the recording has
one → `produced.len() != j` / `produced[i..j] != log[i..j]` → `StampMismatch`.

**Evidence:**
- `replay.rs:667-674` — unconditional `world.enqueue(key.source_class, …)` for every
  logged entry.
- `replay.rs:649-650` — `set_script_engine(RhaiHost)` / `set_autopilot_driver` are
  installed *precisely so* "a recorded run whose snapshot carried scripts/autopilots
  replays their `on_tick` calls" — i.e. the sim is expected to re-produce them.
- `csp-sim/src/lib.rs:3231-3247` — in-range `drain_at(t)` + `control_tick(t)` per
  bubble tick; `lib.rs:5941-5954` — every drained command is appended to
  `command_log`.
- `scenario.rs` `AgentSpec::Script` produces exactly such logs in eval runs; the
  moment a `ScenarioRecipe` (or `csp-mcp` save → record path) includes an active
  script/autopilot, `record()`'s self-check fails.

**Impact:**
Any recorded run containing Script/Autopilot-stamped entries can *never* verify —
`record()` returns `SelfCheckFailed`, so such scenarios are silently un-golden-able.
The failure is loud (not silent divergence), but it caps the determinism tripwire at
exactly the class of runs the artifact format claims to support, and a future recipe
author will hit it as an unexplained harness rejection.

**Suggestion:**
Do not re-enqueue entries whose stamp denotes an in-sim producer
(`SourceClass::Script` / `SourceClass::Autopilot`); let the control passes reproduce
them and let the existing `produced == log` check verify the reproduction. For this
to be sound, the recorder contract (`scenarios.rs` module doc) must also forbid
*externally* enqueuing those classes, since the artifact can't tell the two apart.

---

### [MEDIUM] Finding 2: `AgentSpec::Log` off-by-one — `LoggedStep.tick` lands stamped at `tick - 1`, not the §7.6 drain tick its docs claim

**File:** `crates/csp-bench/src/scenario.rs:90-98` (doc), `528-552` (`LogAgent::act`)

**Problem:**
`LogAgent` issues `step.command` when `snap.tick + 1 == step.tick`, i.e. while the
clock stands at `step.tick - 1`. Commands drain stamped at the *pre-advance* clock
(csp-sim `drain_at` stamps `self.clock.ticks()` before `try_advance`; trivial golden
stamps `AdvanceTo(500)` at tick 0 after `step_frame(1)`). So a `LoggedStep { tick: T }`
drains stamped `T - 1` — one tick early under §7.6 stamp semantics. Symmetrically, a
step with `tick == snap.tick` is *currently issuable* (it would stamp exactly
`tick`), but the `Greater` arm reports `Fail("behind the clock")`.

**Evidence:**
- `log-replay.ron` declares `(tick: 64, command: SetWarp(4))`; it drains stamped 63.
  Its goals only check `tick >= 512` / `warp_level == 4`, so the shift is invisible.
- Doc says "re-issue `command` so it drains at `tick`" and the scenario comment
  claims "a verbatim command log re-issued at its recorded ticks (§7.6 semantics)".
  Feeding a real §7.6 `command_log` into `AgentSpec::Log` shifts every stamp −1 and
  applies each command a frame early — a different run, not a verbatim replay.
  (It is self-consistent with `TranscriptEntry.tick = clock + 1` — the lockstep
  layer's "boundary label" convention — but that is *not* the §7.6 stamp.)

**Impact:**
The `Log` source cannot faithfully re-issue a recorded log; every command applies
one tick early, and the recorded-tick bookkeeping (Fail on "behind the clock") is
shifted too. Either a logic off-by-one or a contract doc that promises what the code
doesn't do — either way it misleads anyone replaying §7.6 artifacts through it.

**Suggestion:**
Issue when `snap.tick == step.tick` and hop with `AdvanceTo(step.tick)` (landing the
clock *on* the drain tick). Alternatively keep the boundary-label convention but
rename the field/docs so `tick` clearly means "boundary index" = stamp+1 — and say
explicitly that §7.6 `key.tick` maps to `LoggedStep.tick = key.tick + 1`.

---

### [MEDIUM] Finding 3: `drain_pending` ignores `report.partial` — a setup `AdvanceTo` past `MAX_TICKS_PER_FRAME`/event budget silently truncates

**File:** `crates/csp-bench/src/scenario.rs:469-494` (`drain_pending`)

**Problem:**
Setup commands enqueue `(Ui, 0)` and `world.step_frame(t + 1)` runs once. A
`SetupSpec::Command(SimCommand::AdvanceTo(S))` raises the call's effective target to
`S` (`apply_command` at csp-sim `lib.rs:4044`); when `S - t` exceeds
`MAX_TICKS_PER_FRAME` (69 120) or the event budget trips, the report is `partial`
and the world stops mid-advance. `drain_pending` checks only `rejected` and returns
`Ok` — the remaining advance is dropped with no error, no re-issue, no record.
`AdvanceTo` intent is per-call (not persisted), so nothing ever resumes it.

**Evidence:**
- `scenario.rs:480-481`: `let report = world.step_frame(t + 1)?;` — `report.partial`
  never inspected; `report.effective_target` never re-issued.
- `AgentSpec::Idle` doc (`scenario.rs:106-108`) explicitly recommends putting
  `AdvanceTo` in `setup` for advancing idle worlds — exactly the path that
  truncates. With `Idle`, the agent `Done`s on the first boundary and the run ends
  at ~69 120 ticks instead of the authored target; a `tick ge <small>` goal then
  passes while the intended scenario state was never reached.

**Impact:**
A scenario author's large setup advance silently lands short; goals evaluate on an
earlier state than declared — a verdict computed on the wrong world. For `Idle`
scenarios the mis-fly is invisible unless a goal happens to bound the tick.

**Suggestion:**
Loop `step_frame` on `report.partial` against `report.effective_target` inside
`drain_pending` (same re-issue discipline as `replay::advance_to`), or reject
`partial` setup drains as `EvalError::Setup` so truncation can't pass silently.

---

### [MEDIUM] Finding 4: `Lockstep::step` never resumes partial frames, and the transcript/report drop both `partial` and `rejected`

**File:** `crates/csp-bench/src/agent.rs:243-305` (`Lockstep::step`),
`scenario.rs:627` (`run.step()?` discards the report)

**Problem:**
One boundary = one `step_frame` call. When an agent issues `AdvanceTo(T)` with
`T - clock > MAX_TICKS_PER_FRAME` (or the event budget trips), the frame returns
`partial` mid-advance and the loop moves on — `ScheduleAgent` can never re-issue
(the command is already popped), so the advance silently truncates to ≤69 120
ticks/boundary. `LogAgent` self-heals only because it re-derives its hop from
telemetry each boundary. Additionally, `report.rejected` is dropped on the floor:
an `Act` transcript entry records the agent's commands but not that they were
refused at the drain — the "reproducible as logs" artifact overstates what applied.

**Evidence:**
- `agent.rs:301` — single `step_frame(tick)`; `report.partial` unread.
- `agent.rs:257-262` — transcript stores the commands; no field records the drain's
  rejection of them (§2.2.1 rejections ride `AdvanceReport::rejected`, per csp-sim
  `lib.rs:1186-1195`).
- `scenario.rs:627` — `run.step()?` keeps only `commands_applied` (via
  `run.applied()`); rejections never reach `EvalReport`.

**Impact:**
A scheduled/scenario fast-forward beyond one frame's worth of ticks lands short
invisibly; a `pass` can be computed on a run whose agent's commands were rejected.
Both are honesty gaps in the artifacts an external eval driver consumes.

**Suggestion:**
Surface `report.rejected` (and ideally `partial`/`reached_tick`) on the
`TranscriptEntry` or `EvalReport`; document the one-frame-per-boundary advance
semantics for `AdvanceTo`, or have the loop drain partials to the effective target
before the next poll if the intended semantic is "the advance completes".

---

### [MEDIUM] Finding 5: `ScenarioFile.save_seed` is declared but never applied — a scenario's seed silently runs as 0

**File:** `crates/csp-bench/src/scenario.rs:334-338` (field), `426-462` (`build_world`)

**Problem:**
`build_world` never calls `world.set_save_seed(file.save_seed)` (compare
`replay::record` at `replay.rs:539`, which does). The field doc says "carried, not
consumed — M0's `World` draws no seeded streams yet", but that claim is stale:
contract offer generation draws the seeded KDF stream (`csp-sim/src/contracts.rs:542`,
exercised by `contracts.rs` probes via `world.set_save_seed(seed)`). A scenario
declaring `save_seed: N` runs seed 0 with no warning, and the report doesn't record
the seed at all.

**Evidence:**
- `grep save_seed` in `scenario.rs` hits only the field declaration/doc.
- `contract_world` (`contracts.rs:78`) applies the seed explicitly, proving the sim
  consumes it.

**Impact:**
Determinism is intact (0 is constant) but the schema lies: seeded domains (contract
boards) are silently pinned to the default; a scenario authored to vary the seed is
wrong with no error. Fix is one line: `world.set_save_seed(file.save_seed)` in
`build_world` — plus update the stale doc claim.

---

### [LOW] Finding 6: `record()` will happily write an empty-log golden that `verify_golden` then rejects as malformed

**File:** `crates/csp-bench/src/replay.rs:533-574` (`record`), `761-767` (`verify_golden`)

**Problem:**
`verify_golden` refuses `command_log.is_empty()` ("a vacuous golden"), but `record`
doesn't enforce it. A recipe whose `drive` never enqueues a command and ends at the
initial clock passes the self-check trivially (replay of an empty log reproduces
the hash), `csp-replay record` writes the three artifacts, and every subsequent
`verify` fails `MalformedGolden`. The gates disagree about the same artifact.

**Suggestion:**
Hoist the non-empty check into `record` (or `write_golden`) so an unverifiable
artifact can never be produced.

---

### [LOW] Finding 7: `fixture_hash` excludes artifact files by *file name at any depth*, and follows directory symlinks unboundedly

**File:** `crates/csp-bench/src/replay.rs:483-493`

**Problem:**
The artifact exclusion matches `path.file_name()` — a fixture data file nested in a
subdir named `replay.bin` / `expected-hash.txt` / `fingerprint.json` is silently
dropped from the hash: a drift in it is invisible to `FixtureDrift` even though the
recipe's `initial` may load it. (The docs say "skips exactly these names", so it's
technically as-documented, but the intent is to exclude the three *top-level* golden
outputs.) Separately, `path.is_dir()` follows symlinks and the DFS has no cycle
guard — a symlinked dir pointing at an ancestor loops the walk forever.

**Suggestion:**
Exclude only top-level artifact paths (compare the full relative path, depth 1), and
skip (or error on) symlinked directories — `read_dir` on a committed fixture should
never see them anyway, so refusing is honest.

---

### [LOW] Finding 8: `tick_timeout` drops on the `n+1`-th consecutive `Wait`, not the `n`-th

**File:** `crates/csp-bench/src/agent.rs:265-275`, `88`, `172-189`

**Problem:**
`dropped = slot.waits > t` after incrementing — `Some(2)` tolerates waits 1 and 2 and
drops on the 3rd (the test pins this). `Slot::tick_timeout`'s doc reads "Max
consecutive `Wait`s before the agent is dropped" and `add_agent` says "drops the
agent after `n` consecutive `Wait`s" — both naturally read as "the n-th wait drops".
The behavior is tested and self-consistent, so this is a contract-wording hazard for
the eval surface, not a sim bug.

**Suggestion:**
Either `>=` (drop at the n-th wait) or reword to "drops on the `n+1`-th consecutive
`Wait`" / "allows `n` misses".

---

### [LOW] Finding 9: `Replay.save_seed` is never cross-checked against `initial_snapshot.save_seed`

**File:** `crates/csp-bench/src/replay.rs:149-151`, `612-620`

**Problem:**
The artifact carries `save_seed` twice: as a top-level field and inside
`initial_snapshot` (the value the sim actually consumes). Nothing verifies they
agree — a doctored artifact can advertise one seed while the snapshot restores
another. Cosmetic today (the snapshot is authoritative), but the doctored-artifact
check in `verify_golden` already validates analogous redundancy
(`data_fixture_hash` vs `fingerprint.data_fixture_hash`); this one is unchecked.

**Suggestion:**
Add `artifact.save_seed == artifact.initial_snapshot.save_seed` to
`verify_golden`'s doctored-artifact checks.

---

### [LOW] Finding 10: `StampMismatch { index }` reports the group start, not "the first divergent entry" as documented

**File:** `crates/csp-bench/src/replay.rs:225-228` (doc), `676-679`

**Problem:**
`Err(ReplayError::StampMismatch { index: i })` — `i` is the first index of the
current same-tick group. If a divergence occurred at index `i + k` (extra
sim-produced entry, a mid-group seq drift), the error still names `i`. The field doc
promises "index into the log of the first divergent entry". Diagnostics-grade nit —
the group start is still a useful cursor — but the doc overstates it.

**Suggestion:**
Find the first `produced[i..j]`/`log[i..j]` pairwise difference and report that
index, or reword the doc.

---

### [LOW] Finding 11: `run_suite` silently drops `read_dir` entry errors

**File:** `crates/csp-bench/src/eval.rs:212-218`

**Problem:**
`.flatten()` on the `read_dir` iterator discards `Err` entries — a scenario file
that fails to enumerate vanishes from the suite with no `SuiteEntry`, so the suite
reports green over a silently smaller file set. `load_scenario` IO failures are
surfaced per-file, but enumeration failures aren't.

**Suggestion:**
Map each `Err` entry to a `SuiteEntry { outcome: Err(EvalError::Io(..)) }` (path may
be partial) or fail the whole suite — a determinism suite shouldn't lose files
quietly.

---

### [LOW] Finding 12: economy probe ignores hire-drain rejections; duplicates `TICK_HZ` as a literal

**File:** `crates/csp-bench/src/economy.rs:63`, `99-114`

**Problem:**
(a) `w.step_frame(w.clock().ticks() + 1)` after the `HireCrew` enqueues discards
`report.rejected` — a rejected hire (insufficient funds after `drain_fraction` eats
the model) silently shrinks the roster; `staffed_base_drains_in_band` never asserts
`crew_count == staff`, so a mis-staffed run is diagnosed as an odd ratio, not the
rejection it was. (b) `const TICKS_PER_SECOND: u64 = 64` mirrors
`csp_core::TICK_HZ` — the same silent-drift hazard the crate's own
`balance_ron_mirrors_*` tests exist to prevent.

**Suggestion:**
Check `rejected` after the hire drain; use `csp_core::TICK_HZ`.

---

### [LOW] Finding 13: `mcp_mission_agent_flies_ascent` accepts a non-JSON `raw` tool payload as proof of lease rejection

**File:** `crates/csp-bench/tests/eval_suite.rs:375-379`

**Problem:**
`denied["rejected"].as_array().is_none_or(|r| !r.is_empty()) || denied.get("raw").is_some()`
— `raw` is populated when `content[0].text` isn't JSON, so a broken/erroring
`vessel.control` response (plain-text error, crash output) satisfies the assertion
the test exists to pin. The gate is "exercised, not assumed" per the comment, but
this arm assumes exactly that.

**Suggestion:**
Require a real `rejected` array for the denial case; treat `raw` as a test failure.

---

## Non-findings (checked, correct)

- **`replay()` group semantics** (empty-queue advance, `target = max(tick+1, group
  AdvanceTo args)` = the recorded `effective_target`, `produced[i..j] == log[i..j]`,
  `StampBehindClock`, `ClockMismatch` pinning the terminal advance) are internally
  consistent with csp-sim's drain/stamp rules for externally-sourced commands.
- **`advance_to`'s stall predicate** correctly treats zero-advance flush frames
  (`events_processed > 0` on a deferred over-ceiling tail) as progress; every
  non-flush iteration either advances the clock or consumes queue items, so it
  terminates.
- **`WorldSnapshot`** carries all per-world tunables (`event_budget`,
  `vessel_caps`, `docking`, `transfer_cfg`, `science_cfg`, `economy_cfg`,
  `contract_cfg`, `life_support_cfg`, `script_op_budget`, `reliability_cfg`); the
  `Replay.config` channel correctly covers the re-derived data tables — nothing is
  silently dropped between record and replay.
- **`fixture_hash`** ordering/canonicalization is deterministic; the self-
  referential-artifact exclusion is right in intent (Finding 7 is about the match
  rule's breadth).
- **Goal evaluation** is honest: unknown paths → `observed: None, passed: false`;
  non-numeric JSON under `Num` → false; `Ne` on a missing field fails rather than
  vacuously passing.
- **`settle_ticks`/`stop_when`** countdown is correct (predicate samples only until
  first trip; exactly `settle_ticks` extra boundaries run).
- **`AscentAgent`** lease handshake, `bind`→`Lease::Agent(client)` wiring, and
  NaN-degenerate steering (→ `DQuat::IDENTITY` via `partial_cmp` guard) are sound;
  `f64::clamp` does not panic on a NaN self, so equal `pitch_start`/`pitch_end`
  args degrade to identity attitude rather than a crash.
- **`perf.rs`** machine detection, hardware-gated honesty (`HardwareGated` never
  blocks `all_passed`), strict-vs-diagnostic `bound()` split, and coverage test are
  sound.
