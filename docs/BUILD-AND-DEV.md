# Build & Dev Guide — Clanker Space Program

Everything a new dev (or AI agent) needs to build, run, test, and tool around this
repo without exploring it. Normative sources: `PLAN.md` (the PRD), the workspace
`Cargo.toml`, `.github/workflows/ci.yml`, and the tool sources cited per command.
**The tree is the truth** — every command below was verified against a manifest,
script, or source file; the proof is cited in parentheses.

Sibling deep docs (written alongside this guide):
[ARCHITECTURE.md](ARCHITECTURE.md) · [SIMULATION.md](SIMULATION.md) ·
[GAMEPLAY-GUIDE.md](GAMEPLAY-GUIDE.md) · [UI-AND-RENDERING.md](UI-AND-RENDERING.md) ·
[DATA-AND-SCRIPTING.md](DATA-AND-SCRIPTING.md). The perf log lives in
[`docs/perf/`](perf/) (see §9). `.review/` holds review-session artifacts — not
product docs.

---

## 1. Toolchain & prerequisites

- **Rust**: pinned by `rust-toolchain.toml` — channel **1.97.1**, components
  `clippy` + `rustfmt`, target `wasm32-unknown-unknown`. Any `cargo` invocation in
  the repo auto-installs/uses this toolchain.
- **MSRV / edition**: `rust-version = "1.95"`, edition 2024
  (`Cargo.toml [workspace.package]`).
- **Workspace**: 13 members — `crates/*` (12 crates) + `xtask`
  (`Cargo.toml [workspace] members = ["crates/*", "xtask"]`).
- **Node.js**: only for `apps/telemetry` (Vite/TanStack SPA; versions pinned
  exactly in its `package.json`). Not needed for any Rust workflow.
- **wasm-bindgen-cli 0.2.129**: only for the web-demo packaging
  (`cargo run -p xtask -- build-web`); must match the crate pin exactly.
- **Native vs wasm**: two targets exist. `csp-server`, `csp-bench`, and `xtask`
  are **native-only** (axum/tokio live in csp-server; csp-bench walks the
  filesystem; xtask depends on csp-bench). The wasm build excludes exactly those
  three (`ci.yml` "check (wasm32)"). `csp-app` links `csp-server` only under
  `cfg(not(target_arch = "wasm32"))` (`crates/csp-app/Cargo.toml`,
  `[target.'cfg(not(target_arch = "wasm32"))'.dependencies]`).
- **No cargo feature flags.** No crate in the workspace declares `[features]`;
  behavior differences are target-`cfg` only. Do not invent feature-flag commands.

## 2. Running the game

```bash
cargo run -p csp-app --bin csp          # debug build, native, from the repo root
cargo run --release -p csp-app --bin csp # release build (see perf note below)
```

- The binary is named **`csp`** (`crates/csp-app/Cargo.toml [[bin]] name = "csp"`,
  path `src/main.rs`; `cargo metadata` confirms `csp:bin`). The crate also builds
  a `cdylib` for wasm.
- **Run from the repo root**: startup loads the shipping `data/` tree.
  `csp_view::data_root()` resolves `./data` when it exists, else falls back to a
  manifest-relative path (`crates/csp-view/src/lib.rs` ~line 4055), so tools work
  from any CWD — but prefer the repo root. A missing/corrupt tree aborts startup
  loudly ("data/ startup load failed").
- **Debug vs release**: perf budgets in `PLAN.md §5.3` are release numbers, and
  the committed perf log `docs/perf/perf-log-devbox-release.ron` is a release
  measurement (its `profile: "release"` field). Release profile: `lto = "thin"`,
  `codegen-units = 1` (`Cargo.toml [profile.release]`). Sim debugging in a debug
  build is fine; never quote debug timings against budgets.
- **Other entry points on the same binary** (`crates/csp-app/src/main.rs`):
  - `csp --serve` — host the game headless under `csp-server` (§6).
  - `csp --bug-report` — write a diagnostic bundle (§10).

Settings/saves live under the platform config dir (`$XDG_CONFIG_HOME/csp` →
`~/.config/csp` on Linux; `crates/csp-app/src/settings_io.rs config_dir`).

## 3. Tests — the complete map

Run everything (this is what CI runs):

```bash
cargo test --workspace
```

Static `#[test]`/`#[tokio::test]` counts (counted 2026-10-26; treat as ~):

| Crate | Inline (`src/`) | Integration (`tests/`) | Layout notes |
|---|---:|---:|---|
| csp-sim | 363 | — | all inline in the 24.5k-line `crates/csp-sim/src/lib.rs` (`mod tests` ≈ half the file) + per-module inline tests across `orbit.rs`, `colonies.rs`, etc. |
| csp-phys | 157 | 14 | `tests/resolve_parts.rs`, `tests/session.rs` |
| csp-data | 81 | 40 | `tests/`: balance, colonies, contracts, experiments, home_system, parts, techtree, unknown_fields — they run the loader/validator over the real `data/` tree |
| csp-view | 60 | 29 | `tests/`: demo, flight_focus, flight_plume, pad_spawn, part_silhouettes, render_dump_extra, render_gate, stage_plan, startup_colonies, tutorial, vab_latency |
| csp-app | 51 | — | inline only |
| csp-bench | 53 | 12 | `tests/eval_suite.rs` (spawns the `csp-eval`/`csp-mcp` binaries) |
| csp-ui | 43 | 2 | `tests/hud_dump.rs` |
| csp-core | 42 | — | RNG KDF expected-bytes tests, StateHasher canonical-stream tests |
| csp-server | 31 | 22 | `tests/e2e.rs` — see below |
| csp-script | 13 | 3 | `tests/api_freeze.rs` pins the frozen `ControlApi` surface |
| csp-render | 5 | — | WGSL validation via `naga =30.0.1` dev-dep, pinned to the wgpu 30.0.1 validator (shader compile gate in `cargo test`) |
| csp-ai | 0 | — | exercised via csp-sim/bench tests |
| xtask | 14 | — | lint-rule engine unit tests |

One crate / one test:

```bash
cargo test -p csp-sim
cargo test -p csp-sim warp_x1_vs_x1e5_equivalence_10_sim_days   # filter by test-name substring
cargo test -p csp-view --test render_gate      # one integration-test binary
```

### Render gate (GPU tests)

`crates/csp-view/tests/render_gate.rs` (+ `render_dump_extra.rs`,
`csp-ui/tests/hud_dump.rs`) render offscreen via a surfaceless wgpu adapter.
With no Vulkan ICD they **print `SKIP` and pass**; with
`CSP_RENDER_GATE_REQUIRED=1` a missing adapter is a *failure* (CI installs
lavapipe — `mesa-vulkan-drivers` — and sets the flag; `ci.yml` lines 30–40).
Locally on a GPU box they just run. The scratch-dump tests are `#[ignore]`d and
run explicitly, e.g.
(`render_gate.rs` documents this invocation itself):

```bash
cargo test -p csp-view --test render_gate dump_sky -- --ignored
```

The dumps are binary P6 PPM (`/tmp/frame.ppm`, `/tmp/sky_*.ppm`,
`/tmp/hud_*.ppm`, …). Convert one to PNG for eyeballing with the xtask
helper — the output path defaults to the input's with the extension
swapped for `.png`:

```bash
cargo run -p xtask --bin ppm2png -- /tmp/frame.ppm   # → /tmp/frame.png
```

### Golden replay fixtures

- Frozen data sets live in `data/test-fixtures/<scenario>/` — currently
  **`trivial`** and **`flight-launch`** (`crates/csp-bench/src/scenarios.rs`
  `SCENARIOS` registry; directory listing agrees).
- Each golden dir holds three artifacts (`replay.bin`, `expected-hash.txt`,
  `fingerprint.json` — the §7.1 determinism fingerprint: rustc, target triple,
  exact dep versions, schema versions, fixture hash; written by
  `csp-bench/build.rs` + `replay.rs`).
- They are verified two ways: the workspace test
  `every_registered_scenario_has_a_verified_golden`
  (`crates/csp-bench/src/replay.rs` ~line 1257) and the CI loop over
  `csp-replay list` (§"CI" below).
- **Goldens are insulated from balance tweaks**: they load frozen fixtures, not
  `data/` (PLAN §8.3). Changing shipping data does *not* redden goldens; it
  changes the `data_set_hash` (§8) and triggers the save-drift warning instead.

### E2E tests

- **csp-server e2e**: `crates/csp-server/tests/e2e.rs` — 22
  `#[tokio::test(flavor = "multi_thread")]` tests driving the real axum router
  (dev-deps `tower` + `tokio-tungstenite` WS client): token lifecycle/0600
  perms, Origin/Host allowlists, MCP round-trip + external ordering, bounded
  ingress backpressure, control lease over the wire, WS subscription frames,
  commands-vs-local-world hash. Run: `cargo test -p csp-server --test e2e`
  (included in `cargo test --workspace`).
- **Eval-tier e2e**: `crates/csp-bench/tests/eval_suite.rs` — runs the committed
  scenario suite in `data/eval-scenarios/` (`ascent-mission`, `idle-warp`,
  `lease-launch`, `log-replay`, `script-ascent`) through the lockstep agent
  loop, and spawns the `csp-eval`/`csp-mcp` binaries as subprocesses.
- **App smoke / screenshots** (lavapipe) and cross-platform divergence reports
  from PLAN §8.5 are CI/nightly design only — no extra local harness exists in
  the tree beyond the render gate above. (unverified beyond the tree)

### Known-flaky notes

- No test is quarantined as flaky. `crates/csp-view/tests/vab_latency.rs`
  (line ~158) deliberately measures a *regression band*, not a hard gate, to
  avoid flakiness — the §5.4 16 ms VAB kill criterion.
- All `#[ignore]`d tests are explicit-run visual dumps (render_gate,
  render_dump_extra, hud_dump), not flake quarantines.

## 4. csp-bench — the four headless binaries

Package `csp-bench` (native-only) ships four bins
(`crates/csp-bench/src/bin/`; confirmed by `cargo metadata`):

```bash
cargo run -p csp-bench --bin csp-replay -- list
```

| Bin | What it does | Exact usage (from each bin's doc header) |
|---|---|---|
| `csp-replay` | Golden-replay CLI: record/verify the §7.6 artifacts | `csp-replay list` · `csp-replay record <scenario> <dir> [--force]` · `csp-replay verify <dir>`. Exit 0 ok / 1 hash mismatch / 2 harness error. `record` is the bootstrap path for a *new* golden; `--force` authorizes overwriting an occupied dir whose hash differs. |
| `csp-perf` | Runs the PLAN §5.3 budget suite; writes the perf log; `--baseline` gates regressions (>10% slower, a Pass→Fail crossing, or an unmeasured row fails) | `csp-perf [--out <path>] [--machine <name>] [--baseline <path>]`. Default out: `docs/perf/perf-log-<machine>-<profile>.ron`. Measurable rows: `sim.200-part-atmo`, `sim.1000-part-orbit`, `sim.300-part-landed`, `sim.map-500-rails`, `warp.20-colonies.*`, `weld.stage1/stage2`, `telemetry.1000-part`; fps rows are recorded `HardwareGated` (reference-machine gates, not CI). |
| `csp-eval` | Scenario runner: `scenario.ron` in, `EvalReport` JSON (exit code + final state hash + score) on stdout | `csp-eval <scenario.ron \| --suite <dir>> [--fixtures-root <dir>] [--transcript <out.json>]`. Default fixtures root `./data/test-fixtures`. Committed suite: `data/eval-scenarios/`. |
| `csp-mcp` | stdio MCP host: newline-delimited JSON-RPC over its own headless sim (the eval-harness spawn target; never attaches to a running game) | `csp-mcp [--empty \| --fixture <dir>] [--save-dir <dir>] [--client <name>] [--dashboard-dir <dir>]`. Defaults: empty world, `./csp-saves`, `./csp-dashboards`, client `local`. |

When to use which: `csp-replay verify` after any determinism-domain change;
`csp-perf` on a reference machine in **release** when touching the tick loop,
weld, or event queue (commit the log to `docs/perf/`); `csp-eval` for
agent/mission-shaped acceptance; `csp-mcp` to drive the sim from an MCP client
without the windowed app.

## 5. csp-server — HTTP/WS/MCP surface

The server is *embedded* — there is no standalone server binary. Two ways to run
it (`crates/csp-app/src/lib.rs serve_cli`, `crates/csp-server/src/serve.rs`):

```bash
cargo run -p csp-app --bin csp -- --serve   # game thread + server on 127.0.0.1:47900
cargo run -p csp-bench --bin csp-mcp        # or the stdio MCP host instead
```

- **Default bind `127.0.0.1:47900`** (`serve.rs DEFAULT_PORT`); token at
  `$XDG_RUNTIME_DIR/csp/<pid>.token` (mode 0600, first line token, second line
  port). Bearer auth everywhere; `?token=` is legal only on the `/ws` upgrade.
- **Surface**: `POST /mcp` (Streamable-HTTP MCP, revision `2026-07-28`, also
  accepts `2025-11-25`; one JSON-RPC per POST; no GET/DELETE, no
  `Mcp-Session-Id`); `GET /ws` (subscription telemetry stream, rates 64/32/…/1
  Hz, default 16); `/` + `/assets/*` (the built telemetry SPA, token-free
  shell); `/api/dashboards` (sidecar dashboard JSON). Origin/Host allowlist on
  **every** endpoint. Tool schema `csp.mcp.tools.v1`; session-class tools are
  marked `"class": "session"` and are not replay-relevant. Full pin list:
  `crates/csp-server/PROTOCOL.md`.
- `csp --serve` resolves the SPA from `apps/telemetry/dist/client` (dev
  checkout), falling back to `share/csp/telemetry` beside the exe
  (`csp-app/src/lib.rs packaged_spa_dir`) — build the SPA (§7) first or you get
  the placeholder page.
- **e2e**: `cargo test -p csp-server --test e2e` (see §3).

## 6. xtask — every task

Invocation is **`cargo run -p xtask -- <task>`** (what `ci.yml` lines 52–54
run). The source doc-headers spell it `cargo xtask …`, but no
`.cargo/config.toml` alias exists in the repo — use the `cargo run` form.
Dispatcher: `xtask/src/main.rs` (`main()`, lines 391–406).

| Command | Purpose | Proof |
|---|---|---|
| `cargo run -p xtask -- lint-deps` | PLAN §2.1 dependency-rule gate: parses `cargo tree --edges normal,build` per crate against explicit per-crate allow/deny lists (no `rand` facade, no `bevy_*` outside csp-view, no wgpu/winit/egui/tokio in the determinism crates); fails loudly on tooling error | `xtask/src/lint_deps.rs` |
| `cargo run -p xtask -- lint-attribution` | §3.7 asset-license lint: every non-authored file under `data/`/`assets/` needs a `path \| source \| license \| url` line in `data/ATTRIBUTION.md` with an allowlisted license | `xtask/src/lint_attr.rs` |
| `cargo run -p xtask -- lint-reach` | "sim-complete but no UI" gate: every `SimCommand` variant needs a production issuer in `csp-ui`/`csp-view`/`csp-app` or an explicit `// ui-unreachable: <reason>` at the declaration | `xtask/src/lint_reach.rs` |
| `cargo run -p xtask -- bless-goldens [--reason "<text>"]` | §8.4 golden regeneration: re-records every registered scenario's `replay.bin`/`expected-hash.txt`/`fingerprint.json`. **Refuses**: dirty tree (`git status --porcelain` non-empty), partial artifact sets, and a changed hash without a `HASH_SCHEMA_VERSION` bump or `--reason`. Bootstrap of a new golden: commit the fixture dir + recipe, then `bless-goldens` or `csp-replay record <name> <dir>` | `xtask/src/main.rs bless_goldens` |
| `cargo run -p xtask -- build-web` | Package the §6.3 web demo: wasm release build of `csp-app --lib`, `wasm-bindgen --target web` into `apps/demo/dist` (staged, atomic rename; prints the exact manual commands if `wasm-bindgen-cli` is missing) | `xtask/src/main.rs build_web` |
| `cargo run -p xtask --bin bake_art` | Bake art: `assets/src/` + procedurally-authored part meshes → `data/art/{parts.csar,terrain.csar}` | `xtask/src/bin/bake_art.rs` |
| `cargo run -p xtask --bin gen_biome_maps [--check]` | Regenerate the authored biome PNGs under `data/systems/home/*/biomes.png`; `--check` verifies the committed files are byte-identical to the program's output (run manually — CI does not invoke it) | `xtask/src/bin/gen_biome_maps.rs` |

## 7. apps/telemetry and apps/demo

### apps/telemetry (TanStack Start + Vite SPA; scripts in `package.json`)

```bash
cd apps/telemetry
npm install          # exact pins, no carets (§2.12.2 rule)
npm run dev          # vite dev on :47901, proxies /ws + /api to $CSP_SERVER_URL (default http://127.0.0.1:47900)
npm run build        # vite build + scripts/finalize.mjs → dist/client/{index.html,assets/*}
npm run preview      # vite preview of the built bundle
```

- Dev flow: run `csp --serve` alongside, then open
  `http://localhost:47901/?csp_token=<token>` once (token file:
  `$XDG_RUNTIME_DIR/csp/<pid>.token`, first line), or export `VITE_CSP_TOKEN`
  (`apps/telemetry/README.md`, `vite.config.ts`).
- The build output `dist/client` is exactly what `csp --serve` serves
  (`packaged_spa_dir`, §5). The shell is token-free by design.

### apps/demo (wasm web demo)

```bash
cargo run -p xtask -- build-web                        # → apps/demo/dist
python3 -m http.server -d apps/demo/dist               # serve it (command printed by the task)
```

Requires `wasm-bindgen-cli` **0.2.129** (`cargo install wasm-bindgen-cli
--version 0.2.129 --locked`). Needs a WebGPU browser (Chrome/Edge 113+, Firefox
141+, Safari 26+ — `apps/demo/index.html`). The demo contract is fixed: Terrin
only, ≤60 parts, no saves (enforced in the app, `apps/demo/index.html` comment).
`apps/demo/data` is the reduced embedded data set; `apps/demo/dist` is
git-ignored (`.gitignore`).

## 8. Data baking / asset pipeline

Two generators, both deterministic:

- **`bake_art`** → `data/art/parts.csar` (part meshes, authored procedurally in
  `xtask/src/bin/parts.rs`) and `data/art/terrain.csar` (ambientCG CC0 textures
  from `assets/src/`; provenance in `assets/ASSETS.md`). Nothing under `assets/`
  is read at runtime — `data/art/*.csar` is embedded via `include_bytes!` by
  `csp-view`. **Regenerate** whenever `xtask/src/bin/parts.rs` or anything in
  `assets/src/` changes, then commit the `.csar` files.
- **`gen_biome_maps`** → `data/systems/home/{terrin,rask,duva}/biomes.png`. The
  committed PNGs must be byte-identical to the program's output — after editing
  the region rules or a body's `biomes.ron` legend, rerun it and verify with
  `--check` (manual; CI does not gate this).

**Hash implications** (`crates/csp-data/src/datahash.rs`): the `data_set_hash`
covers every file under `data/` **except** `data/test-fixtures/**` (hashed
separately as the per-scenario `fixture_hash` inside the golden's
`fingerprint.json`) and art-extension files (`png/jpg/…/csar`) **only under
`data/art/`**. So:

- Re-baking `.csar` packs does **not** change the data-set hash.
- Editing shipping RON/PNG (balance, parts, biomes) **does** — old saves load
  with a drift warning (PLAN §7.7), and `BuiltPart.data_set_hash` freezes what
  in-flight craft meant.
- Editing `data/test-fixtures/` changes the fixture hash recorded in
  `fingerprint.json`; if it changes the replay outcome you are in
  `bless-goldens` territory (§6), which gates it on a schema bump or `--reason`.

## 9. CI, lint gates, commit conventions

CI is one file, `.github/workflows/ci.yml`, two jobs on push/PR:

- **`check`** (ubuntu-latest): `cargo fmt --all -- --check` →
  `cargo clippy --workspace --all-targets -- -D warnings` → install lavapipe →
  `CSP_RENDER_GATE_REQUIRED=1 cargo test --workspace` →
  `cargo check --workspace --exclude csp-server --exclude csp-bench --exclude xtask
  --target wasm32-unknown-unknown` → `xtask lint-deps` + `lint-attribution` +
  `lint-reach` → `cargo deny check` → build `csp-replay` and
  `verify` every registered golden.
- **`perf`**: release `csp-perf --machine ci-host --out /tmp/… --baseline
  docs/perf/perf-log-devbox-release.ron` — the >10% regression gate.

Lint gates you will hit locally:

- Workspace clippy (`Cargo.toml [workspace.lints]`): `unwrap_used`/`expect_used`
  **deny** everywhere (narrow `#[allow]`s with stated reasons only);
  `cognitive_complexity`/`type_complexity`/`too_many_lines`/`too_many_arguments`
  warn; `unsafe_code` warn.
- Per-crate `clippy.toml` in the four determinism crates (`csp-core`,
  `csp-data`, `csp-phys`, `csp-sim`): `disallowed-methods` ban the f64
  transcendentals (libm only) and `SeedableRng::seed_from_u64`;
  `disallowed-types` ban `HashMap`/`HashSet`/`std::time::*`.
- `deny.toml` (cargo-deny): global bans on `rand`, `rayon`, `bevy`;
  copyleft denied; license allowlist; wildcards denied.
- `cargo fmt --check` before every commit (CI runs it first).

Commit/branch conventions (observable in `git log`, branch `main`):
Conventional-Commits-style `type(scope): summary` — `fix(app):`, `fix(sim):`,
`feat(game):`, `feat(vab):`, `perf(view+sim+server+telemetry):`,
`chore(goldens): rebless for Phase1 (schema 21 + auto-rails)`. Golden reblesses
are their own commits and name the schema version/reason (PLAN §8.4).

**Perf-log convention**: `docs/perf/perf-log-<machine>-<profile>.ron`
(`csp-perf` writes the name; the committed reference is
`perf-log-devbox-release.ron`). Reference-machine fps rows are `HardwareGated`
entries in the same file; results are committed, not ephemerally reported.

## 10. Troubleshooting

| Symptom | Cause / fix |
|---|---|
| `data/ startup load failed` on launch | Not running from the repo root, or a broken `data/` tree. The validator refuses loudly on invalid files (collected errors, `csp-view from_source`). Run `cargo run -p csp-app --bin csp` from the workspace root. |
| wasm build fails on `axum`/`tokio`/`csp-server`/`csp-bench` | You built the whole workspace for wasm. Those are native-only; use the CI exclusion set: `cargo check --workspace --exclude csp-server --exclude csp-bench --exclude xtask --target wasm32-unknown-unknown` (`ci.yml`). |
| `build-web` says `wasm-bindgen not found` | `cargo install wasm-bindgen-cli --version 0.2.129 --locked` (the task prints these exact commands; the CLI version must match the crate pin). |
| Render-gate tests print `SKIP` | No Vulkan ICD. Install lavapipe (`mesa-vulkan-drivers libvulkan1`) or accept the skip; set `CSP_RENDER_GATE_REQUIRED=1` to make the skip a failure (CI behavior). |
| A golden replay (`csp-replay verify` or the pin test) goes red after a dep bump | Expected if a determinism-domain dep changed: check `fingerprint.json` first. Legit change → bump `HASH_SCHEMA_VERSION` or pass `--reason`, then `cargo run -p xtask -- bless-goldens` on a **clean tree** (§6). Never hand-edit `expected-hash.txt`. |
| Perf numbers look terrible | Debug build. Budgets are release numbers; use `cargo run --release -p csp-bench --bin csp-perf` and compare with `--baseline docs/perf/perf-log-devbox-release.ron`. |
| Need a repro bundle for a crash/bug | `cargo run -p csp-app --bin csp -- --bug-report` — writes save + command log + data-set hash + fingerprint + last 500 log lines under `<config_dir>/csp/bug-reports/` (`crates/csp-app/src/bugreport.rs bug_report_root`; the CLI prints the path). |
| Telemetry SPA shows the placeholder page | `apps/telemetry/dist/client` not built. `cd apps/telemetry && npm install && npm run build`, then restart `csp --serve`. |
| `cargo xtask <task>` fails | There is no cargo alias; use `cargo run -p xtask -- <task>` (§6). |

---

*Verified against the tree on 2026-10-26 (commit-time HEAD on `main`; some
working-tree files were mid-edit). Counts and line numbers drift — the cited
files are authoritative.*
