# Verified Report 18 — csp-bench (replay/perf/eval harness)
# Verifier: verification agent, ruthless-20261025 | Date: 2026-10-25
# Verified against current HEAD (1e8c6da) + working tree. Concurrent-commit audit:
# only d8cd46f (HASH_SCHEMA_VERSION 19->20, goldens re-blessed, perf.rs +1 line) and
# 5a0115a (xtask Phase 16, unrelated CLI fixes) touched the report's scope after the
# review window. Neither addresses any finding below.

### Finding 1: flight-launch golden committed but never verified by anything — CONFIRMED
**Original**: The only golden CI verifies is `trivial`; the flight-launch artifact set is dead weight that looks like coverage.
**Verification**:
- CI: `.github/workflows/ci.yml:57-58` — the sole golden step is `cargo run -p csp-bench --bin csp-replay -- verify data/test-fixtures/trivial`. Single job, no other step invokes `csp-replay` or `csp-perf`.
- Artifacts exist: `data/test-fixtures/flight-launch/` holds `replay.bin` (22,255 B), `expected-hash.txt`, `fingerprint.json` (mtime 2026-10-24 23:46 — re-blessed by d8cd46f's schema 19->20 bump, which *strengthens* the finding: the golden was regenerated and still nothing verifies it).
- `verify_golden` call sites (repo-wide grep): `crates/csp-bench/src/bin/csp-replay.rs:84` (CLI — CI feeds it only `trivial`) and `crates/csp-bench/src/replay.rs` tests at :1128 (`golden_trivial_round_trips`, uses `trivial_dir()`), :1398/:1407/:1421/:1433 (`verify_golden_enforces_fixture_freeze`, temp-dir copies of `trivial`). No test or bin verifies `flight-launch`.
- `crates/csp-bench/src/scenarios.rs:648-649` — `flight_arc_launch_orbit_reentry_land` builds a fresh world via `flight_initial(&fixture_dir())`; it never loads `replay.bin`/`expected-hash.txt`.
- Other `flight-launch` consumers use the fixture *data files*, not the golden artifacts: `crates/csp-server/tests/e2e.rs:586-590, 995-999` (`(recipe.initial)(&dir)` fresh worlds), `crates/csp-bench/tests/eval_suite.rs:378-411` (csp-mcp spawn), `crates/csp-bench/src/perf.rs:443` (perf scene), `crates/csp-data/tests/unknown_fields.rs:185` (`sites.ron` only). Grep for `replay.bin|expected-hash|fingerprint.json` shows no reader of the flight-launch artifact set anywhere.
- `xtask bless-goldens` is a writer (with §8.4 refusals) and is not run by CI.
Corroborates verify-C16's CI-side confirmation (ci.yml:58 + replay.rs:1080).

### Finding 2: perf budgets numerically unenforced (x400 debug, x4 release, strict only manual; no nightly; FAIL rows with no consequence) — CONFIRMED
**Original**: The only automated budget assertion CI runs is debug `bound = budget * 400`; no nightly workflow exists; the committed perf log already has 2 FAIL rows.
**Verification**:
- `crates/csp-bench/src/perf.rs:992-1002` — `bound()`: `if strict { row.budget } else if cfg!(debug_assertions) { row.budget * 400.0 } else { row.budget * 4.0 }`; `strict` requires env `CSP_PERF_MACHINE=ref-*` (never set by CI).
- CI runs the perf tests only via `cargo test --workspace` (ci.yml:27-28, debug) — effective bound for `sim.200-part-atmo` is 200 ms/tick vs the written 0.5 ms budget.
- No nightly: `.github/workflows/` contains exactly one file, `ci.yml`. Repo-wide grep for "nightly" hits only perf.rs:1054's own doc comment ("the CI-HOST gate the nightly job asserts") — the claimed job does not exist.
- No regression gate: grep for `baseline|regression` in perf.rs hits only a doc comment (:987). `run_perf_log` (perf.rs:336-383) measures fresh and returns; `csp-perf` bin writes the log and exits — neither diffs against the last committed log. No CI step reads `docs/perf/*.ron`.
- FAIL rows confirmed in `docs/perf/perf-log-devbox-release.ron`: `sim.200-part-atmo` measured 0.672 > budget 0.5 (status: Fail) and `sim.1000-part-orbit` measured 4.634 > budget 2.0 (status: Fail) — with nothing anywhere treating them as a failure.

### Finding 3: lockstep transcript ticks off-by-one vs command-log stamps — CONFIRMED
**Original**: Transcript entries are labeled `clock + 1` while the commands they carry are stamped at the pre-advance clock N in the §7.6 command log.
**Verification**:
- `crates/csp-bench/src/agent.rs:276` — `let tick = self.world.clock().ticks().saturating_add(1);` (clock = N at poll time).
- `agent.rs:339` — the agent's pending commands are enqueued *before* `step_frame`; `agent.rs:341` — `self.world.step_frame(tick)?` (advance N -> N+1 or beyond).
- `crates/csp-sim/src/lib.rs:6528-6541` — `drain_boundary` calls `drain_at(self.clock.ticks(), ...)`, and inside `step_frame` (lib.rs:7503-7524) the drain runs before the clock advances — so commands an agent enqueues at poll time land in the command log stamped N.
- `agent.rs:361` — the code itself passes the pre-drain boundary: `self.accumulate_dv(tick - 1, report.reached_tick)`; `agent.rs:362` — `record_rejections(tick, ...)` labels rejections N+1.
- `agent.rs:103` — the field doc "The sim tick the boundary drained at (`clock + 1` at poll time)" is wrong about the drain tick (the drain happens at N).
Net: correlating a transcript against a csp-replay command log requires an undocumented +1 shift; determinism tests can't see it because both runs carry the same label.

### Finding 4: fingerprint omits rhai — CONFIRMED
**Original**: TRACKED_DEPS lists parry/rapier/libm/rand_chacha/blake3 but not rhai, so a rhai bump reds goldens with an empty fingerprint diff.
**Verification**:
- `crates/csp-bench/build.rs:19-25` — `TRACKED_DEPS = &[("parry3d-f64",..),("rapier3d-f64",..),("libm",..),("rand_chacha",..),("blake3",..)]`; no rhai entry.
- `crates/csp-bench/src/replay.rs:58-87` — `Fingerprint` (and `FingerprintSidecar`) carry only those five dep versions + schema versions + fixture hash; no rhai field.
- rhai is squarely in the run's determinism domain: `crates/csp-bench/Cargo.toml` depends on `csp-script` (which links rhai); the replay path installs it — `replay.rs:757`, `scenarios.rs:180` (`world.set_script_engine(Box::new(csp_script::RhaiHost::new())`); script persist values hash (`crates/csp-sim/src/scripts.rs:169` — `Float(f64)` hashed via `h.f64`) and script-issued commands change state, so a rhai float-evaluation change moves golden hashes while `fingerprint_warnings` shows an empty diff.
Nuance (does not dismiss): rhai is exactly-pinned (`=1.27.0`, workspace Cargo.toml:45), so a bump is a deliberate manifest edit rather than resolver drift — but when it happens the sidecar still cannot explain the red, which is the failure mode §7.1 built the fingerprint for. `scripts.rs`'s module doc ("keeps rhai out of the determinism crate's tree — the engine is never hashed") is about serialized state, not evaluation outputs, which do hash.

### Finding 5: dv_used_mps integrates post-drain throttle across a whole multi-tick advance — CONFIRMED
**Original**: `accumulate_dv(from, to)` charges final throttle * thrust/mass * dt over the whole span; mid-span throttle changes (scripts/autopilots) are mis-charged.
**Verification**:
- `crates/csp-bench/src/agent.rs:417-437` — `accumulate_dv` reads `body.throttle`/`body.mass_kg` once, post-drain, and integrates `throttle * (thrust_n / mass_kg) * dt` over `[from, to)`. Its own doc admits "Reads the *post-drain* throttle".
- `agent.rs:361` — called once per boundary with `to = report.reached_tick`, which for an agent-issued `AdvanceTo` spans thousands of ticks: `step()` (agent.rs:275-363) advances to the raised target and re-issues partials (`agent.rs:341-358`) without re-polling or re-reading throttle; scripts run per tick inside `step_bubble_range` (csp-sim lib.rs:7540-7566) and their `set_throttle` drains mid-span at inner boundaries (`drain_at` per scripts.rs: "commands a script issues are stamped at the producing tick + 1").
- So a script burn-then-cut inside one long agent advance is charged the cut throttle for the whole span, and a mid-span ignition is charged from the start.
- Committed-suite scoping claim holds: `data/eval-scenarios/script-ascent.ron` advances 1 tick per boundary (WaitAgent, no AdvanceTo — `Lockstep::step` targets `clock+1`); `lease-launch.ron` holds throttle 1.0 constant across its `AdvanceTo(1280)` span; `idle-warp.ron`/`log-replay.ron` set no throttle. No committed scenario hits the mis-integration, but the API silently mis-scores any that pairs a Script/autopilot with long advances — and `dv_used_mps` is a score axis (`scenario.rs` EvalScore).

### Finding 6: zero-goal scenario passes vacuously — CONFIRMED (empirically)
**Original**: `goals` is `#[serde(default)]` with no minimum in `validate()`; `pass = goals.iter().all(..)` is trivially true; `csp-eval --suite` exits 0.
**Verification**:
- `crates/csp-bench/src/scenario.rs:360-361` — `#[serde(default)] pub goals: Vec<GoalPredicate>`; `:396-400` — `validate()` checks only `goals.len() > MAX_GOALS`, no minimum.
- `scenario.rs:699` — `let pass = goals.iter().all(|g| g.passed) && agent_faults.is_empty();` — vacuously true on empty goals.
- Suite is additive-by-file: `eval.rs run_suite` picks up every `*.ron`; the only anti-vacuous guard is `tests/eval_suite.rs:73-74` pinning `ascent-mission.ron`'s existence (one of five committed files).
- `bin/csp-eval.rs:88-107` — `all_pass &= report.pass` -> exit 0 for a zero-goal member.
- EMPIRICAL REPRO (current tree, debug binary built after all bench sources): a file `(schema_version: 1, name: "vacuous", world: Empty, agent: Idle, tick_budget: 2, event_budget: 2)` through `./target/debug/csp-eval` prints `"pass": true, "goals": []` and exits 0. Confirmed exactly as reported.

### Finding 7: `csp-replay record` bypasses every §8.4 anti-laundering refusal — CONFIRMED
**Original**: The shipped `record` subcommand writes all three artifacts with no hash-change comparison, no `--reason`, no dirty-tree check — the refusal chain lives only in `xtask bless-goldens`.
**Verification**:
- `crates/csp-bench/src/bin/csp-replay.rs:51-68` — `record`: `let golden = replay::record(recipe, dir)?; replay::write_golden(dir, &golden)?;` — no comparison against the existing `expected-hash.txt`, no reason flag.
- `replay.rs:969-985` — `write_golden` is an unconditional writer (create_dir_all + three file writes); no refusal logic exists in the library, so neither writer inherits one.
- Contrast, `xtask/src/main.rs`: `refuse_on_dirty_tree` (:78/:95), partial-artifact refusal (:107-180), and the hash-change-without-`HASH_SCHEMA_VERSION`-bump refusal (:58-61, overridable only by `--reason`) — all in xtask, none in the bin most likely to be run by hand. `record`'s self-check proves the new artifact reproduces itself, not that the hash didn't change vs the committed golden. After a `record`-overwrite the three files are freshly consistent, so `verify_golden`'s doctored-artifact checks cannot catch it.

### Finding 8: perf scenes build the fixture world twice per measurement — CONFIRMED
**Original**: `measure_atmo_200`/`measure_orbit_1000`/`measure_landed_300` call `fixture_world()` for the `Environment` clone, then `measure_tick_scene` rebuilds it from scratch.
**Verification**:
- `crates/csp-bench/src/perf.rs:644-657` (`measure_atmo_200`), `:661-675` (`measure_orbit_1000`), `:683-694` (`measure_landed_300`) — each starts `let w = fixture_world()?; let env = w.environment().cloned()...` then calls `measure_tick_scene`.
- `perf.rs:578-583` — `measure_tick_scene` starts `let mut w = fixture_world()?;` — the full recipe-driven world construction (system + balance + physics wiring) runs twice per row; only the cloned `Environment` survives from the first build.
- Waste, not wrongness (both constructions are identical within a run) — matches the original LOW severity.

## Summary
8 confirmed, 0 dismissed, 0 fixed-by-concurrent-edit.
Concurrent-commit audit: since the review window only d8cd46f (schema 19->20 + golden rebless + 1 perf.rs line) and 5a0115a (xtask Phase 16 fixes, incl. an unrelated `rhai_feature_set` fix that does NOT touch the fingerprint's TRACKED_DEPS) changed the report's scope — neither addresses any finding.
Strongest findings stand as HIGH: the flight-launch golden (re-blessed for the schema bump!) is still verified by nothing (F1), and every automated perf assertion runs at >=4x (400x in CI's debug profile) of the written budget while the committed log already carries two FAIL rows with no consequence (F2).
