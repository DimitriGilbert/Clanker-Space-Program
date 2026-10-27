# Verified Report C29 — verification of review-report-C29.md

Verifier ran against the current working tree at HEAD `7dd4b86` (tree dirty only under
`.review/`). Empirical: `cargo build -p csp-bench --bin csp-replay` (cached), then
`csp-replay verify` on both goldens — both exit 0. All file:line claims opened and read
with surrounding context; PLAN §5.3/§6.4/§7.6/§7.7/§8.2–§8.5 read in full.

Result: **6 confirmed, 0 dismissed.** Flight-launch golden RESOLVED verdict: **confirmed empirically.**

---

### Finding 1: §5.3 CPU budget rows red in the committed perf log, untriaged, stale baseline — CONFIRMED

**Original summary**: the only committed reference log records `sim.200-part-atmo` 0.672 vs 0.5 and `sim.1000-part-orbit` 4.634 vs 2.0, both `status: Fail`; the CI gate is movement-only so absolute misses gate nothing; `CSP_PERF_STRICT` runs nowhere automated; the baseline predates the growth phase.

**Verification**:
- Rows confirmed at `/home/didi/workspace/Clanker-Space-Program/docs/perf/perf-log-devbox-release.ron:12-28`: `measured_value: Some(0.67194925390625)` vs `budget_value: 0.5` (1.344×) and `Some(4.634418503906249)` vs `2.0` (2.317×), both `status: Fail`. The `detail` fields describe the scene only — no deviation reason, no issue reference. A repo-wide grep of `docs/` finds no triage of either row anywhere.
- Movement-only gate confirmed: `crates/csp-bench/src/bin/csp-perf.rs:116-125` — `verdict_ok = if diff_mode { regressions.is_empty() } else { log.all_passed() }`, and CI always passes `--baseline` (`.github/workflows/ci.yml:92-95`). In `regression_diff` (`crates/csp-bench/src/perf.rs:472-480`) the Pass→Fail rule fires only when `bv <= e.budget_value && mv > e.budget_value`; a baseline row already over budget (bv > budget) can never re-fire. So rows red in the committed log gate nothing, exactly as claimed.
- Strict assertion: `perf.rs:1086-1091` (`strict_budget()`) lives inside the `#[cfg(test)] mod tests` (`perf.rs:1075`) and keys on `CSP_PERF_STRICT` or `CSP_PERF_MACHINE=ref-*`. Grep across the repo: nothing outside `perf.rs` sets `CSP_PERF_STRICT`; `ci.yml` does not set it. Confirmed: absolute-budget assertion runs nowhere automated.
- Staleness confirmed: perf log last touched at `3d56a75` (2026-10-23 13:03). Phase2 `973eb74` is 2026-10-26 16:06 and the fix commits `1096007`…`7dd4b86` run to 2026-10-26 22:56 — all after the baseline. 57 commits since; `git diff 3d56a75..HEAD -- '*.rs'` = 137 files, +58,260/−4,823 (net ≈ +53k — the report's "+14k-LOC growth" context is if anything understated; the direction is right).
- PLAN quotes verified: §5.3 — "The per-tick budgets are what make physics warp honest: ×2 needs 128 ticks/s, so ×2 on the 200-part scene costs 64 ms of CPU per wall second." §8.2 CPU perf gate row — "fail on >10% regression" (movement letter, as claimed). §6.4 cut list sanctions only render tier / life support / third body / docking / web polish — no CPU-budget overrun escape.
- Warp-impact claim checked against the plan's own controller: TICK_HZ = 64, ×2 = 128 ticks/s; §2.6.2's demotion rule `measured_step_ms * TICK_HZ * k / FPS < 0.5 * 16.67 ms` gives 4.634 × 64 × 2 / 60 = 9.90 ms/frame vs the 8.33 ms threshold — the controller demotes ×2 on that scene. "Cannot run 128 ticks/s" is loose phrasing (raw CPU could fit ~593 ms/s on a dedicated thread), but under the plan's own rule the effect is as stated. Not a false positive.

### Finding 2: CI perf gate diffs ci-host fresh vs devbox baseline under a machine-blind 10% band — CONFIRMED

**Original summary**: baseline measured on `devbox`, fresh log on a GitHub runner; on mismatch the bin prints a cross-machine note and applies the same `REGRESSION_TOLERANCE = 0.10`; §5.3 reserves CI-HOST "only for the CPU-only rows", so the CI baseline should be a ci-host log.

**Verification**:
- `.github/workflows/ci.yml:83-95` confirmed: perf job runs `csp-perf --machine ci-host --out /tmp/perf-log-ci-host-release.ron --baseline docs/perf/perf-log-devbox-release.ron`; the baseline log's `machine.name` is `"devbox"` (log line 4).
- `crates/csp-bench/src/bin/csp-perf.rs:139-144` confirmed verbatim: on name mismatch it prints `note: baseline was measured on 'devbox' — the diff is cross-machine` and falls through to `regression_diff` with `REGRESSION_TOLERANCE = 0.10` (`perf.rs:408`). No band widening, no machine-delta model — the note is advisory only.
- PLAN §5.3 confirmed: "**CI-HOST**: GitHub hosted runner, no GPU — used **only** for the CPU-only rows." (The plan does not literally mandate a ci-host-measured baseline — the normative anchor is somewhat analogical — but the engineering flaw is real: a fixed 10% band across heterogeneous machines either permanently reds or hides regressions up to the machine ratio, and the report's suggested fix matches `default_log_path`'s naming scheme.)

### Finding 3: replay.bin has no artifact-level schema version; churn surfaces as generic bincode decode error — CONFIRMED (with a §7.7-applicability nuance)

**Original summary**: `HASH_SCHEMA_VERSION` versions the state-hash stream, not the serde wire shape of `WorldSnapshot`/`SimCommand`; a decode failure maps to exit 2 "harness error" indistinguishable from corruption; `fingerprint.json` records `hash_schema_version: 21` beside the unreadable `replay.bin` and nothing consults it; §7.7 wants refusals "naming the versions".

**Verification**:
- On-disk format confirmed: `crates/csp-bench/src/replay.rs:36-38` — `bincode::config::standard()`, whole-file bincode, no magic/header/version prefix. `Replay` (`replay.rs:160-193`) carries version info only inside `Fingerprint` (`hash_schema_version`/`kdf_version`/`data_hash_version`, `replay.rs:83-88`), which serializes inside the payload — unreadable until decode succeeds. Confirmed.
- Error path confirmed: `decode` (`replay.rs:1020-1030`) returns `ReplayError::Decode`; `csp-replay.rs:35-42` maps every `ReplayError` except `HashMismatch` to exit 2 ("usage or harness error") — a stale artifact is indistinguishable from a corrupt file. The fingerprint-sidecar fallback in `verify_cmd` (`csp-replay.rs:107-118`) runs only on `HashMismatch`, never on decode failure. Confirmed.
- `data/test-fixtures/flight-launch/fingerprint.json` confirmed to record `"hash_schema_version": 21`.
- Nuance, not a dismissal: PLAN §7.6 explicitly says a replay is "**Not a save format**", and replay.rs's own module doc (lines 8-9) says "replays are version-scoped by T3 and carry no migration promise (§7.2, §7.7)". §7.7's "refuses with a clear message naming the versions" is written for the save loader. So this is not a literal §7.7 violation — it is a diagnostic-quality gap, which is exactly how the report frames its impact ("residual gap is diagnostic quality only", "Not silent (CI is loud) but misclassified"). The incident class is real (it happened once; see Finding 6). MEDIUM is defensible for CI-diagnosis ergonomics.

### Finding 4: stall predicate duplicated; colony warp scene runs twice — CONFIRMED

**Original summary**: identical zero-progress predicate in `replay.rs` `advance_to` and `agent.rs` `Lockstep::step`, each with its own error type; `measure_colony_warp_events`/`measure_colony_warp_ms` each call `colony_warp_run()`, doubling the 20-colony warp scene per suite.

**Verification**:
- `replay.rs:925-929` and `agent.rs:348-352`: byte-for-byte the same five-condition predicate (`reached_tick <= before && events_processed == 0 && commands_applied == 0 && rejected.is_empty() && degraded.is_empty()`), diverging only in error type (`ReplayError::AdvanceStalled` vs `SimError::Fatal`). Confirmed.
- `perf.rs:947-958`: both measures call `colony_warp_run()` independently; the scene (spawn + warmup + 8 release frames per `perf.rs:927`) executes twice per suite, yielding two noisy samples of the same frames. Confirmed.

### Finding 5: `found_authored_colonies` error laundered into "aero-table build failed" — CONFIRMED

**Original summary**: `perf.rs:590-595` maps the colony-founding error via `|_|` to `ReplayError::MalformedGolden("perf: aero-table build failed")` — wrong subsystem, real cause discarded.

**Verification**:
- `perf.rs:590-595` confirmed verbatim. `World::found_authored_colonies` (`crates/csp-sim/src/colonies.rs:1537`) applies `ColonyDef`s through `apply_found_colony` and returns `Result<(), SimError>` — colony founding, nothing aero-table-specific in its contract; the `|_|` discards the `SimError`. The surrounding `load_*` calls propagate via `ReplayError::Load`, so the asymmetry is real. Confirmed.

### Finding 6: stale "Known issue" claim in the data doc — CONFIRMED

**Original summary**: `docs/DATA-AND-SCRIPTING.md:377-379` still says the flight-launch golden fails at the bincode decode step; it passes outright in the current tree.

**Verification**:
- `docs/DATA-AND-SCRIPTING.md:377-379` confirmed to read: "the `flight-launch` golden currently fails at the bincode **decode** step against the uncommitted W2.6 serialized-type churn — passes at HEAD". Empirically (this verification session): `./target/debug/csp-replay verify data/test-fixtures/flight-launch` → `OK (ee94b6cf…)` exit 0, and `trivial` → `OK (8120a1bb…)` exit 0. The doc line is stale. Confirmed.

---

## Flight-launch golden RESOLVED verdict — CONFIRMED (empirically reproduced)

- `cargo build -p csp-bench --bin csp-replay` then `csp-replay list` → exactly `trivial`, `flight-launch`.
- `csp-replay verify data/test-fixtures/trivial` → exit 0, hash `8120a1bbd2a0af1a810a934d0104d7f4de62e270dd684ee5416354ecaba2993c` — matches report.
- `csp-replay verify data/test-fixtures/flight-launch` → exit 0, hash `ee94b6cf5567d8aa4047fb71903110552a8d2a6a8ddf84e1e0d1118e74dbf337` — matches report.
- Fixture dirs are clean in the working tree (dirty files are only under `.review/`). Last golden-touching commit is `973eb74` (2026-10-26 16:06, "Phase2 player surface"; prior touch `a02e496` "rebless for Phase1 (schema 21…)"). `fingerprint.json` records `hash_schema_version: 21`. The four later fix commits left the goldens untouched and they still verify.
- The documented failure (Finding 6) was against the pre-`973eb74` uncommitted W2.6 tree and no longer reproduces. **RESOLVED verdict stands.**

## PRD coverage rows — verification

| Row | Verdict on the report's claim | Evidence |
|---|---|---|
| §7.6 replay artifact WIRED (superset) | **Confirmed** | `final_tick` pins the end (`replay.rs:177-182`, `ClockMismatch` check in `replay`), `config` field at `:186-192`; both goldens verify exit 0 |
| §8.3 fixture freeze + exclusion + canonical hashing WIRED | **Confirmed** | `FixtureDrift` hard error `replay.rs:997-1003`; top-level-only artifact exclusion `:618`; mirror test `fixture_walk_mirrors_data_set_hash` (~:1760) pins byte-identity with `csp_data::data_set_hash` |
| §8.4 bless-goldens WIRED — exceeds PRD | **Confirmed** | `xtask/src/main.rs`: dirty-tree refusal (:92-106), orphan-golden refusal (:121-145), partial-set refusal (:172-191), hash-change-without-bump/`--reason` refusal (:211-231). PRD asked for regenerate + dirty-tree + hash-change + diff summary; orphan/partial refusals exceed it. Lib-side anti-laundering confirmed: `replay::write_golden` refuses a hash-changing overwrite (`GoldenOccupied`, `replay.rs:1055-1073`), inherited by `csp-replay record` |
| §8.2 golden replays gate in CI WIRED | **Confirmed** | `ci.yml:72-77` loops `csp-replay list` → verify per-commit |
| §8.2 golden menu PARTIAL | **Confirmed** | Registry is exactly 2 (empirical `csp-replay list`); flight-launch covers launch→orbit→reentry→land per `scenarios.rs:89-94`; ×1-vs-×10⁵ equivalence is the sim test `warp_x1_vs_x1e5_equivalence_10_sim_days` (`csp-sim/src/lib.rs:13135`); docking-merge and warp-jump goldens absent from the §8.2 menu ("ascent, reentry, landing+settle, docking merge, ×10⁵ warp jump, ×1-vs-×10⁵ equivalence"). Docking sim machinery (`DockSeam`/`LiveJoint`/`DockingConfig`) exists in csp-sim, consistent with the "authorable" parenthetical |
| §8.5 Sim e2e WIRED | **Confirmed** | 5 committed scenarios in `data/eval-scenarios/` (idle-warp, lease-launch, ascent-mission, script-ascent, log-replay); `crates/csp-bench/tests/eval_suite.rs` iterates the committed set (suite not re-run by verifier; nothing contradicts the report's green run) |
| §8.5 App smoke e2e MISSING as specified | **Confirmed** | Grep for `smoke` / `--frames` / `--snapshot` across all Rust sources: zero hits. PLAN §8.5 literally specifies `csp --smoke --frames N --snapshot out.png` (nightly on CI-HOST lavapipe, winit+wGPU+frame-loop integration). Nearest realization is `crates/csp-view/tests/render_gate.rs` (exists) — render-level, not the app frame-loop integration. Also note ci.yml has no nightly job at all, so the §8.5 nightly tier (smoke, interface e2e) has no scheduling surface |
| §5.3 CPU perf budgets DEGRADED | **Confirmed** | 9 measurable CPU rows, 2 Fail (Finding 1); movement gate cross-machine (Finding 2); absolute assertion nowhere automated |
| §5.3 fps rows WIRED (honest) | **Confirmed** | All 9 fps/wasm rows `HardwareGated` with reasons in `detail` ("requires a GPU — REF-A / REF-B reference-machine gate (§8.2)") — no fake numbers in the committed log |
| §8.3 economy-harness note | **Not deep-verified** (out of the required verification list; no contradiction found) | — |

## Notes for the coordinator

- Two wording precision points, neither a false positive: (1) Finding 1's "×2 warp … cannot run 128 ticks/s" is strictly the plan's own demotion rule refusing ×2 (9.90 ms/frame > 8.33 ms threshold), not raw-CPU impossibility (~593 ms/s would fit a dedicated thread); (2) Finding 3's §7.7 anchor is analogical — §7.7 governs saves and §7.6 explicitly exempts replays from the migration promise; the report's own impact framing ("diagnostic quality only") is the accurate reading, and the facts (no header, exit-2 launder, unconsulted sidecar) are all verified true.
- Finding 1's "+14k-LOC" context is understated: the actual delta since the baseline commit is +58k/−4.8k lines across 137 Rust files, which strengthens the stale-baseline claim.
