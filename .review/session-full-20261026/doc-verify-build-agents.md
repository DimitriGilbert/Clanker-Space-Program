# Doc verification — BUILD-AND-DEV.md + AGENTS.md (session-full-20261026)

Verifier: independent re-check against the working tree at `main` (7dd4b86, dirty
with `.review/` artifacts + untracked `crates/csp-view/tests/tmp_pick_probe.rs`).
Method: every command and factual claim traced to a manifest, script, or source
file; test counts are static `rg` counts of `#[test]`/`#[tokio::test…)]`
attributes (no test runs). No files were edited except this report.

## Summary

| Doc | Verdict | Result | Claims checked | Exact | Inaccurate | Within-hedge drift |
|---|---|---|---:|---:|---:|---:|
| `docs/BUILD-AND-DEV.md` | MINOR-INACCURATE | **PASS-WITH-FIXES** | 75 | 72 | 2 | 1 |
| `AGENTS.md` | MINOR-INACCURATE | **PASS-WITH-FIXES** | 40 | 37 | 2 | 1 (policy, not statically checkable) |

Both docs are substantially truthful — every command form, manifest fact, CI
step, and lint gate I traced resolves as written. The two failures per doc are
listed first in each table.

---

## 1. docs/BUILD-AND-DEV.md

### Inaccuracies (fix these)

| # | Severity | Location | Claim | Reality (proof) |
|---|---|---|---|---|
| B1 | [warning] | BUILD-AND-DEV.md:97 | `cargo test -p csp-sim advance_to_equivalence` as the "filter by test-name substring" example | No such test exists anywhere in the repo (`rg` over `crates/ xtask/ PLAN.md` finds the string only in this doc). The real §2.6.1 test is `warp_x1_vs_x1e5_equivalence_10_sim_days` (`crates/csp-sim/src/lib.rs:13135`). Worse than a doc typo: the command *succeeds* while running 0 tests, silently teaching nothing. Fix: use the real test name. |
| B2 | [nit] | BUILD-AND-DEV.md:82 | csp-view integration count "28"; file list omits `tmp_pick_probe.rs` | Static count is **29** tracked (30 incl. the untracked scratch `crates/csp-view/tests/tmp_pick_probe.rs`, 1 test). The doc says "treat as ~", but the layout-notes list also names every other file, so the omission is a real (if tiny) gap. |

Within-hedge drift (acceptable under the doc's own "~" caveat): `data_root()`
cited "~line 4034", actual `crates/csp-view/src/lib.rs:4055` (fn) / 4050 (doc
comment) — behavior described is exactly right (`./data` first, then
manifest-relative, `balance.ron` probe, final fallback `"data"`).

### Verified claims (selected proof per cluster)

| Claim (§) | Result | Proof |
|---|---|---|
| Toolchain 1.97.1 + clippy/rustfmt + wasm32 target (§1) | PASS | `rust-toolchain.toml` |
| edition 2024, `rust-version = "1.95"`, 13 members = 12 crates + xtask (§1) | PASS | `Cargo.toml [workspace.package]/[workspace]`; `ls crates/` = 12 |
| No `[features]` in any manifest (§1) | PASS | `rg '\[features\]'` over all 13 manifests: zero |
| `csp` bin + `cdylib`; csp-server dep only under `cfg(not(wasm32))` (§1–2) | PASS | `crates/csp-app/Cargo.toml` `[[bin]] name="csp"`, `[lib] crate-type`, target-gated dep |
| csp-server/csp-bench/xtask native-only trio; wasm exclusion set (§1) | PASS | xtask deps csp-bench; csp-bench deps csp-server (axum/tokio); ci.yml:47-48 same 3 excludes |
| `./data` resolution + "data/ startup load failed" abort (§2) | PASS | `csp-view/src/lib.rs:4050-4064`; strings at `csp-app/src/lib.rs:405,2983`, `csp-view/src/lib.rs:1694` |
| Release profile `lto="thin"`, `codegen-units=1`; committed log `profile: "release"` (§2) | PASS | `Cargo.toml [profile.release]`; `docs/perf/perf-log-devbox-release.ron:7` |
| `--serve` / `--bug-report` entry points (§2) | PASS | `csp-app/src/main.rs:7,16` |
| Settings under `$XDG_CONFIG_HOME/csp` (§2) | PASS | `settings_io.rs:35-56` |
| Test-count table (§3) | PASS except B2 | static counts: csp-sim 363, csp-phys 157/14, csp-data 81/40, csp-view 60/**29-30**, csp-app 51, csp-bench 53/12, csp-ui 43/2, csp-core 42, csp-server 31/**22**, csp-script 13/3, csp-render 5, csp-ai 0, xtask 14 — all others exact |
| csp-data/csp-phys test-file lists (§3) | PASS | directory listings match |
| `--test render_gate dump_sky -- --ignored` (§3) | PASS | `render_gate.rs:515-519` documents that exact invocation itself |
| SKIP + `CSP_RENDER_GATE_REQUIRED=1`; lavapipe `ci.yml lines 30-40` (§3) | PASS | `render_gate.rs:81-102`; ci.yml:30-40 |
| Fixtures = trivial + flight-launch, 3 artifacts each (§3) | PASS | `ls data/test-fixtures/*`; `scenarios.rs:50-58` registry |
| `every_registered_scenario_has_a_verified_golden` ~line 1257 (§3) | PASS | exact: `crates/csp-bench/src/replay.rs:1257` |
| Goldens insulated from `data/` edits (§3, §8) | PASS | `csp-data/src/datahash.rs:188-213` excludes `test-fixtures/**` from `data_set_hash` |
| e2e = 22 `#[tokio::test(flavor="multi_thread")]` with the listed subjects; dev-deps tower + tokio-tungstenite (§3) | PASS | `crates/csp-server/tests/e2e.rs` — 22 fns; `0600` (l.314), origin/host (l.400/428), mcp+ordering (l.445), backpressure (l.530), lease (l.582), ws frames (l.721), cmd-vs-local hash (l.657); `csp-server/Cargo.toml:48,50` |
| Eval suite: 5 scenarios, spawns `csp-eval`/`csp-mcp` (§3) | PASS | `data/eval-scenarios/` = ascent-mission, idle-warp, lease-launch, log-replay, script-ascent; `eval_suite.rs:285` `CARGO_BIN_EXE_csp-eval` |
| No flaky quarantines; vab_latency regression band ~line 158 (§3) | PASS | `vab_latency.rs:156-159` (`200.0` debug vs `16.0` release band) |
| csp-replay usage + exit codes 0/1/2, `--force` semantics (§4) | PASS | `csp-replay.rs:3-15` doc header, `ExitCode` mapping at l.29-44 |
| csp-perf flags, default out path, >10%/Pass→Fail/unmeasured gate, HardwareGated fps, row list (§4) | PASS | `csp-perf.rs:3-19`; `perf.rs:82-149` row ids match (doc's `weld.stage1/stage2` shorthand = `weld.stage1-1000-part`/`weld.stage2-300-part`); `REGRESSION_TOLERANCE = 0.10` (perf.rs:408) |
| csp-eval / csp-mcp usage + defaults (§4) | PASS | bin doc headers; `--fixtures-root` default `data/test-fixtures` (csp-eval.rs:57); `csp-saves`/`csp-dashboards`/`local` (csp-mcp.rs:42-45) |
| Server embedded (no standalone bin), 127.0.0.1:47900, token `$XDG_RUNTIME_DIR/csp/<pid>.token` mode 0600, `<token>\n<port>` (§5) | PASS | `csp-server/src/` has no `bin/`; `serve.rs:19` DEFAULT_PORT; `serve.rs:6-8,260-269` token file 0600 |
| MCP revision 2026-07-28 (+2025-11-25), no GET/DELETE, no `Mcp-Session-Id`, one JSON-RPC per POST (§5) | PASS | `crates/csp-server/PROTOCOL.md:5-31` |
| `/api/dashboards`, `/`+`/assets/*` token-free shell, `?token=` only on `/ws`, Origin/Host on every endpoint (§5) | PASS | `http.rs:6-23`; e2e `query_token_authenticates_only_the_ws_path`, `spa_serving_is_token_free_but_allowlisted` |
| WS rates "64/32/…/1, default 16" (§5) | PASS | `http.rs:84-85`; PLAN.md:861 "divisors of 64 Hz only (64/32/16/8/4/2/1), default 16 Hz" |
| `csp.mcp.tools.v1` + `"class": "session"` marker (§5) | PASS | `mcp.rs:51` TOOL_SCHEMA_VERSION; class marker mcp.rs:25,253 |
| SPA resolution: `apps/telemetry/dist/client` → `share/csp/telemetry` (§5) | PASS | `csp-app/src/lib.rs:3003-3028 packaged_spa_dir` |
| xtask invocation form; no cargo alias; dispatcher main() lines 391-406 (§6) | PASS | no `.cargo/` dir exists; `main()` at `xtask/src/main.rs:391`, file is 406 lines |
| lint-deps / lint-attribution / lint-reach behaviors (§6) | PASS | `lint_deps.rs` (cargo tree `--edges normal,build`, allow/deny, bevy_* family, exit 2 on tooling error); `lint_attr.rs` (pipe-format + allowlist); `lint_reach.rs` (PRODUCER_CRATES = csp-ui/csp-view/csp-app, `// ui-unreachable:` opt-out) |
| bless-goldens refusals: dirty tree, partial artifact set, hash-change w/o schema bump or `--reason`; bootstrap note (§6) | PASS | `xtask/src/main.rs:69-118,159-172,215-243` |
| build-web: staged atomic rename, prints exact manual commands on missing CLI (§6, §7, §10) | PASS | `main.rs build_web`; prints `cargo install wasm-bindgen-cli --version 0.2.129 --locked` + `python3 -m http.server -d apps/demo/dist` |
| `--bin bake_art`, `--bin gen_biome_maps [--check]` (§6, §8) | PASS | `xtask/src/bin/{bake_art.rs,gen_biome_maps.rs}`; `--check` = in-memory regen + byte-compare |
| Telemetry scripts dev/build/preview, :47901, proxy `/ws`+`/api` → `$CSP_SERVER_URL` (default 127.0.0.1:47900), `VITE_CSP_TOKEN`/`?csp_token=`, exact pins, finalize → `dist/client/index.html` (§7) | PASS | `package.json` (`"dev": "vite dev --port 47901"`, all versions exact), `vite.config.ts:20-30`, `scripts/finalize.mjs`, `README.md:21-28` |
| Demo: browser floor Chrome/Edge 113+/FF 141+/Safari 26+, Terrin-only ≤60 parts no saves, `apps/demo/data`, dist git-ignored (§7) | PASS | `apps/demo/index.html:6,29`; `.gitignore` `apps/demo/dist/` |
| bake_art → `data/art/{parts.csar,terrain.csar}`, `assets/ASSETS.md`, biome PNGs for terrin/rask/duva (§8) | PASS | `ls data/art/`, `assets/ASSETS.md`, `ls data/systems/home/*/biomes.png` |
| datahash exclusions: `test-fixtures/**` + art-extensions only under `data/art/` (§8) | PASS | `datahash.rs:27-31,188-213` (ART_EXTENSIONS incl. png…csar, ASCII-case-folded) |
| CI: two jobs, step order, xtask at ci.yml lines 52-54, wasm excludes, golden loop, perf job flags (§9) | PASS | ci.yml read line-by-line; check = fmt → clippy → lavapipe (30-33) → gated test (37-40) → wasm (45-48) → 3 lints (50-54) → deny (60-64) → goldens (72-77); perf job = release csp-perf with `--machine ci-host --baseline docs/perf/perf-log-devbox-release.ron` |
| Lint gates: workspace clippy lints, 4 per-crate `clippy.toml`s (f64 transcendentals, `seed_from_u64`, `HashMap/HashSet/std::time::*`), deny.toml (rand/rayon/bevy, copyleft, allowlist, wildcards) (§9) | PASS | `Cargo.toml [workspace.lints]`; `crates/{csp-core,csp-data,csp-phys,csp-sim}/clippy.toml` all present with those entries; `deny.toml` |
| Commit conventions incl. the quoted `chore(goldens): rebless for Phase1 (schema 21 + auto-rails)` (§9) | PASS | that exact subject is in `git log` (a02e496) |
| Troubleshooting rows (§10) | PASS | all cross-check to the sources above |

### Missing topics (what would still trip a new dev; prioritized)

1. **CI toolchain is `dtolnay/rust-toolchain@stable`, not the pinned 1.97.1**
   (ci.yml:15,87). §1 says the pin governs "any cargo invocation" (true
   locally via rustup) but never says CI deliberately builds on stable — a dev
   chasing a CI-only failure may assume toolchain parity. One sentence in §9
   fixes it.
2. `cargo-deny` is not vendored and must be installed locally before the
   "Definition of done"-style gate list in §9 can be run in full (ci.yml:56-59
   even admits it was never pre-verified locally). §9 lists `cargo deny check`
   with no install pointer.
3. The working tree currently carries an untracked scratch test
   (`crates/csp-view/tests/tmp_pick_probe.rs`) — irrelevant to a fresh clone,
   but a dev sharing this exact tree gets 1 extra csp-view test and may wonder
   why counts differ from §3.
4. Node.js engine floor is unstated (no `engines` field; Vite 8 requires a
   recent Node). §1 says only "Node.js: only for apps/telemetry".
5. A second perf row `weld.stage2-300-part.wasm` exists (`perf.rs:223`) and is
   absent from §4's row list — harmless until someone diffs the list against
   `perf.rs`.
6. §3 names `render_dump_extra` / `hud_dump` as explicit-run dumps but only
   spells the `-- --ignored` invocation for `render_gate`; the other two need
   the same flag (verified `#[ignore]` at `render_dump_extra.rs:172,226,395`,
   `hud_dump.rs:319,354`).

---

## 2. AGENTS.md

### Inaccuracies (fix these)

| # | Severity | Location | Claim | Reality (proof) |
|---|---|---|---|---|
| A1 | [warning] | AGENTS.md:78 | "the 24k-line lib.rs already carries ~360" tests | `crates/csp-sim/src/lib.rs` carries **199** of the crate's 363 inline tests; the rest live in `colonies.rs` (43), `orbit.rs` (22), `transfer.rs` (21), `contracts.rs` (16), etc. The ~360 figure is the *crate* total (BUILD-AND-DEV's table states this correctly). The misattribution sends a dev hunting for a test in the wrong 24k-line file. Fix: "~200 in lib.rs, ~360 across the crate". |
| A2 | [warning] | AGENTS.md:102 | "The game must run from the repo root (loads `./data`)" | Overstated, and contradicts BUILD-AND-DEV §2. `csp_view::data_root()` (`csp-view/src/lib.rs:4055`) probes `./data` first, then the **compile-time manifest-relative** repo `data/`, so a binary run from any CWD still finds the shipping tree (that is the fallback's stated purpose). "Prefer the repo root" is right; "must" is wrong — and the parenthetical reason is only the first candidate. |

Not statically checkable (policy claims, consistent with the tree): "state-
mutating iteration in ascending monotonic-ID order", "every control command is
absolute", "HASH_SCHEMA_VERSION bumps land in the same commit" (the bless tool
enforces bump-or-`--reason` at write time; same-commit is process, not tooling),
warp-never-changes-dt (TICK_HZ=64 immutable const supports it).

### Verified claims

| Claim | Result | Proof |
|---|---|---|
| 12 crates + xtask; edition 2024; bevy_ecs-standalone + wgpu 30 + egui 0.37 | PASS | `ls crates/` (12); workspace deps `bevy_ecs 0.20.0`, `wgpu 30`, `egui 0.37.0` |
| 64 Hz integer tick; libm-only floats; BLAKE3 state hash; golden replays | PASS | `csp-core/src/time.rs:9` `TICK_HZ=64`; clippy.toml bans; `blake3 =1.8.3`; goldens verified above |
| Workspace map (12 rows) incl. csp-ui "no sim/wgpu deps", csp-server/csp-bench native-only, csp-bench's 4 bins | PASS | `csp-ui/Cargo.toml` deps = egui/csp-core/csp-data/csp-phys only; `csp-bench/src/bin/` = the 4 bins |
| `SimCommand` at `crates/csp-sim/src/lib.rs`; issuers enforced by lint-reach | PASS | `lib.rs:534 pub enum SimCommand`; `lint_reach.rs` PRODUCER_CRATES |
| All 11 commands in the block | PASS | every form verified above (bin `csp`, `--serve`, `--bug-report`, `--test e2e`, `csp-replay verify data/test-fixtures/flight-launch`, xtask tasks/bins) |
| Wasm gate command; "cargo xtask is NOT an alias"; no feature flags | PASS | ci.yml:47-48; no `.cargo/`; no `[features]` anywhere |
| Determinism hard rules match actual gates: no wall clock (std::time banned), no HashMap/HashSet, rand facade banned (deny.toml), `seed_from_u64` disallowed, unwrap/expect denied workspace-wide, RNG via `csp_core::rng` | PASS | 4 clippy.tomls + deny.toml + `[workspace.lints]`; `csp-core/src/rng.rs` exists |
| Serialization-churn landmine: hand-written hash visitor, bless refuses dirty tree + unexplained hash change, CSPSAVE/CBOR saves, bincode replays | PASS | replay/bless machinery above; `ciborium` + CSPSAVE refs; `bincode` =2.0.1 in csp-bench |
| Where-tests-go guidance matches the tree | PASS | layout verified in B-table above |
| "Repo frequently has uncommitted in-progress edits in csp-app/csp-ui/csp-view" | PASS | `git status` shows exactly that pattern (historically; today's dirt is `.review/` + tmp_pick_probe) |
| Licensing: engine crates MIT OR Apache-2.0; csp-app/data/apps proprietary under LICENSE-GAME; CC0/MIT only | PASS | per-crate `license` fields; `LICENSE-GAME` at root; deny.toml copyleft=deny + attribution allowlist |
| Landmines: lib.rs ~24.5k (half tests: `mod tests` at 11881 of 24461 = 51%); vab.rs ~4.9k (4938); validate.rs ~6.6k (6580); colonies.rs ~5.2k (5172); `.review/` + REVIEW*.md/SYNTHESIS.md not product docs; perf log regenerate-only; demo dist git-ignored; csar committed | PASS | `wc -l` all four; `.review/`, `REVIEW.md`, `REVIEW2-*.md`, `SYNTHESIS.md` exist; `git ls-files data/art/` shows both .csar tracked; `.gitignore` |
| Definition-of-done gate list mirrors ci.yml exactly (fmt, clippy -D warnings, workspace tests, wasm check w/ 3 excludes, 3 lints, deny, every-scenario golden verify, perf --baseline) | PASS | ci.yml checked line-by-line — 1:1 match |
| Commits `type(scope): summary` on `main` | PASS | `git log` (fix(app)/feat(game)/feat(vab)/perf(view+sim+server+telemetry)/chore(goldens)…) |

### Missing topics (prioritized)

1. **Local `cargo-deny` install requirement** — the "Definition of done" list
   includes `cargo deny check`, but the tool is neither vendored nor mentioned
   as needing `cargo install cargo-deny`; a dev following the checklist hits an
   avoidable wall (ci.yml's own comment admits it was never pre-verified).
2. Same CI-vs-pinned-toolchain note as BUILD-AND-DEV (CI builds on `stable`).
3. Optional: point at `crates/csp-server/PROTOCOL.md` for the wire surface —
   AGENTS.md delegates to BUILD-AND-DEV, which covers it, so this is a
   convenience, not a defect.

---

## Worst 3 inaccuracies across both docs

1. **B1** — BUILD-AND-DEV.md:97 filter example names a test that does not exist
   (`advance_to_equivalence`); the command silently runs 0 tests.
2. **A1** — AGENTS.md:78 claims ~360 tests in `csp-sim/src/lib.rs`; the file has
   199 (363 is the crate-wide inline total).
3. **A2** — AGENTS.md:102 "must run from the repo root" is contradicted by
   `data_root()`'s manifest-relative fallback and by BUILD-AND-DEV's own §2.

## Verdicts

- `docs/BUILD-AND-DEV.md`: **MINOR-INACCURATE — PASS-WITH-FIXES** (2 fixes: B1, B2).
- `AGENTS.md`: **MINOR-INACCURATE — PASS-WITH-FIXES** (2 fixes: A1, A2).
