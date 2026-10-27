# Review Report C29 — Bench: replay/perf/agent/eval

Session full-20261026. Reviewed against the CURRENT working tree (HEAD 7dd4b86, clean).
Files: `crates/csp-bench/src/{replay,perf,agent,eval,lib,build}.rs`, `src/bin/{csp-replay,csp-perf,csp-eval}.rs`.
Everything below was verified empirically where possible: both goldens re-verified via
`csp-replay verify` (exit 0), full `cargo test -p csp-bench` run (53 unit + 12 eval-suite tests, all green),
bless-goldens refusal chain read in `xtask/src/main.rs`, CI wiring read in `.github/workflows/ci.yml`.

## Flight-launch golden: known-issue verdict (focus item 5)

**RESOLVED — PASSES in the current tree.** `csp-replay verify data/test-fixtures/flight-launch` →
exit 0, hash `ee94b6cf…`; `trivial` → exit 0, hash `8120a1bb…`. The fixture artifacts are committed
clean (no working-tree diff); last golden-touching commit is `973eb74` ("rebless" line of Phase2,
schema 21 per `fingerprint.json`). The four later fix commits (`1096007`…`7dd4b86`) left the goldens
untouched and they still verify. The failure documented in `docs/DATA-AND-SCRIPTING.md` §2.2 was
against the pre-`973eb74` uncommitted W2.6 tree and no longer reproduces — that doc line is now stale
(see Finding 6).

Root-cause class: serde-shape churn in `WorldSnapshot`/`SimCommand` with **no artifact-level version
stamp** in `replay.bin` — an old artifact fails as a generic bincode `Decode` error (exit 2 "harness
error"), not a "golden predates current schema — re-bless" verdict (Finding 3). Guards that already
prevent silent recurrence: the per-commit CI loop (`ci.yml` golden step verifies every scenario from
`csp-replay list` — loud red) and the §8.4 bless refusal chain (hash change without `HASH_SCHEMA_VERSION`
bump or `--reason` is refused; verified in `xtask/src/main.rs:210-232`). The residual gap is diagnostic
quality only.

---

### [HIGH] Finding 1: §5.3 CPU budget rows are red in the committed perf log, untriaged, and the baseline predates the +14k-LOC growth
**File**: `docs/perf/perf-log-devbox-release.ron` (committed at `3d56a75`, 2026-10-23) — **Problem**: the only committed reference log records `sim.200-part-atmo` measured 0.672 vs budget 0.5 ms/tick (1.34×, `status: Fail`) and `sim.1000-part-orbit` measured 4.634 vs budget 2.0 ms/tick (2.32×, `status: Fail`). The CI gate is movement-only by design (`csp-perf --baseline` diffs fresh-vs-committed; rows already red never fail it — `perf.rs:440-485` + `csp-perf.rs:96-125`), which matches §8.2's ">10% regression" letter, so these absolute misses gate nothing anywhere: the strict absolute assertion only runs under `CSP_PERF_STRICT`/`ref-*` (`perf.rs:1086-1091`), which no automation sets. Meanwhile the baseline is 3 days stale — it predates Phase2 (973eb74), the four sim-fix commits, and the W2.6 pass; the current-tree numbers on the reference machine are simply unknown. **Impact**: §5.3 is a normative budget table ("the per-tick budgets are what make physics warp honest"); a 2.3× overrun on the 1000-part row means ×2 warp on that scene cannot run 128 ticks/s, and nothing in the repo tracks or triages the miss — the growth phase most likely to have moved these numbers is exactly the one with no fresh measurement. No §6.4 cut-list line sanctions CPU-budget overruns (§6.4 cuts render tiers/life support/third body/web polish only). **Suggestion**: re-run `csp-perf` on the devbox at current HEAD, commit the fresh log, and either fix the two red rows or record the deviation with a reason in the row `detail`/a tracked issue so the next regression diff starts from an honest, current baseline.

### [MEDIUM] Finding 2: CI's perf regression gate diffs a ci-host run against a devbox-committed baseline — the 10% band does not model the machine delta
**File**: `.github/workflows/ci.yml:79-93` (perf job: `--machine ci-host --baseline docs/perf/perf-log-devbox-release.ron`); `crates/csp-bench/src/bin/csp-perf.rs:139-144` — **Problem**: the baseline was measured on `devbox`; the fresh log is measured on a GitHub runner. On a machine-name mismatch the bin prints `"note: baseline was measured on 'devbox' — the diff is cross-machine"` and proceeds to apply `REGRESSION_TOLERANCE = 0.10` (`perf.rs:408`) as if the machines were identical. **Impact**: single-threaded sim-tick numbers between a dev desktop and a shared CI runner routinely differ by more than 10% in either direction. If the runner is slower, every row flags and the job is permanently red (a gate nobody keeps gets disabled); if faster, real regressions up to the machine ratio hide under the band. §5.3/§8.2 explicitly reserve CI-HOST "only for the CPU-only rows" — the committed baseline for a CI gate should be a ci-host log, not a devbox one. **Suggestion**: bootstrap and commit a `perf-log-ci-host-release.ron` as the CI baseline (keep the devbox log for reference-machine tracking); the `default_log_path` naming already supports both.

### [MEDIUM] Finding 3: replay.bin carries no artifact-level schema version — snapshot serde churn decodes as an opaque bincode error, not a version-naming refusal
**File**: `crates/csp-bench/src/replay.rs:30-38` (fixed bincode config, no header), `:160-193` (`Replay` — the only version fields are `fingerprint.hash_schema_version`/`kdf_version`/`data_hash_version`, which ride *inside* the bincode payload, i.e. are unreadable until decode succeeds) — **Problem**: `HASH_SCHEMA_VERSION` (currently 21) versions the *state-hash stream*, not the serde wire shape of `WorldSnapshot`/`SimCommand`. A field addition to those types breaks decode of every existing golden without touching `HASH_SCHEMA_VERSION`, and `decode` (`replay.rs:1020-1030`) reports it as `ReplayError::Decode(bincode…)`, which the bin maps to exit 2 "harness error" — indistinguishable from a corrupt file. This is exactly the flight-launch incident class: the doc's only guidance was "bincode decode fails; run bless-goldens". §7.7's policy is that invalidation is fine pre-1.0 but the refusal must come "with a clear message naming the versions". The human-readable `fingerprint.json` sidecar sits right beside the unreadable `replay.bin` and already records `hash_schema_version: 21` — nothing consults it when the binary decode fails. **Impact**: next churn event produces a CI red whose message names neither the recorded nor the current version; an operator must know to open `fingerprint.json` by hand. Not silent (CI is loud) but misclassified and under-diagnosed. **Suggestion**: either prefix `replay.bin` with a small fixed header (magic + u32 artifact version) checked before bincode decode, or — cheaper — on `Decode` failure in `load_golden`, fall back to parsing `fingerprint.json` and, if it parses, emit an error like "golden recorded under hash_schema_version 20 / current 21 — re-bless via `xtask bless-goldens`".

### [NIT] Finding 4: zero-progress stall predicate duplicated across the harness; colony warp scene executed twice
**File**: `crates/csp-bench/src/replay.rs:925-935` (`advance_to`) vs `crates/csp-bench/src/agent.rs:348-358` (`Lockstep::step` partial loop) — identical four-condition "reached nothing, fired nothing, applied nothing, rejected/degraded nothing ⇒ stall" predicate on `AdvanceReport`, each with its own error type. A single `AdvanceReport::made_progress_since(tick)` helper in csp-sim would collapse them and keep the §2.6.5 budget contract defined once. Also `perf.rs:947-958`: `measure_colony_warp_events` and `measure_colony_warp_ms` each call `colony_warp_run()` — the 20-colony warp scene (spawn + warmup + 8 release frames) runs twice per perf suite, producing two independently-noisy samples of one scene; one run returning both numbers halves suite time and makes the two rows describe the same frames.

### [NIT] Finding 5: `found_authored_colonies` error laundered into a misleading message
**File**: `crates/csp-bench/src/perf.rs:590-595` — **Problem**: `w.found_authored_colonies(…).map_err(|_| ReplayError::MalformedGolden("perf: aero-table build failed"))` — founding the authored colonies has nothing to do with aero tables, and the `|_|` discards the real cause (bad colonies.ron, duplicate site, …). A red perf row here would send the reader to the wrong subsystem. Pass the underlying error through (as the surrounding `load_*` calls already do via `ReplayError::Load`).

### [NIT] Finding 6: stale "Known issue" claim in the data doc
**File**: `docs/DATA-AND-SCRIPTING.md:377-379` — still says the `flight-launch` golden "currently fails at the bincode decode step against the uncommitted W2.6 serialized-type churn — passes at HEAD". In the current tree it passes outright (verified exit 0; reblessed at `973eb74`). One-line correction so the next reader doesn't chase a fixed problem.

---

## PRD coverage table

| PRD item | Status | Evidence |
|---|---|---|
| §7.6 replay artifact (shape, bincode, not-a-save) | **WIRED** (superset: `final_tick` pins the end, `config` makes artifacts self-contained) | `replay.rs:160-193`; both committed goldens verify exit 0 |
| §8.3 fixture freeze + artifact exclusion + canonical hashing | **WIRED** | `FixtureDrift` hard error (`replay.rs:997-1003`); artifact exclusion top-level only (`:618`); byte-identical mirror of `csp_data::data_set_hash` pinned by test (`:1762-1771`) |
| §8.4 bless-goldens (refuses dirty tree / hash change without schema bump or reason; diff summary) | **WIRED — exceeds PRD** | `xtask/src/main.rs:36-232`: staged no-partial-write flow, orphan-golden refusal, partial-set refusal, anti-laundering also enforced in lib `write_golden` so `csp-replay record` inherits it |
| §8.2 golden replays gate in CI | **WIRED** | `ci.yml:66-77` verifies every scenario from `csp-replay list` per-commit; `every_registered_scenario_has_a_verified_golden` pins the registry count in-process |
| §8.2 golden *menu* (ascent, reentry, landing+settle, docking merge, ×10⁵ warp jump, ×1-vs-×10⁵ equivalence) | **PARTIAL** | Registry = 2 (`trivial`, `flight-launch`); flight-launch covers launch→orbit→reentry→land in one golden; ×1-vs-×10⁵ equivalence exists as a sim test (`csp-sim/src/lib.rs:13135`) not a golden; docking-merge, warp-jump goldens and the nightly full set (colony century, pathological event density) absent — docking sim is complete (opt-out is UI-only), so the golden is authorable |
| §8.5 Sim e2e (scenario.ron, goal predicates, budgets, exit code + hash + score) | **WIRED** | `scenario.rs` + `eval.rs:214-237` (empty-dir refusal, sorted, additive-by-file); 5 committed scenarios all pass per-commit via `tests/eval_suite.rs`; `csp-eval` bins + transcript JSON |
| §8.5 App smoke e2e (`csp --smoke --frames N --snapshot out.png`) | **MISSING as specified** | no `--smoke`/`--frames` flag anywhere in `csp-app`; nearest realization is the lavapipe render-gate tests (`csp-view/tests/render_gate.rs`) which cover render, not the winit+wGPU+frame-loop integration the PRD row names |
| §5.3/§8.2 CPU perf budgets | **DEGRADED** | all 9 CPU rows measured honestly with named scenes and warmup; 2 of 9 FAIL in the committed log (Finding 1); movement gate wired in CI but cross-machine (Finding 2); absolute-budget assertion runs nowhere automated |
| §5.3 fps rows / wasm row | **WIRED (honest)** | recorded `HardwareGated` with reasons, never fake numbers; coverage test pins row counts |
| §8.3 economy harness (C08 consequence, not re-derived) | **note** | harness self-solves its salary rate, so the 2–5%/sim-month band cannot catch authored balance drift in `balance.ron`; combined with §8.3's fixture freeze (shipping data never touches goldens by design), balance regressions are guarded only by the cheaper "validates + 1000 ticks no NaN/Fatal" shipping gate |

## Top 3 issues

1. **Finding 1 (HIGH)** — two §5.3 CPU budget rows red and untriaged in the committed perf log; baseline stale through the largest growth phase; absolute budgets enforced nowhere automated.
2. **Finding 2 (MEDIUM)** — CI perf gate diffs ci-host fresh vs devbox baseline under a 10% band that ignores the machine delta; baseline should be a ci-host log.
3. **Finding 3 (MEDIUM)** — no artifact-level version in `replay.bin`; schema churn surfaces as an opaque bincode decode error instead of §7.7's "refuses naming the versions" (the flight-launch incident class).
