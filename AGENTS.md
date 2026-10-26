# AGENTS.md — Clanker Space Program

KSP-style build/fly space sim with an RTS progression layer. Rust workspace
(12 crates + `xtask`), edition 2024, `bevy_ecs`-standalone + wgpu 30 + egui 0.37,
deterministic headless sim (64 Hz integer tick, libm-only floats, BLAKE3 state
hash, golden replays). **`PLAN.md` is the PRD** — normative, sections cited as
§N everywhere in code comments. Deep docs live in `docs/`:
[BUILD-AND-DEV.md](docs/BUILD-AND-DEV.md) (commands, verified),
ARCHITECTURE.md, SIMULATION.md, GAMEPLAY-GUIDE.md, UI-AND-RENDERING.md,
DATA-AND-SCRIPTING.md. `docs/perf/` is a perf log, not documentation.

## Workspace map

| Crate | Role |
|---|---|
| `csp-core` | SimTime, DVec3/DQuat, StateHasher (§7.1), RNG KDF (§2.7), error classes |
| `csp-data` | RON schemas + loader/validator + data-set hash; `data/` is content |
| `csp-phys` | weld, EffectivePart resolver, SimpleBackend (contact/aero/thermal), CraftSession |
| `csp-sim` | **authoritative headless sim** — all game verbs are `SimCommand`s here |
| `csp-script` | Rhai host + frozen `ControlApi` (§2.10) |
| `csp-ai` | ascent autopilot (Rust, drives ControlApi) |
| `csp-view` | bevy_ecs presentation world, §2.5 schedules, the ONLY sim+render bridge |
| `csp-render` | wgpu renderer (Baseline tier), WGSL |
| `csp-ui` | pure-egui panels (VAB, HUD, program window, settings) — no sim/wgpu deps |
| `csp-app` | winit shell, frame loop, warp controller; ships binary **`csp`** |
| `csp-server` | native-only axum HTTP/WS/MCP (§2.12) |
| `csp-bench` | native-only harness; bins `csp-replay` `csp-perf` `csp-eval` `csp-mcp` |

Player verbs live in `SimCommand` (`crates/csp-sim/src/lib.rs`) and are issued
from `csp-ui`/`csp-view`/`csp-app` (enforced by `xtask lint-reach`). Content
lives in `data/` (RON parts/systems/tech/contracts/balance); frozen golden
fixtures in `data/test-fixtures/`; art packs in `data/art/*.csar`;
web-demo data in `apps/demo/data`.

## Commands (verified — see docs/BUILD-AND-DEV.md for the full map)

```bash
cargo run -p csp-app --bin csp                  # run the game (from repo root)
cargo run -p csp-app --bin csp -- --serve       # headless + HTTP/WS/MCP on 127.0.0.1:47900
cargo run -p csp-app --bin csp -- --bug-report  # diagnostic bundle
cargo test --workspace                          # all tests (CI parity)
cargo test -p csp-sim <name>                    # one test (substring filter)
cargo test -p csp-server --test e2e             # server e2e suite
cargo run -p csp-bench --bin csp-replay -- verify data/test-fixtures/flight-launch
cargo run -p xtask -- lint-deps                 # also: lint-attribution, lint-reach
cargo run -p xtask -- bless-goldens [--reason "<text>"]   # gated golden regen
cargo run -p xtask -- build-web                 # wasm demo → apps/demo/dist
cargo run -p xtask --bin bake_art               # assets → data/art/*.csar
cargo run -p xtask --bin gen_biome_maps [--check]
```

Wasm gate (native-only crates excluded):
`cargo check --workspace --exclude csp-server --exclude csp-bench --exclude xtask --target wasm32-unknown-unknown`.
Note: `cargo xtask …` is NOT an alias — always `cargo run -p xtask -- …`.
There are no cargo feature flags; native-only behavior is `cfg`-gated.

## Hard rules

- **Determinism invariants** (PLAN §2.x, enforced by lints): no wall clock, no
  `HashMap`/`HashSet`, no `rand` facade / `seed_from_u64`, no f64 methods for
  transcendentals — `libm` only, RNG only through `csp_core::rng`. Per-crate
  `clippy.toml` in csp-core/data/phys/sim denies all of these; `unwrap`/`expect`
  are denied workspace-wide. State-mutating iteration must be in ascending
  monotonic-ID order. Warp never changes dt (1/64 s, immutable forever); every
  control command is absolute, never a delta.
- **Data-driven content**: parts/systems/balance/contracts are `data/` RON, not
  code. Adding content = editing data + validators (`csp-data`), never a code
  path in the sim. Attribution for any non-authored asset is mandatory
  (`data/ATTRIBUTION.md`, linted).
- **Serialization churn is a landmine**: goldens (`data/test-fixtures/*/`)
  hash sim state via a hand-written visitor. Reordering struct fields, changing
  iteration order, RNG draws, or event order can redden every golden. Bumps to
  `HASH_SCHEMA_VERSION` must land **in the same commit** as the rebless
  (`chore(goldens): …` naming the reason); `bless-goldens` refuses a dirty tree
  and refuses unexplained hash changes. Saves are CSPSAVE/CBOR; replay logs are
  bincode — don't swap codecs casually (§7.2).
- **Where tests go**: sim behavior → inline `#[cfg(test)]` in `csp-sim`
  (the 24k-line lib.rs already carries ~360); data validation →
  `csp-data/tests/*`; player-flow e2e → `csp-bench` scenarios/eval suite;
  server wire behavior → `csp-server/tests/e2e.rs`. No code lands without its
  test (§8.7).
- **Uncommitted work**: check `git status` before blessing goldens (they refuse
  a dirty tree by design) and before rebasing onto mid-flight work — this repo
  frequently has uncommitted in-progress edits in `csp-app`/`csp-ui`/`csp-view`.
- **Licensing**: engine crates are MIT OR Apache-2.0; `data/`, `apps/`, and the
  shipped binary are proprietary (`LICENSE-GAME`). CC-BY-NC / share-alike assets
  are unusable — CC0/MIT only.

## Landmines

- `crates/csp-sim/src/lib.rs` is **~24.5k lines** (half of it inline tests) —
  edit surgically; the `SimCommand` doc comments are the feature ledger.
- `crates/csp-ui/src/vab.rs` (~4.9k), `crates/csp-data/src/validate.rs`
  (~6.6k), `crates/csp-sim/src/colonies.rs` (~5.2k) are the other big files.
- `.review/` is review-session artifacts (exploration maps, findings) — **not
  product docs**; do not cite it as spec. Same for `REVIEW*.md`/`SYNTHESIS.md`
  at the root (review inputs to PLAN.md).
- `docs/perf/*.ron` is a measurement log committed as a regression baseline;
  don't hand-edit it — regenerate with `csp-perf`.
- `apps/demo/dist/` is generated (git-ignored); `data/art/*.csar` is generated
  but **committed** (embedded via `include_bytes!`).
- The game must run from the repo root (loads `./data`).

## Definition of done

- Work targets a PLAN.md milestone (M0–M10 tags appear in code comments and
  commit messages). The milestone table (§6) carries measurable exit criteria.
- Gatekeeping before you claim done (mirror of `.github/workflows/ci.yml`):
  `cargo fmt --all -- --check`; `cargo clippy --workspace --all-targets -- -D
  warnings`; `cargo test --workspace`; the wasm check with the three excludes;
  `cargo run -p xtask -- lint-deps && lint-attribution && lint-reach`;
  `cargo deny check`; golden replays green (`csp-replay verify` for every
  scenario in `csp-replay list`). Perf-sensitive changes: `csp-perf
  --baseline docs/perf/perf-log-devbox-release.ron` (release build).
- Commits follow `type(scope): summary` (e.g. `fix(sim): …`, `feat(game): …`)
  on `main`.
