# Review Report — PRD-24: Tooling, CI, workspace config

**Cluster:** `xtask/src/{main,lint_deps,lint_attr,bin/*}`, workspace `Cargo.toml`, `deny.toml`, `rust-toolchain.toml`, `.github/workflows/ci.yml`, `data/ATTRIBUTION.md`, `docs/perf/`.
**PRD anchor:** PLAN.md §8.2 (gates table), §8.1 (test pyramid), §8.3–8.5 (fixtures, bless, e2e), §2.1 (dependency rules), §7.5 (save determinism).
**Method:** read all cluster files; ran `cargo run -p xtask -- lint-deps` / `lint-attribution`; `cargo tree`/`cargo metadata` for ground truth; `git log -S` for violation attribution.

---

## 1. §8.2 gate coverage matrix

| §8.2 row | Status | Evidence |
|---|---|---|
| `cargo check` native | ✅ covered | `cargo clippy --workspace --all-targets` + `cargo test --workspace` are supersets (ci.yml:25,40) |
| `cargo check` wasm | ✅ exact | ci.yml:46–48 matches the spec exclusion list verbatim |
| wasm purity (positive, incl. tokio edge exception) | ✅ via lint-deps | lint_deps.rs:176–192, 419–444; scans full wasm trees of csp-app/csp-view/csp-render/csp-sim/csp-ui — csp-app's tree transitively covers csp-script/csp-ai/csp-core/csp-data/csp-phys, so the five-name list is sufficient |
| Dependency rules (`xtask lint-deps`) | ✅ implemented — **currently RED** | lint_deps.rs; **fails today: F1** |
| Determinism lints (clippy.toml, 4 crates only) | ✅ | `crates/{csp-core,csp-data,csp-phys,csp-sim}/clippy.toml` byte-identical (diff-verified); identity + required-entry + reverse-membership pinned by lint_deps.rs:849–906; fired by `clippy -D warnings` |
| `cargo-deny` licenses + bans | ⚠️ installed, unverified | ci.yml:55–63 admits "could not be pre-verified locally"; deny.toml:18 duplicates = warn only (F8) |
| Rhai feature parity | ✅ | lint_deps.rs:507–694 — parity + banned features + *required* `no_time`/`no_std` |
| Data validators | ✅ | `cargo test --workspace` runs `crates/csp-data/tests/*` (8 files) + xtask balance/loc RON gates |
| `xtask lint-attribution` | ✅ | lint_attr.rs; currently `OK` |
| Property + unit tests (§8.1 a+b) | ⚠️ partial | no `proptest` anywhere; hand-rolled equivalents cover most named properties; ~3 missing (F9) |
| Golden replays (fast subset) | ⚠️ **2 of 6 named scenarios** | ci.yml:71–76; scenarios.rs:50–62 (F2) |
| Save round-trip determinism, N=10 000 | ⚠️ partial | mid-run restore tests exist at N≤65 ticks; golden verify restores initial snapshot only (F5) |
| Shader validation (naga, both binding models) | ⚠️ partial | csp-render/src/lib.rs:474–491 validates all 4 WGSL; only the Baseline binding model exists, so "both" is vacuous |
| `xtask lint-quality` (complexity/geiger/dup) | ❌ **task does not exist** | main.rs:389–401 knows only 4 commands |
| Coverage floors + ratchet | ❌ | no `ci/coverage-floors.json`, no llvm-cov step |
| cargo-machete/udeps | ❌ | no scheduled workflow |
| `cargo doc -D warnings` | ❌ **spec'd per-commit, absent** | — |
| cargo-mutants / `cargo msrv verify` | ❌ | rust-version "1.95" is never verified; toolchain pins 1.97.1 |
| CPU perf budgets (>10% vs committed log) | ⚠️ implemented, baseline compromised | ci.yml:88–94; baseline is devbox-measured and already contains Fail rows (F6) |
| Golden replays (full: colony century, event density, 100-yr economy) | ❌ no such scenarios/job | economy bands *are* unit-tested per-commit (economy.rs:370–479) — stronger than spec's nightly, a partial mitigations |
| Physics oracle (SimpleBackend vs RapierBackend) | ❌ | `RapierBackend` does not exist; `rapier3d-f64`/`parry3d-f64` pinned in Cargo.toml:60–70 but **declared by no crate** |
| Cross-platform divergence report | ❌ | — |
| Baseline screenshots (lavapipe, loose tolerance) | ⚠️ approximated | `render_gate.rs` + `CSP_RENDER_GATE_REQUIRED=1` (ci.yml:37–40): real headless rasterization + statistical pixel checks w/ control tests — arguably better than loose image-diff, but no committed reference images |
| Advanced screenshots + fps (REF machines) | ➖ n/a today | Advanced tier unbuilt (§6.4 cut OK); perf rows exist as HardwareGated |
| §8.5 app-smoke e2e (`csp --smoke`) | ❌ **absent** | csp-app/src/main.rs:7 — only `--bug-report` exists (F3) |
| §8.5 sim e2e / interface e2e | ✅ stronger than spec | eval_suite.rs (5 committed scenarios) + csp-server/tests/e2e.rs run per-commit |

**Net:** the per-commit core (check/clippy/wasm-purity/deps/deny/validators/attribution/goldens/render-gate/perf-diff) is real and largely faithful; the entire *weekly/nightly* tier and three named per-commit rows (docs, lint-quality, coverage) are absent; and the dep-rule gate is **currently failing**.

---

## 2. Findings

### F1 — HIGH — `xtask lint-deps` fails on the shipped tree: `csp-app` violates `both-sim-and-render`

- **Where:** rule at `xtask/src/lint_deps.rs:404–415` (PLAN §2.1, PLAN.md:163); violation in `crates/csp-app/Cargo.toml:19–22`.
- **Evidence:** `cargo run -p xtask -- lint-deps` →
  `VIOLATION: csp-app [both-sim-and-render] direct deps on both csp-sim and csp-render` ×2 (native tree + wasm tree) → exit 1.
- **Origin:** `csp-sim` was added to csp-app's deps in commit `c0be63e` (2026-10-25, "Phase3 UX criticals") — the need is real: `csp_sim::VesselId` at `csp-app/src/lib.rs:254,3605`, and `csp_render::{Gpu,…}` at `lib.rs:46–50` (the app owns the winit surface → GPU).
- **Impact:** CI step "dependency rules + wasm purity + attribution" (ci.yml:50–53) is red on every push/PR. Everything the step gates — dep rules, wasm purity, rhai parity, attribution — is masked by the failure. This is also a spec-vs-architecture tension: the app shell legitimately needs both `csp_sim` types and `csp_render` ownership; §2.1's intent ("only csp-view *bridges* them") is not what the lint enforces ("only csp-view may *depend on* them").
- **Suggestion:** pick one — (a) re-export `VesselId` (and the other sim types csp-app touches) through `csp-view` and drop the direct `csp-sim` dep; or (b) amend PLAN §2.1 + `check_deps` to allow `csp-app` (and only it) a type-level csp-sim edge while keeping the bridge-ban. Either way, the gate cannot stay red — a permanently-red gate is how the whole layer dies.

### F2 — HIGH — Golden subset is 2 of the 6 §8.2-required scenarios

- **Where:** `crates/csp-bench/src/scenarios.rs:50–62` registers only `trivial` and `flight-launch`; `data/test-fixtures/` holds matching dirs.
- **Evidence:** §8.2 names *ascent, reentry, landing+settle, docking merge, ×10⁵ warp jump, ×1-vs-×10⁵ equivalence*. flight-launch covers the first three (ascent→orbit→reentry→latch→`RecoverVessel`); `trivial` exercises command-ordering + warp ×4/×16 (scenarios.rs:80–81). **Missing: docking-merge golden, ×10⁵ warp jump, and ×1-vs-×10⁵ equivalence as a golden** (the equivalence exists only as the unit test `warp_x1_vs_x1e5_equivalence_10_sim_days`, lib.rs:12143).
- **Impact:** the 1 248-line `joints.rs` machinery has unit tests but no end-to-end golden; long-warp determinism is guarded only below the replay tier. The CI loop itself is correctly registry-driven (`csp-replay list`, ci.yml:71–76) — the gap is content, not plumbing.
- **Suggestion:** add the three scenarios to `SCENARIOS` + frozen fixtures; docking-merge needs a synthetic craft with `DockPort` modules built programmatically (no `DockPort` part exists in `data/parts/` — PRD-23 finding).

### F3 — HIGH — §8.5 app-smoke tier absent: no `csp --smoke`; the shipped binary is structurally untestable

- **Where:** `crates/csp-app/src/main.rs:7` — the only CLI flag is `--bug-report`; `csp_app::run()` is a blocking winit loop with no headless/frames/snapshot mode.
- **Evidence:** §8.5 requires `csp --smoke --frames N --snapshot out.png` to prove winit+GPU init, the frame loop, schedules, egui, snapshot export — "the thing unit tests structurally can't see". `render_gate.rs` drives `View` headlessly (no winit, no surface, no `App`), so it deliberately does *not* cover this layer.
- **Impact:** app-shell integration bugs — key dispatch → `enqueue` wiring, warp controller, settings IO, GPU surface init — have zero coverage anywhere in the pyramid.
- **Suggestion:** implement the `--smoke` mode (offscreen adapter like render_gate, run N `begin_frame/sim_step/end_frame` iterations, dump hash+PNG, exit) and wire it into CI — it subsumes most of what F4 needs.

### F4 — HIGH (structural) — No gate can see the *reachability* bug class

- **Evidence:** every test layer injects commands below the UI: unit tests call `w.enqueue(SourceClass::Ui, 0, SimCommand::…)` directly (e.g. scenarios.rs:78–84); goldens replay recorded command logs; eval scenarios replay logs; `csp-server` e2e uses the generic `sim.command` MCP tool (mcp.rs:312), which exposes the *entire* `SimCommand` surface to agents.
- **Problem:** nothing measures "which `SimCommand` variants are issuable by a player". `HireCrew`, `SetAlarm`, `LoadScript`, `EngageAutopilot`, `ManufactureCraft`, `StartResearch`, `RunBenchTest`, save/revert — all unreachable per the exploration map — cannot fail any gate. The same hole exists one level up: nothing diffs `ProgramMirror`/`stage_program` coverage against sim state fields.
- **Impact:** this PRD session's headline finding ("half the game unreachable") is *provably invisible* to the existing gates — they were all green while the reachability gap grew.
- **Suggestion:** a static surface lint (enumerate `SimCommand` variants; each must appear in `csp-ui`/`csp-app` dispatch or carry an explicit `ui-unreachable:` annotation — turns the reachability table into a ratchet), plus the F3 smoke path which forces player-shaped input.

### F5 — MEDIUM — §7.5 save round-trip at N=10 000 not implemented

- **Where:** §7.5 specifies "save → load → N ticks, hash-compared against uninterrupted N ticks, N = 10 000, across the scenario set".
- **Evidence:** existing mid-run restore tests step only 64–65 ticks (lib.rs:14494, 18039); golden `verify_golden` (replay.rs:783+) restores the *initial* snapshot — restore-at-tick-0, not a mid-run save→continue.
- **Impact:** the failure mode §7.5 targets — derived caches that are functions of history, visible only after mid-run restore — is shallowly exercised. Small-N restores can't catch caches that drift slowly.
- **Suggestion:** a per-scenario mid-run snapshot→restore→+10 000-tick hash test inside `csp-bench` (reuse recipes; snapshot mid-drive, fork, compare).

### F6 — MEDIUM — Perf gate compares against a devbox baseline on ci-host; baseline already contains failing rows

- **Where:** ci.yml:88–94 runs `csp-perf --machine ci-host --baseline docs/perf/perf-log-devbox-release.ron`.
- **Evidence:** the committed log's `machine.name = "devbox"` and contains `status: Fail` rows (`sim.200-part-atmo` 0.672 vs budget 0.5 ms/tick; `sim.1000-part-orbit` 4.63 vs 2.0). In `--baseline` mode the verdict is *regressions only* (csp-perf.rs:114–124 — "a permanently-red gate is a gate nobody keeps"); the cross-machine caveat is a printed note, not a gate input (csp-perf.rs:130–136); `regression_diff` matches rows by `id` only (perf.rs:440+).
- **Impact:** §5.3 absolute budgets are effectively unenforced in CI — a row red on devbox can stay red forever. Runner-to-runner timing variance on shared CI hosts can also produce >10% false regressions (or mask real ones). Also runs per-commit though §8.2 scheduled it nightly — stricter but noisier.
- **Suggestion:** commit a `perf-log-ci-host-release.ron` baseline measured *on the CI runner class* (first-run bootstrap), and add a separate ratchet row for currently-failing budgets (either fix them or annotate them as known-miss with tracking).

### F7 — MEDIUM — Entire scheduled-tier of §8.2 absent

- **Where:** `.github/workflows/` contains only `ci.yml` (`on: push, pull_request` — no `schedule:`).
- **Missing:** `xtask lint-quality` (doesn't exist — main.rs:389–401), cargo-geiger, machete/udeps, cargo-mutants, `cargo msrv verify` (rust-version = "1.95" vs toolchain channel 1.97.1 — nothing verifies MSRV), coverage floors+ratchet (`ci/coverage-floors.json` absent), full golden suite, physics oracle (impossible — no `RapierBackend`; `rapier3d-f64`/`parry3d-f64` workspace-pinned at Cargo.toml:60–70 but declared by **no** crate), cross-platform divergence report, and **the per-commit `cargo doc` `-D warnings` row**.
- **Impact:** ~half the §8.2 table is aspirational. Severity MEDIUM rather than HIGH because most rows are honest about cadence and the per-commit core is real.
- **Suggestion:** add a `nightly.yml` implementing the cheap rows first (doc, machete, MSRV, lint-quality skeleton); declare `RapierBackend` unimplemented in PLAN rather than letting the oracle row imply it exists.

### F8 — LOW/MEDIUM — cargo-deny gate admitted unverified; duplicates only warn

- **Where:** ci.yml:55–63 comment: "not vendored in this workspace, so this step could not be pre-verified locally — if the pin misbehaves on first CI run, adjust here." `deny.toml:18`: `multiple-versions = "warn"`.
- **Impact:** the license allowlist + global bans are likely fine (deny.toml's scoped bans are correct — `rand`/`rayon`/`bevy` global, per-crate scope deferred to lint-deps), but `cargo deny check` also runs the advisories/sources checks by default (network fetch, unconfigured) and duplicate versions cannot fail the build — §8.2 lists them inside a gates table.
- **Suggestion:** pin the action's `cargo-deny` version, run `cargo deny check` locally once, decide `multiple-versions = "deny"` vs documenting the warn choice.

### F9 — LOW — §8.1(a) proptest layer is hand-rolled; ~3 named properties missing

- No `proptest` in any manifest or Cargo.lock. Fixed-case equivalents exist for most named properties: `round_trip_property_e_dt_sweep` (orbit.rs:1630), `bit_identical_under_input_permutation` (weld.rs:1097), `resolution_is_bit_identical_and_permutation_safe` (resolve.rs:3102), `warp_x1_vs_x1e5_equivalence_10_sim_days` (lib.rs:12143), `command_key_is_a_strict_total_order` (lib.rs:11274), `tables_are_bit_deterministic`/`direction_set_is_deterministic` (aero.rs:1504,1833), `merge_and_split_conserve_momentum` (lib.rs:17515).
- **Not found:** colony flow-network allocation determinism under *permuted module insertion order*; hazard-integral closed form vs a fine-stepped reference (reliability tests exist but not this comparison); voxelizer determinism under *AABB perturbation* and *part-order permutation*.
- **Suggestion:** either adopt proptest for the remaining three, or document the deviation — example-tests give weaker falsification than the spec intends but are honest.

### F10 — LOW — `missing_docs` never configured

- §8.6: "`missing_docs` lint = warn on all public API in `csp-*` crates". No `missing_docs` in workspace lints or any crate manifest/lib.rs. Combined with the absent `cargo doc` gate, the "docs are the API contract" clause is unenforced.

### F11 — LOW — §2.2.A's state-write provenance test absent

- §2.2.A specifies "a debug build asserts that every state write is reached from a command handler or an event handler". No such assertion/context flag found in `csp-sim`. (A `mutation-origin` guard is the standard implementation; today `pub` fields + restores make it hard to express.)

### F12 — LOW — Determinism-lint domain excludes csp-script/csp-ai command issuers

- §8.2 scopes clippy.toml to the four crates — correctly implemented. But `csp-ai`'s ascent guidance and `csp-script`'s host shims run on every sim and issue commands; a platform `f64::atan2` there would diverge the *command stream* across platforms. Impact limited to T2 cross-platform parity (explicit non-goal, report-only row) → low. Consider extending the clippy.toml set to `csp-ai` (it already deps `libm`).

### F13 — LOW — `cargo check` native row satisfied only transitively

- `clippy --all-targets` + `cargo test` imply a compile, so this is cosmetic — noting only for matrix completeness.

---

## 3. lint-deps allow/deny vs current crate set

- **`DETERMINISM_CRATES` = the 4 spec'd crates** (lint_deps.rs:64) — matches §2.1's ban list; `check_membership` (610–636) proves every name is a live member and `cargo metadata` derives the member list — a new crate cannot pass unwalked. Sound.
- **`ALLOW` allowlist entries are permissive supersets:** csp-sim's list includes `blake3`, `rand_chacha` (unused as direct deps); csp-phys's includes `parry3d-f64`/`rapier3d-f64` (not declared); csp-core's includes `bincode`/`ciborium`/`miniz_oxide` (undeclared). Harmless today (extra deps are already in their trees) but means future additions of those crates skip review — acceptable per §2.1's wording, noted for completeness.
- **`csp-phys` allowlist pre-approves rapier/parry** — the deps were never declared (Cargo.toml:50–59 comment says "no crate depends on these yet"). The "rapier for joints" plan hasn't landed; the oracle row (F7) is therefore unimplementable.
- **`csp-render` EXTRA_BANS** (156–170) enforces §2.1's render rule including the `csp-view` reverse ban — good.
- **`bevy_ecs` rule** (386–400) implements the honest semantics — "enters only via csp-view edge" via parent-chain, not the impossible literal reading. Good.
- **`WASM_PKGS`** (176) omits csp-core/csp-data/csp-phys/csp-script/csp-ai by name — **not a gap**, since the check scans each listed crate's *entire* wasm tree and csp-app's tree contains them all.
- **`--edges normal,build`**: dev-deps (proptest's `rand`, naga, tokio-tungstenite) are correctly out of scope; build-deps land inside the tree and would hit allowlists — intentional strictness.
- **The engine is genuinely good:** pure `check_deps` with negative fixtures, exact-name matching, connector-column depth parsing, empty/misrooted output = error, `tree()` exits 2 on any cargo failure, `--locked` everywhere.

---

## 4. bless-goldens (§8.4) assessment — mostly strong, minor notes

`xtask/src/main.rs:69–265` implements the spec faithfully and in some places better:

- **Correct:** dirty-tree refusal (90–104); staged all-or-nothing artifact sets (113–143, 170–189) incl. *partial-set refusal* and *orphaned-golden refusal* (recipe deleted but artifacts committed); `record()` self-verifies before any write (153); hash-change refusal unless `HASH_SCHEMA_VERSION` bumped (currently 20, csp-core/hash.rs:200) **or** `--reason` (209–230); writes only after every refusal passed (234–243).
- **Nits:** (a) every bless re-records *all* scenarios — fine at 2, will hurt at the §8.2 full-suite scale; (b) `--reason` is only echoed to stdout — nothing ties it into the commit message, so the "documented" part of "documented behavioural change" is convention, not enforcement; (c) blessing is local-only by design — CI's `verify_golden` is the real gate, correct split.

## 5. Other observations

- `rust-toolchain.toml` pins channel `1.97.1`; `rust-version = "1.95"` is declared but nothing runs `cargo msrv verify` — MSRV is aspirational (F7).
- `deny.toml`'s global bans are correctly scoped and its comments correctly explain why `tokio`/`getrandom`/`wgpu` can't be global — this file is well-reasoned.
- `data/ATTRIBUTION.md`'s header-declared authored suffixes/names + the suffix (not extension) matching in lint_attr.rs handle `.ron.disabled`, `LICENSE`, `biomes.png` correctly; `lint-attribution: OK` today.
- `ci.yml` step ordering installs lavapipe *before* tests and sets `CSP_RENDER_GATE_REQUIRED=1` — the render gate cannot silently skip on CI. Good.
- `Cargo.toml:117–122` denies `unwrap_used`/`expect_used` workspace-wide; `#![forbid(unsafe_code)]` is present in every csp-* crate's lib.rs (stronger than §8.6's warn-level `unsafe_code`).
- xtask `src/bin/{bake_art,gen_biome_maps,parts/}` exist and `default-run = "xtask"` keeps `cargo run -p xtask` resolving — hygiene fine.

---

## 6. Severity summary

| # | Severity | One-liner |
|---|---|---|
| F1 | **HIGH** | `lint-deps` is red today: csp-app directly deps csp-sim+csp-render (violates own §2.1 rule) |
| F2 | **HIGH** | Golden suite = 2 scenarios; §8.2 requires 6 (no docking/warp-jump/equivalence goldens) |
| F3 | **HIGH** | No `csp --smoke` — the app binary/winit path is structurally untested |
| F4 | **HIGH** | Reachability class is invisible to all gates (everything enqueues `SimCommand`s directly) |
| F5 | MED | §7.5 N=10 000 save→load→ticks test absent (max ~65 ticks tested) |
| F6 | MED | Perf gate: cross-machine baseline, budget-Fail rows in committed log, movement-only verdict |
| F7 | MED | No scheduled-tier: doc/coverage/lint-quality/MSRV/mutants/oracle/full-goldens all absent |
| F8 | LOW-MED | cargo-deny never pre-verified; duplicate versions only warn |
| F9 | LOW | No proptest; ~3 §8.1(a) properties missing (colony-permutation, hazard-integral, voxel-perturbation) |
| F10 | LOW | `missing_docs` unconfigured; `cargo doc` gate absent |
| F11 | LOW | §2.2.A state-write-provenance debug assertion absent |
| F12 | LOW | clippy.toml determinism domain excludes csp-script/csp-ai (T2-only risk) |

**Bottom line:** the gate *plumbing* is better than most projects' — loud-failure discipline, pure testable rule engines, self-verifying goldens, a real lavapipe render gate — but (1) the dep gate is currently failing, (2) the golden/e2e *content* is thin against §8.2's named list, and (3) every gate operates below the UI seam, so the PRD-level reachability problem can never trip a wire. Fix F1 immediately (CI is red); F3+F4 are the structural recommendation.
