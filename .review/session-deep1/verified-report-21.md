# Verified Report — Cluster C21

Verifier's verdict per finding in `review-report-21.md`, checked against the
actual sources (paths verified under `/home/didi/workspace/Clanker-Space-Program`):
`crates/csp-bench/build.rs`, `crates/csp-bench/src/bin/{csp-eval,csp-mcp,csp-perf,csp-replay}.rs`,
`xtask/src/{main,lint_deps,lint_attr}.rs`, `xtask/src/bin/gen_biome_maps.rs`,
root `Cargo.toml`, `Cargo.lock`, `crates/csp-bench/src/{eval,perf}.rs`,
`crates/csp-sim/src/lib.rs`, `crates/csp-server/src/backend.rs`, and live
`cargo tree` output.

## Verdict summary

| # | Severity | Verdict |
|---|----------|---------|
| 1 | MEDIUM | CONFIRMED (latent — no shipped detail currently straddles byte 110) |
| 2 | MEDIUM | CONFIRMED (list is in sync today; drift would be silent) |
| 3 | LOW | CONFIRMED |
| 4 | LOW | CONFIRMED |
| 5 | LOW | CONFIRMED (one immaterial numeric detail corrected) |
| 6 | LOW | CONFIRMED (2 cargo-tree call sites, not 3 — substance stands) |
| 7 | LOW | CONFIRMED (verified against live cargo tree output) |
| 8 | LOW | CONFIRMED |
| 9 | LOW | CONFIRMED |
| 10 | LOW | CONFIRMED |
| 11 | LOW | CONFIRMED |
| 12 | LOW | CONFIRMED |
| 13 | LOW | CONFIRMED |

No finding was dismissed. Details and nuances below.

---

### Finding 1 — `shorten` panics on non-ASCII details — **CONFIRMED**

`crates/csp-bench/src/bin/csp-perf.rs:82-89`: `format!("{}…", &s[..MAX])` with
`MAX = 110` is a byte-index slice; Rust panics with
`byte index 110 is not a char boundary` when byte 110 is inside a multibyte
char. Inputs reaching `shorten` are `PerfEntry.detail` strings built at
`perf.rs:359` as `format!("{detail}; {}", row.note)` (and `row.note` verbatim
for gated rows at `perf.rs:348`, `"harness error: {e}"` at `perf.rs:372`).

Multibyte chars are confirmed present in the real strings: `≤` in the
`weld.stage2-300-part` note (perf.rs:145) and `telemetry.1000-part` note
(perf.rs:154), `—`/`§` in the `warp.20-colonies.events` note (perf.rs:120-121)
and every gated-row note, `×` in the telemetry measured detail (perf.rs:958),
`⁵` in scene strings.

Nuance the report got right but that bounds severity: several details do
exceed 110 bytes today (sim.200-part-atmo ≈ 150 B, weld.stage2 ≈ 160 B,
warp.events ≈ 124 B), yet the multibyte chars all sit at byte offsets < 100 —
no *current* authored detail places a multibyte char across byte 110, so the
panic is latent, not firing. It is real and reachable: digit-count growth in
measured fields (`{measured} ticks`, `{total_events}`) or any note edit shifts
the boundary. The fix suggestions are valid (`str::floor_char_boundary` is
nightly-only; `char_indices`/`chars().take` work on stable). MEDIUM is a fair
call: it is a genuine panic path on user-visible output, gated by incidental
layout.

### Finding 2 — `ALL_PKGS` is hand-maintained — **CONFIRMED**

`xtask/src/lint_deps.rs:192-206` hardcodes 13 names; consumed at
`lint_deps` lines 583-588. Root `Cargo.toml:3` is `members = ["crates/*", "xtask"]`
— a glob, so membership grows without touching xtask. Nothing cross-checks
`ALL_PKGS` against actual members.

Current state verified in sync: `crates/` contains exactly 12 members
(csp-ai, csp-app, csp-bench, csp-core, csp-data, csp-phys, csp-render,
csp-script, csp-server, csp-sim, csp-ui, csp-view) + `xtask` = the 13 listed.
A 14th member would indeed produce no tree, no violations, no error — the
lint passes silently. The header comment (189-191) does claim "a member
present in the workspace is linted," which is unenforced. The companion blind
spot is also real: `DETERMINISM_CRATES` (line 59) and the clippy.toml test's
hardcoded `["csp-core","csp-data","csp-phys","csp-sim"]` (lines 786, 811)
share it.

### Finding 3 — `locked_version` first-match pin is order-dependent — **CONFIRMED**

`crates/csp-bench/build.rs:73-125`: `pinned` is never reset per `[[package]]`;
the `pinned.is_none()` guard at line 112 makes the first workspace member
(`source`-less block) writing a `"pkg X.Y.Z"` spec win, in lockfile order.

Verified against the real `Cargo.lock`: member blocks are alphabetically
ordered (csp-ai L963 … csp-bench L997, csp-core L1016 …); only csp-core pins
`"rand_chacha 0.10.0"` (L1021); `rand 0.9.5`'s `"rand_chacha 0.9.0"` (L3166)
is correctly ignored because its `source =` line (L3163) clears `member`
before the deps block is scanned. So today's pin is correct — but if e.g.
csp-bench (earlier in file order) ever pins a divergent `rand_chacha`, the
fingerprint picks the version the determinism domain does not link. Latent,
advisory-only impact — correctly scoped LOW.

### Finding 4 — `named` survives non-package tables — **CONFIRMED**

`build.rs:88-94`: the generic `starts_with('[')` arm resets `member` and
`in_deps` but not `named`. `named` is cleared only at `[[package]]` (line 83)
and after consuming a version (line 103). A malformed lockfile with
`name = "pkg"` as a block's last line followed by `version = "…"` in another
table would push a wrong version. Malformed-input-only; the suggested
one-line hardening is correct.

### Finding 5 — `semver_key` mis-orders release vs pre-release — **CONFIRMED**

`build.rs:130-135`: numeric-run key. `"1.0.0-rc.1"` → `[1,0,0,1]` vs
`"1.0.0"` → `[1,0,0]`; `Vec` ordering puts the longer equal-prefix key last,
so the prerelease wins `max`. (Correction to the report's arithmetic: the key
is `[1,0,0,1]`, not `[1,0,0,0,1]` — `"rc"` contributes no digits — but the
ordering conclusion is unchanged.) Harmless for today's tracked deps;
doc'd "good enough" already hedges it. LOW confirmed.

### Finding 6 — no `--locked` on cargo tree / cargo build — **CONFIRMED**

`xtask/src/lint_deps.rs:470-484` (`tree()`) and `517-531` (`rhai_feature_set`)
invoke `cargo tree` without `--locked`; `xtask/src/main.rs:241-253` invokes
`cargo build` without it. `cargo tree` performs a real resolution and will
rewrite a stale `Cargo.lock` as a side effect — a nominally read-only lint
can dirty the tree. Minor wording fix: there are **two** cargo-tree call
sites (the report's "three invocations" presumably counted `tree()` +
`rhai_feature_set()` × 2 targets; in code it's 2 sites / 20 runtime calls).
`cargo tree` and `cargo build` both accept `--locked`; substance confirmed.

### Finding 7 — `(*)` dedup marker pollutes `rhai_feature_set` — **CONFIRMED**

`xtask/src/lint_deps.rs:543-565`: for a line `rhai v1.27.0 f1,f2 (*)`, the
`(*)` token → `trim_matches(['(',')'])` → `"*"` → `split(',')` → `"*"` →
`features.insert("*")` (lines 558-562; only `""` and `"proc-macro"` are
filtered). Verified against **live** `cargo tree -p csp-script --edges normal
--format "{p} {f}"`: `(*)` tokens are real (`proc-macro2 v1.0.107
default,proc-macro (*)`), rhai currently appears expanded once
(`rhai v1.27.0 core-error,hashbrown,libm,no-std-compat,no_std,no_time`), and
`rhai_codegen` cannot false-match the `"rhai "` prefix. If rhai is ever
dedup'd asymmetrically across targets (target-dependent tree shape via
cfg-gated deps), `native != wasm` fires a false `rhai-feature-parity`
violation; symmetric dedup just pollutes both sets with `"*"`. Latent —
confirmed.

### Finding 8 — `lint_attribution` resolves `data/`/`assets/` relative to CWD — **CONFIRMED**

`xtask/src/lint_attr.rs:217-224`: `read_to_string("data/ATTRIBUTION.md")`,
`collect_files(Path::new("data"), …)`, `Path::new("assets")` — all relative
to the process CWD, and the read uses `.expect(...)` → panic/exit 101 from a
non-root CWD. Confirmed the inconsistency: `bless_goldens` (main.rs:69-72),
`build_web` (main.rs:237-240), `gen_biome_maps` (`home_dir`, lines 245-252),
and even the lint_deps tests (lint_deps.rs:783-785) all anchor to
`env!("CARGO_MANIFEST_DIR")`. `lint_deps`' `cargo tree` works from any
subdir (cargo walks up), so an in-workspace non-root invocation is a
plausible failure mode. Confirmed.

### Finding 9 — `csp-eval` arg gaps — **CONFIRMED** (all three sub-points)

`crates/csp-bench/src/bin/csp-eval.rs`:

- (a) `--transcript` is parsed (60-65) but the suite branch (73-93) returns
  before `transcript_out` is used (102-106) — silently dropped. The doc
  comment (line 17) does say "single-scenario mode only," so it is a
  documented-but-unenforced restriction — silently ignored rather than
  rejected, exactly as reported.
- (b) `csp-eval a.ron --suite d`: positional fills `scenario_path` (66-68),
  `--suite` fills `suite_dir`; the suite branch wins and the positional is
  silently discarded. USAGE shows them as alternatives
  (`<scenario.ron | --suite <dir>>`).
- (c) `crates/csp-bench/src/eval.rs:212-228`: `run_suite` on a dir with zero
  `*.ron` files returns `Ok(vec![])` → `all_pass` stays `true` → exit 0.
  An empty/mispointed suite greens the gate. Confirmed.

### Finding 10 — partial golden dir treated as brand-new — **CONFIRMED**

`xtask/src/main.rs`: both the orphan check (line 109) and the
existing-vs-new decision (139-152) key solely on
`dir.join(REPLAY_FILE).exists()`. A dir retaining `expected-hash.txt` /
`fingerprint.json` but missing `replay.bin` yields `old_expected=None` →
classified as a new golden → `check_schema_version_refusal` skips it →
`write_golden` overwrites the remnants with no hash comparison — the §8.4
hash-change refusal is bypassed. Edge case but correctly identified.

### Finding 11 — `csp-mcp` reconfigure swallows `load_balance` failure; comment wrong — **CONFIRMED**

`crates/csp-bench/src/bin/csp-mcp.rs:74-84`: `if let Ok(bal) =
csp_data::load_balance(&dir.join("balance.ron"))` silently skips the budget
reinstall on `Err` — no warning, engine left with the snapshot/default
budget. Comment claim verified wrong: `script_op_budget` **is** serialized
(`csp-sim/src/lib.rs:2883` writes it into the snapshot; `lib.rs:2780`
restores it). What genuinely isn't in the snapshot is `script_engine` /
`autopilot_driver` (reset to `None` at lib.rs:2785-2786). So "budgets …
never in the snapshot" is inaccurate for that field even though
reinstalling the fixture value is defensible. Both halves confirmed.

### Finding 12 — `gen_biome_maps` unbounded palette index + sloppy flags — **CONFIRMED**

`xtask/src/bin/gen_biome_maps.rs`: `rasterize` (147-158) pushes
`(spec.pixel)(lon, lat)` with no check against `spec.palette.len()`;
`encode_indexed_png` (221-243) writes PLTE from `palette` and IDAT indices
from `pixels` with no cross-validation — a hand-edit returning an index
≥ palette size yields a PNG with dangling palette indices, byte-identically
reproduced so `--check` cannot catch it. And `main` (line 280)
`args().any(|a| a == "--check")` ignores every other arg — a typo
(`--chek`, or any junk) silently runs WRITE mode. Both confirmed.

### Finding 13 — `build_web` leaves a partial `dist/` on bindgen failure — **CONFIRMED**

`xtask/src/main.rs:261-303`: `dist/` is created and `index.html` copied at
263-270, *before* `wasm-bindgen` runs at 271-303. On bindgen
absence/failure the task exits non-zero (loudly, with recovery commands —
that part is honest), but `dist/` still holds `index.html` plus any stale
bundle from a prior run (never cleaned). The doc comment's "never a silent
half-artifact" is about loudness, but the on-disk half-artifact claim in the
finding is accurate. Confirmed.

## Re-checks of the report's "verified-correct" section (spot audit)

- `Cargo.lock` claims re-verified: csp-core pins `rand_chacha 0.10.0`
  (L1021), `rand 0.9.5` carries `rand_chacha 0.9.0` (L3166) behind a
  `source =` line — the exclusion works as described.
- Live `cargo tree` output confirmed `{p} {f}` renders `(*)` markers and
  comma-joined feature tokens exactly as the report claims; `rhai ` prefix
  does not match `rhai_codegen`.
- `csp-sim` snapshot round-trip confirmed: `script_op_budget` at lib.rs:2883
  (out) / 2780 (in); `script_engine`/`autopilot_driver` forced `None` at
  2785-2786; `reconfigure` hook invoked at `csp-server/src/backend.rs:369`
  and stored via `with_reconfigure` (452-453).
- `bless_goldens` ordering in `main.rs:60-77` confirmed: dirty-tree refusal
  → stage (self-verifying `record()`) → schema/hash refusal → write; refusals
  precede all writes.

## Caveats / corrections to the original report

1. Finding 5's example key is `[1,0,0,1]` not `[1,0,0,0,1]` (immaterial).
2. Finding 6's "three cargo tree invocations" = 2 call sites (one runs for
   every pkg, the other twice) — the fix is still "add `--locked` to both
   functions and `build_web`."
3. Finding 1 is a real panic path but currently latent: no shipped detail
   string places a multibyte char across byte 110 today — verified by
   reconstructing each `>110`-byte detail from perf.rs. The MEDIUM severity
   stands on the strength of it being a crash-before-artifact-write on the
   §5.3 gate, triggered by incidental layout.
4. Finding 2's exposure is currently nil (13 listed = 13 actual) — it is a
   *staleness trap*, correctly framed.
