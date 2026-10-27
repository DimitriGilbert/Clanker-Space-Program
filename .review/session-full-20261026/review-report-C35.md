# Review C35 — Tooling, parts-mesh generator, CI (session-full-20261026)

Scope: `xtask/src/{main,lint_deps,lint_attr,lint_reach}.rs`, `xtask/src/bin/{bake_art,gen_biome_maps,parts/mod}.rs`,
workspace `Cargo.toml`, `deny.toml`, `rust-toolchain.toml`, `.github/workflows/ci.yml`, `docs/perf/perf-log-devbox-release.ron`.
Verified against the working tree (HEAD 7dd4b86). Parallel findings respected, not re-derived: C29 (perf gate
machine-blind, `csp --smoke` missing), V-DATA (`gen_biome_maps --check` not run in CI), V-C32 (ID-stability tripwire absent).

Positive verifications (no finding): lint-deps ALLOW lists match the actual direct-dep sets of all four determinism
crates (no drift in either direction); `check_membership` (lint_deps.rs:610) pins every authored list to real members;
attribution ledger is consistent with the Kenney-kit retirement — no stale rows, `data/art/parts.csar` and all
`assets/src/*.jpg` rows resolve; bake_art is idempotent (fixed `build_all` order parts/mod.rs:1290-1317, `BTreeMap`
throughout, no clock/RNG, pure-Rust miniz deflate); wasm check flags in ci.yml:47-48 match §8.2's excludes exactly;
lint-reach IS wired in CI (ci.yml:54).

---

### [SEVERITY: HIGH] Finding 1: The weekly/nightly half of §8.2 does not exist — no scheduled CI, no lint-quality, 8 gate rows absent
**File**: `.github/workflows/ci.yml:3-5` — **Problem**: the only workflow triggers on `push`/`pull_request`; there is no
`schedule`/cron anywhere in `.github/`, and no `ci/` directory exists.
**Evidence**: repo-wide grep for `schedule|cron|llvm-cov|cargo-mutants|machete|udeps|cargo-geiger|lint-quality` returns
zero hits; `xtask/src/main.rs:393-405` dispatches only lint-deps/lint-attribution/lint-reach/bless-goldens/build-web —
the §8.2 "Code quality" tool (`xtask lint-quality`, PLAN:1761) is not coded at all. §8.2 rows with no implementation:
Code quality (1761), Coverage+ratchet (1762 — `ci/coverage-floors.json` nonexistent), Docs `cargo doc -D warnings` +
`missing_docs` (1764), Mutation testing (1765), MSRV verify (1766), Golden replays full nightly (1768), Physics oracle
nightly (1769 — no `RapierBackend` exists; `parry3d-f64`/`rapier3d-f64` are declared in `Cargo.toml:60-70` with the
comment "no crate depends on these yet"), Cross-platform divergence report (1770). Half-missing: the weekly
`cargo-machete`/`cargo-udeps` half of the Dead-code row (1763; the `clippy -D warnings` half is wired, ci.yml:25).
§6.4's cut list sanctions none of this.
**Impact**: every §8.2 gate that §8.7.6 deliberately placed off the per-commit path ("slow gates live where their
runtime is acceptable") lives nowhere instead; §8.6's "quality is enforced by tooling that fails the build" (CRAP,
function-size >200, geiger, duplication) has no tooling, which is why `validate.rs` at 6.6k lines (already flagged to
C32) could accumulate.
**Suggestion**: add `ci-nightly.yml` (cron) covering rows 1765-1772 + the weekly quality job; implement `xtask
lint-quality` minimally (function >200 lines = fail, cognitive-complexity report) since the workspace already exposes
the parse utilities; add `ci/coverage-floors.json` + an llvm-cov step.

### [SEVERITY: HIGH] Finding 2: Baked-art staleness is undetectable — committed `.csar` packs have no check mode, fingerprint, or CI regeneration
**File**: `xtask/src/bin/bake_art.rs:590-655`; `crates/csp-view/src/art.rs:87-88` — **Problem**: `data/art/parts.csar`
and `terrain.csar` are committed binaries embedded via `include_bytes!`, regenerated only when a developer remembers
to run `bake_art`; nothing anywhere proves the committed bytes match their sources.
**Evidence**: `bake_art::main` has no `--check` and writes unconditionally; no CI step, test, or sidecar hash
references the packs (grep for `csar` outside csp-view/xtask/docs: none). The sibling `gen_biome_maps.rs:267-290`
implements `--check` explicitly "so CI can prove the committed files are what this program produces" — the art bake has
no equivalent. Staleness inputs: `xtask/src/bin/parts/*.rs` (12 model constructors), `assets/src/*.jpg`, and the bake
code itself. The decoder degrades to a procedural/kitbash fallback only on *decode failure* (art.rs:103,120) — a
valid-but-stale pack renders silently wrong, and a part authored in `parts/` without a model-key update (16 data
parts vs 12 model keys; dockport/lab/legs/probe/tank.life reuse keys via `model_key`, C20's mismatch) shows a stale
silhouette with zero signal. The code even guards the *intra-run* version of this hazard (bake_art.rs:598-601,
C19bake-F3: "a corrupt jpg left a fresh parts pack beside a stale terrain pack — silently mixed generations") but
leaves the *across-commits* version unguarded.
**Impact**: mixed-generation art can ship; a reviewer cannot verify `data/art` matches the tree it sits in — the same
class of undetected-state problem V-DATA flagged for biome maps, but with no coded check to wire.
**Suggestion**: add `bake_art --check` (assemble in memory, byte-compare, exit 1) and run it in CI beside
lint-attribution; alternatively commit a BLAKE3 sidecar of both packs and verify in the same step.

### [SEVERITY: MEDIUM] Finding 3: lint-reach has two false-negative classes — a ui-unreachable regression can ship green
**File**: `xtask/src/lint_reach.rs:61-64` and `:132-133` — **Problem**: (a) any line *containing* `ui-unreachable:` is
consumed as a pending annotation and `continue`d **before** variant-row detection; (b) issuer detection is
`text.contains("SimCommand::Name")` over raw non-test source, so comments and string literals count as production
issuers.
**Evidence**: (a) a trailing-style opt-out `Foo, // ui-unreachable: x` both removes `Foo` from the checked set
entirely *and* binds the opt-out to the NEXT variant row, wrongly exempting it too — a double silent pass. (b) live
instances of the comment class today: `crates/csp-app/src/warp.rs:2-3` (module doc mentioning
`SimCommand::SetWarp`/`SimCommand::AdvanceTo`), `warp.rs:192,310,344`, `crates/csp-ui/src/settings.rs:219`
(`SimCommand::TakeControl`). If the real construction sites were deleted while these docs stayed, the lint stays
green. The gate exists precisely to prevent "sim-complete, UI-missing" (module doc, lint_reach.rs:12-19).
**Impact**: the reachability guarantee — the repo's answer to the Phase-2 findings — is enforceable only by convention
(comment hygiene), not by the lint.
**Suggestion**: strip `//`/`///` comments and string literals from sources before the `contains` scan (a line-comment
cutter is ~5 lines); and reject the trailing style loudly — if a line that is a variant row also contains the marker,
emit a failure telling the author to put the opt-out on its own line.

### [SEVERITY: MEDIUM] Finding 4: §8.2/§7.5 save round-trip determinism gate (N = 10 000) not implemented as specified
**File**: `crates/csp-sim/src/save.rs:618,650`; `PLAN.md:1759` + `PLAN.md:1672-1677` — **Problem**: the every-commit §8.2
row promises "§7.5, N = 10 000" — save → load → N ticks hash-compared against uninterrupted N ticks, across the
scenario set.
**Evidence**: what exists is single-shot round-trip equality (`save_round_trip_current`, `unlabeled_save_round_trips`)
and byte-stability (`save_encoding_is_byte_stable`); no test in csp-sim/csp-bench performs the reload-vs-uninterrupted
hash comparison at N=10,000 across scenarios (the `0..10_000` loops at `csp-sim/src/lib.rs:15421/15620/15682/15937`
are settle/latch/loop tests, not save-reload equivalence).
**Impact**: §7.5's named bug class — "usually a cache that is a function of history rather than of state" — has no
tripwire, while §8.2 lists it as a per-commit gate; this matters more now that save/load/revert is on the player hot
path (F10 revert, Game menu).
**Suggestion**: add a workspace test (or a registered csp-bench scenario) that runs each fixture uninterrupted for
10,000 ticks and again with a save/load at the midpoint, asserting equal state hashes.

### [SEVERITY: LOW] Finding 5: wasm purity asserted for only 5 of the 8 wasm-built members
**File**: `xtask/src/lint_deps.rs:176` — **Problem**: `WASM_PKGS = [csp-app, csp-view, csp-render, csp-sim, csp-ui]`,
but ci.yml:47-48 wasm-builds the workspace minus server/bench/xtask — `csp-core`, `csp-data`, `csp-script`, `csp-ai`
compile for wasm32 yet their trees are never scanned for `tokio`/`rayon`/`csp-server`. The module's own comment
justifies widening beyond the named crate "because a banned crate entering via any wasm member poisons the bundle" —
the same argument covers the four omitted members.
**Impact**: today nil (their trees are clean), but the guard is one feature-flip away from a blind spot.
**Suggestion**: derive `WASM_PKGS` from `workspace_members()` minus `{csp-server, csp-bench, xtask}`; the membership
machinery already exists.

### [SEVERITY: LOW] Finding 6: bake cross-platform float determinism is informal — fine today, relevant the moment Finding 2's check lands
**File**: `xtask/src/bin/parts/mod.rs:479,1089` (`powf`), `xtask/src/bake_art.rs:391` (`acos`), `:460-466` (Lanczos3) —
**Problem**: the mesh pipeline uses std-libm transcendentals, which are not guaranteed bit-identical across
platforms/libc.
**Evidence**: in practice the 1e-6 weld grid (`parts/mod.rs:90-99`) and the f32 output quantization absorb last-ulp
differences, `miniz_oxide` deflate is pure-Rust, and same-platform rebuild is byte-identical (idempotence verified) —
so nothing is broken now. But a future cross-machine `--check` (Finding 2) could flake on a vertex sitting exactly on
a quantization boundary.
**Suggestion**: when wiring `--check`, either scope it same-toolchain or hash the uncompressed logical payload;
switching the handful of transcendentals to `libm` (already a workspace pin) would make byte-stability formal.

### [SEVERITY: NIT] Finding 7: small tooling debts
- `xtask/src/lint_reach.rs:142` — `let _ = &issuers;` is a dead statement (issuers is used only for emptiness); drop
  it, or include the issuer list in some output where it aids fixing.
- `xtask/Cargo.toml:14-17` — comment and the `hdr` image feature reference a "Radiance .hdr sky" that no longer
  exists (the star map was removed, `bake_art.rs:609-613`); stale comment + an unused feature.
- `.github/workflows/ci.yml:15` — installs `dtolnay/rust-toolchain@stable` while `rust-toolchain.toml` pins 1.97.1;
  verified the action sets no `RUSTUP_TOOLCHAIN`, so the toolchain file wins and the stable install is redundant —
  but it reads like Appendix A.7's "CI uses floating stable" problem is back. Use the pin explicitly or comment why.

---

## PRD coverage table — PLAN §8.2 gates vs CI (the core deliverable)

Legend: WIRED (runs, matches or exceeds spec) · PARTIAL (reduced vs spec) · MISSING.

| §8.2 gate (PLAN line) | Status | Evidence |
|---|---|---|
| `cargo check` native (1748) | WIRED | subsumed by `clippy --workspace --all-targets -D warnings` (ci.yml:25) |
| `cargo check` wasm + excludes (1749) | WIRED | ci.yml:45-48, flags match spec exactly |
| wasm purity, positive (1750) | WIRED | lint-deps wasm trees + the single `wasm-bindgen-futures→tokio` parent exception (lint_deps.rs:180-192,419-444); gap: only 5 of 8 wasm members (Finding 5) |
| Dependency rules lint-deps (1751) | WIRED | ci.yml:52; fails on tooling/parse error; allowlists verified drift-free vs actual Cargo.tomls |
| Determinism lints via clippy.toml (1752) | WIRED | 4 crates' clippy.toml pinned byte-identical + complete by test (lint_deps.rs:849-906); `-D warnings` denies |
| cargo-deny (1753) | WIRED | ci.yml:60-64; deny.toml license list consistent with §3.7 |
| Rhai feature parity (1754) | WIRED | inside lint-deps (native vs wasm feature sets + banned/required features, lint_deps.rs:507-565,655-694) |
| Data validators incl. ID stability (1755) | PARTIAL | validators run via `cargo test`; **ID-stability tripwire absent (V-C32)** |
| lint-attribution (1756) | WIRED | ci.yml:53; ledger verified consistent with the procedural-art switch (no stale Kenney rows) |
| Property + unit tests §8.1(a)(b) (1757) | PARTIAL | run per-commit via `cargo test`; `proptest` crate absent — property tests are hand-rolled (depth owned by C05/C06) |
| Golden replays fast subset (1758) | WIRED+ | *every* registered scenario via `csp-replay list` (ci.yml:72-77) + in-process pinning test |
| Save round-trip determinism N=10 000 (1759) | PARTIAL | Finding 4 — round-trip + byte-stability only, no §7.5 N=10 000 equivalence test |
| Shader validation (1760) | WIRED (Baseline) | naga parse+validate test (`csp-render/src/lib.rs:476-490`) + lavapipe render gate (CSP_RENDER_GATE_REQUIRED, ci.yml:30-40); "both binding models" reduces to Baseline per the §6.4 cut ("R3 Advanced render tier → post-v1") — sanctioned |
| Code quality lint-quality (1761) | **MISSING** | no tool, no weekly job (Finding 1) |
| Coverage + ratchet (1762) | **MISSING** | no llvm-cov step; `ci/coverage-floors.json` does not exist |
| Dead code/deps (1763) | PARTIAL | clippy `-D warnings` live WIRED; machete/udeps MISSING |
| Docs (1764) | **MISSING** | no `cargo doc` step; `missing_docs` not configured in workspace lints |
| Mutation testing (1765) | **MISSING** | — |
| MSRV verify (1766) | **MISSING** | no msrv step; toolchain pinned 1.97.1 > MSRV 1.95, but 1.95 never verified |
| CPU perf budgets (1767) | WIRED cadence / DEGRADED semantics | runs per-commit (stronger than nightly) but ubuntu CI compares against the devbox baseline — machine-blind; committed log carries 2 red untriaged §5.3 rows (C29, respected) |
| Golden replays full nightly (1768) | **MISSING** | no nightly job |
| Physics oracle nightly (1769) | **MISSING** | `RapierBackend` absent; parry/rapier have zero dependents |
| Cross-platform divergence report (1770) | **MISSING** | no scheduled job |
| Baseline screenshots nightly (1771) | WIRED+ | render gate + render_dump tests per-commit on lavapipe (stronger cadence) |
| Advanced screenshots/fps, REF machines (1772) | N/A (sanctioned cut) | §6.4 cut-list item 1: "R3 Advanced render tier → post-v1" |
| §8.5 App smoke e2e `csp --smoke` (1807) | **MISSING** | V-C29 (respected) |
| §8.5 Interface e2e (1808) | WIRED+ | `csp-server/tests/e2e.rs` runs per-commit via `cargo test` |
| gen_biome_maps `--check` (adjacent to 1755) | **MISSING (wiring)** | tool coded for CI use, never run (V-DATA, respected) |

**Headline: 8 §8.2 rows entirely absent (the whole weekly/nightly tier), 4 partial, 2 adjacent e2e/data checks
missing.** The per-commit core (build, lints, deps, licenses, goldens, render gate) is genuinely solid.

## Top 3 issues
1. **Finding 1 (HIGH)** — no weekly/nightly CI tier at all: 8 §8.2 gate rows (quality, coverage, docs, mutation, MSRV,
   full goldens, physics oracle, cross-platform report) have no implementation and no schedule.
2. **Finding 2 (HIGH)** — committed `data/art/*.csar` packs have no staleness detection (no `--check`, no fingerprint,
   no CI), one `bake_art` run away from silently mixed-generation art.
3. **Finding 3 (MEDIUM)** — lint-reach false negatives: comment/string mentions count as issuers (live doc-comment
   instances in `csp-app/src/warp.rs`), and a trailing `// ui-unreachable:` swallows the variant *and* exempts the next
   one.
