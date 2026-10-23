# Cluster C20 — Verification of `review-report-20.md`

Scope verified against `crates/csp-bench/src/*` plus the cross-referenced
`csp-sim`/`csp-script`/`csp-ai` internals (the report's `csp-bench/src/*`
paths map to `crates/csp-bench/src/*` in this tree).

Method: line-level re-derivation of every cited mechanism, plus **live
reproductions** for Findings 1, 2, 3, 5, 6 via a temporary integration
test (`crates/csp-bench/tests/zz_verify20.rs`, since deleted — no source
files modified). The `Lockstep`/`record`/`verify_golden` paths were
exercised against the committed `flight-launch` fixture.

| # | Severity | Verdict |
|---|----------|---------|
| 1 | HIGH | **CONFIRMED** — reproduced: `record()` of a scripted run returns `SelfCheckFailed(… StampMismatch …)` |
| 2 | MEDIUM | **CONFIRMED** — reproduced: `LoggedStep{tick:64}` drains stamped **63** |
| 3 | MEDIUM | **CONFIRMED** — reproduced: setup `AdvanceTo(200_000)` partials at 69 120 and `drain_pending` drops it |
| 4 | MEDIUM | **CONFIRMED** (code) |
| 5 | MEDIUM | **CONFIRMED** — reproduced: `save_seed: 42` scenario hashes identically to a seed-0 world |
| 6 | LOW | **CONFIRMED** — reproduced end-to-end (`record`→`write_golden`→`verify_golden` = `MalformedGolden`) |
| 7–13 | LOW | **CONFIRMED** (code) |

**0 DISMISSED.** All 13 findings verified.

---

### Finding 1 — `replay()` re-enqueues sim-produced Script/Autopilot entries → guaranteed `StampMismatch` — CONFIRMED (empirically)

Mechanism verified end-to-end:

- `step_bubble_range` runs `drain_at(t)` + `control_tick(t)` per in-range
  tick (`csp-sim/src/lib.rs:3245-3247`); `control_tick` → `run_script_tick`/
  `run_autopilot_tick` (`lib.rs:6088-6117`), and `WorldTickIo::issue` pushes
  into `self.pending` (`lib.rs:7367-7372`). Real scripts stamp
  `SourceClass::Script` (`csp-script/src/lib.rs:1032`), the ascent
  autopilot `SourceClass::Autopilot` (`csp-ai/src/lib.rs:200`). Every
  drained applied command is appended to `command_log` regardless of
  class (`lib.rs:5953`), so recordings legitimately carry
  Script/Autopilot-stamped entries (asserted by `lib.rs:16861`,
  `17057`-area tests).
- `WorldSnapshot` serializes `scripts`/`autopilots` (`lib.rs:1372,1383`;
  restore `2778,2781`), and `replay()` installs `RhaiHost` +
  `AscentAutopilot` (`replay.rs:649-650`) — so the restored world's
  control passes deterministically re-produce the same commands.
- In `replay()` (`replay.rs:653-680`): `advance_to(world, tick)` lands
  the clock exactly on the group tick; the last in-range
  `control_tick(tick-1)` has already pushed the sim copy into `pending`
  (the drain that would consume it — boundary drain at `tick` — runs
  only in the *next* `step_frame`, which `advance_to` never reaches).
  The unconditional `world.enqueue(key.source_class, …)` at
  `replay.rs:672` then appends a second copy to the same
  `(class, source_id)` FIFO. The next `advance_to` drains both:
  `produced.len() == j+1 != j` → `StampMismatch { index: i }`.

**Live reproduction:** a `ScenarioRecipe` driving `spawn_pad` → `LoadScript`
(`fn on_tick(ctx) { set_throttle(1.0); }`) → 8 frames was passed to
`replay::record`. Result:

```
ReplayError::SelfCheckFailed(
  "scenario 'zz-scripted-probe' artifact fails its own replay:
   replayed command at log index 2 stamped differently than recorded")
```

Index 2 is the first Script-stamped log entry (index 0 = `Spawn…` Ui,
1 = `LoadScript` Ui) — exactly the predicted failure, via the
`SelfCheckFailed` path the finding names.

Two nuances (don't change the verdict):

- *"Guarantees"* is slightly overstated in one corner: if the second
  (replayed) copy is **rejected** at apply, it leaves no log entry, so
  `produced[i..j]` can still match. Applies only to commands whose
  second application fails (e.g. a second resource debit); the dominant
  script/autopilot verbs (`SetThrottle`, `SetAttitude`, `AdvanceTo`,
  `SetWarp`) apply unconditionally, so double-stamp is guaranteed in
  practice. The failure is still guaranteed whenever the command applies
  twice cleanly.
- Externally enqueued Script/Autopilot-class commands (legal via
  `World::enqueue`, exercised by `csp-sim` test `lib.rs:9343`) would
  replay fine — the artifact cannot distinguish them from sim-produced
  ones, which is exactly the ambiguity the finding's suggestion flags.

### Finding 2 — `LogAgent` off-by-one — CONFIRMED (empirically)

`Lockstep::step` polls at `snap.tick = clock` (`telemetry.rs:26,68`),
then `step_frame(clock+1)` drains stamped at the **pre-advance** clock —
`drain_boundary` → `drain_at(self.clock.ticks())` (`lib.rs:5885-5891`)
runs before `try_advance` (`lib.rs:6836`). `LogAgent::act`
(`scenario.rs:533-552`) issues when `snap.tick + 1 == step.tick`, i.e.
while `clock == step.tick - 1` → stamped `step.tick - 1`.

**Live reproduction:** a verbatim `LogAgent` clone on `World::new` with
`LoggedStep{tick:64, SetWarp(4)}` produced `command_log` ticks `[0, 63]`
— the command stamped **63**, not 64. Matches `log-replay.ron`'s
`(tick: 64, SetWarp(4))`, whose goals (`tick >= 512`, `warp_level == 4`)
can't see the shift — as the finding states.

The second half also verified: `step.tick == snap.tick` (`next_tick >
step.tick` → `Greater`) → `Fail("behind the clock")`, yet that is
precisely the step that *would* stamp exactly `tick`. And
`TranscriptEntry.tick = clock + 1` (`agent.rs:101,244`) confirms the
boundary-label convention vs the §7.6 dequeue stamp mismatch — the doc
at `scenario.rs:90-91` and `log-replay.ron`'s "(§7.6 semantics)" comment
describe semantics the code does not implement.

### Finding 3 — `drain_pending` ignores `report.partial` — CONFIRMED (empirically)

`scenario.rs:480-481`: `world.step_frame(t + 1)` then only `rejected` is
inspected. `AdvanceTo` raises `effective_target` at apply
(`csp-sim/src/lib.rs:4044`), and `partial` = `clock < effective_target`
(`lib.rs:6867`).

**Live reproduction:** `enqueue(Ui, 0, AdvanceTo(200_000))` +
`step_frame(1)` → `partial=true, reached_tick=69120
(=MAX_TICKS_PER_FRAME), effective_target=200000`. `drain_pending`
returns `Ok` — the remaining 130 880 ticks are silently dropped. The
`AgentSpec::Idle` doc (`scenario.rs:105-108`) explicitly recommends the
`AdvanceTo`-in-`setup` pattern that hits this. Confirmed.

### Finding 4 — `Lockstep::step` drops `partial` and `rejected` — CONFIRMED (code)

- `agent.rs:301`: single `step_frame(tick)`; `report.partial` never
  read. `ScheduleAgent` pops each step once — a truncated `AdvanceTo`
  is never re-issued. (`LogAgent` happens to self-heal by re-deriving
  `AdvanceTo(step.tick - 1)` each boundary.)
- `TranscriptEntry` (`agent.rs:99-110`) has no field for drain
  rejections; `scenario.rs:627` discards the `AdvanceReport` so
  `report.rejected` never reaches `EvalReport` (only `commands_applied`
  via `run.applied()`). Rejections ride `AdvanceReport::rejected`
  (`csp-sim/src/lib.rs:1195`-area doc). Confirmed.

### Finding 5 — `ScenarioFile.save_seed` never applied — CONFIRMED (empirically)

`scenario.rs:334-338` declares `save_seed` (doc: "carried, not
consumed — M0's `World` draws no seeded streams yet"); `build_world`
(`426-462`) never calls `set_save_seed` (contrast `replay.rs:539`).
The doc is stale: the seed feeds contract-offer draws
(`csp-sim/src/contracts.rs:542,602`, fired every interval via
`publish_contracts` → `lib.rs:6022`), reliability (`lib.rs:3219,3428`),
and is **hashed** (`lib.rs:6991`).

**Live reproduction:** a scenario `save_seed: 42, world: Empty,
agent: Idle` produced `final_hash` `0f24f4…` — byte-identical to a
seed-**0** world stepped to the same tick, different from a seed-42
world (`2ed7d0…`). The declared seed is silently dropped. Confirmed.

### Finding 6 — `record()` writes goldens `verify_golden` rejects — CONFIRMED (empirically)

`record` (`replay.rs:533-574`) has no empty-log gate; `verify_golden`
(`763-767`) rejects `command_log.is_empty()`. Reproduction: a recipe
whose `drive` is a no-op produced `Ok(RecordedGolden)` with an empty
log; `write_golden` wrote the three artifacts; `verify_golden` returned
`MalformedGolden("replay.bin has an empty command log — a vacuous
golden")`. (A `drive` that enqueues-but-never-steps also works:
`pending` is excluded from snapshot and hash — `lib.rs:1229`.) Confirmed.

### Finding 7 — `fixture_hash` name-match + symlink loop — CONFIRMED (code)

`replay.rs:488-493`: exclusion tests `path.file_name()` — any nested
`replay.bin`/`expected-hash.txt`/`fingerprint.json` is dropped from the
hash. `path.is_dir()` (`484`) follows symlinks (`std::fs::metadata`); the
DFS stack has no visited set → a symlinked dir to an ancestor loops the
walk unboundedly. Confirmed as stated (documented-but-overbroad match +
unguarded traversal).

### Finding 8 — `tick_timeout` drops on the (n+1)-th Wait — CONFIRMED (code)

`agent.rs:265-266`: `waits` incremented, then `waits > t` — `Some(2)`
drops on the 3rd Wait, pinned by the test at `375-398`. Docs at
`agent.rs:88` ("Max consecutive `Wait`s before the agent is dropped")
and `170-171` ("drops the agent after `n` consecutive `Wait`s")
naturally read as the n-th. Behavior is tested/self-consistent — a
wording hazard, as filed.

### Finding 9 — `Replay.save_seed` not cross-checked — CONFIRMED (code)

`replay()` never reads `artifact.save_seed` — the restored world takes
`initial_snapshot.save_seed` (`csp-sim/src/lib.rs:2752`), so the
top-level field (`replay.rs:149-151`) is decorative inside the harness
while `verify_golden` (`761-777`) does enforce the analogous
`data_fixture_hash`↔`fingerprint` redundancy. Confirmed (cosmetic /
doctored-artifact gap only).

### Finding 10 — `StampMismatch{index}` reports group start — CONFIRMED (code)

`replay.rs:677-678` returns `index: i` (group start) while the field doc
(`replay.rs:226`) promises "the first divergent entry"; divergence within
`[i,j)` isn't pinpointed. Confirmed (diagnostics nit).

### Finding 11 — `run_suite` drops `read_dir` entry errors — CONFIRMED (code)

`eval.rs:213-216`: `.flatten()` on the `ReadDir` iterator discards `Err`
entries — a file that fails enumeration vanishes from the suite with no
`SuiteEntry`. Confirmed.

### Finding 12 — economy probe rejects unchecked + `TICK_HZ` literal — CONFIRMED (code)

- `economy.rs:114` discards the `AdvanceReport` → `rejected` hires
  invisible; `staffed_base_drains_in_band` (`357-383`) never asserts
  `crew_count == staff`. (Note: the `hundred_year_*` test *does* assert
  `crew_count == 20` at `402`, so the gap is specific to the probe and
  the band test — the finding's wording is accurate.)
- `economy.rs:63` `TICKS_PER_SECOND: u64 = 64` duplicates
  `csp_core::TICK_HZ = 64` (`csp-core/src/time.rs:9`). Same value today;
  drift hazard as filed.

### Finding 13 — `mcp_mission_agent_flies_ascent` accepts non-JSON `raw` — CONFIRMED (code)

`eval_suite.rs:299-305`: `tool()` wraps non-JSON `content[0].text` as
`{"raw": text}`; the assertion at `375-379`
(`denied["rejected"].as_array().is_none_or(|r| !r.is_empty()) ||
denied.get("raw").is_some()`) is satisfied by any malformed/erroring
`vessel.control` response. Confirmed.

---

## Non-findings spot-check

Not exhaustively re-derived, but the load-bearing ones were verified
in the course of checking the findings above:

- `replay()` group semantics, `StampBehindClock`, `ClockMismatch`,
  `target = max(tick+1, group AdvanceTo args)` — consistent with
  `drain_at`/`effective_target` semantics; the empirical run confirmed
  the group machinery on real artifacts.
- `advance_to`'s stall predicate (`replay.rs:706-716`) — correct;
  terminates.
- `WorldSnapshot` carries the tunables the report lists
  (`lib.rs:1330`+ fields confirmed: `event_budget`, `vessel_caps`,
  `docking`, `transfer_cfg`, `science_cfg`, `economy_cfg`, `save_seed`,
  `contract_cfg`, `recipes`, `scripts`, `autopilots`; also
  `life_support_cfg` `lib.rs:1364`, `script_op_budget` `1379`,
  `reliability_cfg` `1391`).
- `GoalPredicate::evaluate` honesty (`scenario.rs:191-208`) — missing
  path → `(None, false)`; non-numeric under `Num` → false; `Ne` on
  missing → false. Confirmed.
- `settle_ticks` countdown (`scenario.rs:604-630`) — correct.
- `pending` excluded from snapshot/hash (`lib.rs:1229`) — confirms the
  Finding-6 edge.

## Verification artifacts

- `cargo test -p csp-bench --test zz_verify20 -- --nocapture` (temporary
  file `crates/csp-bench/tests/zz_verify20.rs`, **deleted** after the
  run — `git status` clean except `.review/`).
- Outputs captured above in-line for Findings 1, 2, 3, 5, 6.
