# Deep Review — Cluster C21

Scope: `crates/csp-bench/build.rs`, `crates/csp-bench/src/bin/{csp-eval,csp-mcp,csp-perf,csp-replay}.rs`,
`xtask/src/{main,lint_deps,lint_attr}.rs`, `xtask/src/bin/gen_biome_maps.rs`.

Cross-checked against `crates/csp-bench/src/{replay,eval,scenario,scenarios,perf}.rs`,
`crates/csp-server/src/{backend,mcp}.rs`, `crates/csp-sim/src/lib.rs`, `crates/csp-view/src/lib.rs`,
the real `Cargo.lock`, and live `cargo tree` output.

## Findings

### [MEDIUM] Finding 1: `shorten` panics on non-ASCII details (byte-index slice)
File: `crates/csp-bench/src/bin/csp-perf.rs:82-88`
Problem: `format!("{}…", &s[..MAX])` byte-slices a `String` at a fixed offset. If byte 110 is
inside a multi-byte char, `&s[..110]` panics (`byte index … is not a char boundary`).
Evidence: `detail` strings reaching `shorten` are built as `format!("{detail}; {}", row.note)`
(perf.rs:359) and notes contain `≤` (`"wasm ≤2 s …"`, `"≤16 KiB …"`) and `—`; measured details
contain `×` (`"{bytes} B/frame × 16 Hz"`, perf.rs:958). Any detail ≥ 111 bytes with a multibyte
char straddling the cut panics mid-print instead of writing the log/verdict.
Impact: the §5.3 budget gate crashes (exit 101) with a confusing panic rather than a clean
report — and whether it fires depends on incidental byte layout.
Suggestion: truncate on a char boundary, e.g. `let cut = s.floor_char_boundary(MAX)` (nightly) or
`let cut = s.char_indices().map(|(i,_)| i).take_while(|i| *i <= MAX).last().unwrap_or(0)`, or
`s.chars().take(MAX).collect::<String>()`.

### [MEDIUM] Finding 2: `ALL_PKGS` is hand-maintained — new workspace members are silently unlinted
File: `xtask/src/lint_deps.rs:192-206` (consumed at `lint_deps`, 583-588)
Problem: the lint walks a hardcoded list of 13 crate names. A new `crates/` member added to the
workspace but not to `ALL_PKGS` produces no trees, no violations — the lint passes while §2.1
rules go unenforced for that crate. The header comment claims the opposite ("a member present in
the workspace is linted").
Evidence: `members = ["crates/*", "xtask"]` (root Cargo.toml:3) is a glob — membership grows
without touching xtask. Nothing cross-checks `ALL_PKGS` against actual members. The same blind
spot exists for `DETERMINISM_CRATES`/the clippy.toml test (new determinism crate invisible).
Impact: silent coverage gap in the §2.1 dep gate — exactly the class of silent pass the module
says it was redesigned to eliminate.
Suggestion: derive the member list from the workspace (parse root `Cargo.toml` members glob, or
`cargo metadata --no-deps`), or add a test asserting `ALL_PKGS` == on-disk `crates/*` names so a
new member fails loudly until listed.

### [LOW] Finding 3: `locked_version` pin is first-match across the whole lockfile — order-dependent
File: `crates/csp-bench/build.rs:73-125`
Problem: `pinned` is never reset per `[[package]]`; the FIRST `source`-less package (any workspace
member, path dep, or path `[patch]`) that writes a versioned `"pkg X.Y.Z"` spec wins. The correct
answer for the fingerprint is "the version csp-bench's graph resolves", not "the first member
alphabetically that pins one". If any member earlier in lockfile order (e.g. `csp-ai` < `csp-core`)
ever pins a different version of a tracked dep (`rand_chacha 0.9` via a `=0.9` bound), the pin
returns the version the determinism domain does NOT link — the same false-drift the fix removed.
Evidence: verified the fix works on the real lockfile — `csp-core` (member) writes
`"rand_chacha 0.10.0"` (line 1021), `rand 0.9.5` (external, `source =` clears `member`) is
correctly ignored (line 3166); `libm`/`blake3` single-version, parry/rapier absent → `None`.
Impact: currently correct; latent false-positive if dep graphs diverge per member. Drift is
advisory-only at verify time, so blast radius is a misleading note.
Suggestion: collect member pins per dep and prefer specs from the domain members
(csp-bench/csp-sim/csp-core/csp-data/csp-phys), or when members pin *different* versions emit
`cargo:warning=` and fall back to max rather than silently taking file order.

### [LOW] Finding 4: `locked_version` state-machine hygiene — `named` survives non-package tables
File: `crates/csp-bench/build.rs:88-104`
Problem: `named` is cleared only at `[[package]]`, not by the generic `line.starts_with('[')`
branch (which resets `member`/`in_deps`). `[[patch.unused]]`/`[[replace]]`/`[metadata]`-style
tables can contain `version = "…"`; on a malformed lockfile (`name = "pkg"` as a block's last
line before another table) that version is pushed as a version of `pkg`. Valid lockfiles always
pair `name`/`version` adjacently, so this is malformed-input-only, but the scanner is
intentionally hand-rolled and this is a one-line hardening (`named = false` in the `[` branch).
Evidence: code path inspection; `named` set at 96, consumed at 98-104, only reset at 83.
Impact: wrong fingerprint value on corrupt input — unlikely, low cost to fix.
Suggestion: `named = false` alongside `member`/`in_deps` in the `starts_with('[')` arm.

### [LOW] Finding 5: `semver_key` mis-orders release vs pre-release
File: `crates/csp-bench/build.rs:130-135`
Problem: numeric-run keys compare lexicographically, so `"1.0.0-rc.1"` → `[1,0,0,0,1]` beats
`"1.0.0"` → `[1,0,0]` — a prerelease outranks the release it precedes. Harmless for today's
tracked deps but wrong for any future release/prerelease pair.
Impact: wrong `max` in the duplicate-version fallback → wrong fingerprint.
Suggestion: treat the presence of a `-` prerelease tag as lower at equal numeric prefix (e.g.
compare `(key, has_prerelease)` tuples), or note the limitation is already doc'd "good enough".

### [LOW] Finding 6: `cargo tree` / `cargo build` invoked without `--locked`
File: `xtask/src/lint_deps.rs:470-484, 517-531`; `xtask/src/main.rs:241-253`
Problem: `cargo tree` performs a real resolution; a stale `Cargo.lock` is silently rewritten as a
side effect of a nominally read-only lint. Same for `build_web`'s `cargo build`. `--locked` turns
"lockfile out of date" into a loud failure instead of a mutation — consistent with this file's
own "fail loudly" posture.
Impact: `cargo xtask lint-deps` can dirty the tree it lints; in CI the mutation is silently
discarded so a stale lockfile never gets reported.
Suggestion: add `--locked` to the three `cargo tree` invocations and the `cargo build`.

### [LOW] Finding 7: `rhai_feature_set` pollutes the feature set with `*` from `(*)` dedup markers
File: `xtask/src/lint_deps.rs:545-564`
Problem: cargo appends `(*)` to already-expanded packages. On a dedup'd `rhai` line
(`rhai v1.27.0 feat1,feat2 (*)`), the `(*)` token → `trim_matches(['(',')'])` → `"*"` →
`features.insert("*")`. If rhai is dedup'd on one target and expanded on the other (tree shape
is target-dependent via cfg-gated deps), `native != wasm` fires a false `rhai-feature-parity`
violation; the `*` also just pollutes both sets.
Evidence: real output shows `(*)` tokens under `{p} {f}` — e.g.
`proc-macro2 v1.0.107 default,proc-macro (*)`. `t != "proc-macro"` is filtered but `*` is not.
Impact: latent false parity failure; sets contain a bogus feature.
Suggestion: skip the token when `t == "*"` (or drop `(*)`/`(*,)` tokens outright), and/or stop
after the first `rhai ` node.

### [LOW] Finding 8: `lint_attribution` resolves `data/`/`assets/` relative to caller CWD
File: `xtask/src/lint_attr.rs:217-224`
Problem: `std::fs::read_to_string("data/ATTRIBUTION.md")` and `collect_files(Path::new("data"))`
assume CWD == repo root. `cargo run -p xtask`/`cargo xtask` runs the binary in the caller's
directory, so invoking from `crates/` (still inside the workspace — `cargo tree` in `lint_deps`
still works there) panics with exit 101 rather than a clean lint result. Every other task here
(`bless_goldens`, `build_web`, `gen_biome_maps`) anchors to `env!("CARGO_MANIFEST_DIR")`.
Impact: inconsistent, confusing failure mode outside the repo root; CI (root-invoked) unaffected.
Suggestion: derive `root` like `bless_goldens` does and join `data`/`assets` under it.

### [LOW] Finding 9: `csp-eval` arg gaps — `--transcript` ignored in suite mode; positional + `--suite` both accepted; empty suite passes
File: `crates/csp-bench/src/bin/csp-eval.rs:66-92`
Problem: (a) `--suite dir --transcript out.json` — `transcript_out` is parsed then silently dropped
(the suite branch returns before using it), though the doc says transcript is single-scenario
only. (b) `csp-eval a.ron --suite d` — the positional is silently discarded though usage shows
them as mutually exclusive. (c) `--suite` on a dir containing zero `*.ron` files returns
`all_pass == true` → exit 0 — an empty/mispointed suite silently greens the gate, the exact
"pass on empty output" anti-pattern `lint_deps` guards against.
Impact: user thinks a transcript was written; a malformed invocation silently narrows the run;
a missing scenario set passes CI.
Suggestion: reject `--transcript` when `suite_dir.is_some()`; reject positional when `--suite`
given; treat `entries.is_empty()` as a usage/harness error (exit 2).

### [LOW] Finding 10: `bless-goldens` — a partial artifact dir is treated as a brand-new golden
File: `xtask/src/main.rs:103-152` (esp. 109, 139-152)
Problem: both the orphan check and the "existing vs new" decision key solely on
`replay.bin` existing (`dir.join(REPLAY_FILE).exists()`). A dir holding
`expected-hash.txt`/`fingerprint.json` but a lost `replay.bin` is classified `old_expected=None`
→ "new golden" → blessed in with no schema-bump/hash-change refusal — the §8.4 refusal is
bypassed by a partially-corrupt golden.
Impact: edge case, but it is a hole in the very refusal this task exists to enforce.
Suggestion: treat ANY of `ARTIFACT_FILES` present as "existing": try `load_golden`, and if the
set is incomplete, refuse loudly (exit 2) rather than blessing over the remnants.

### [LOW] Finding 11: `csp-mcp` reconfigure swallows `load_balance` failure; comment overstates §7.3
File: `crates/csp-bench/src/bin/csp-mcp.rs:74-84`
Problem: `if let Ok(bal) = load_balance(...)` silently skips the budget reinstall — a fixture
`balance.ron` that fails to reload leaves the engine installed with the snapshot's budget (or
the default), diverging from the "re-install fixture config" contract with no warning.
Separately, the comment says budgets are "never in the snapshot" — but `script_op_budget` IS
serialized (`lib.rs:2883`, restored at 2780); the comment is inaccurate for that field even
though the overwrite-with-fixture-value behavior is defensible.
Impact: silent config divergence on a broken fixture; misleading §7.3 documentation.
Suggestion: `match load_balance` and `eprintln!` a warning on `Err`; fix the comment to name
what actually isn't serialized (engine/driver trait objects).

### [LOW] Finding 12: `gen_biome_maps` — palette index unbounded; sloppy flag handling
File: `xtask/src/bin/gen_biome_maps.rs:146-158, 279-288`
Problem: (a) `rasterize` pushes whatever `(spec.pixel)` returns with no check against
`spec.palette.len()` — a hand-edit returning index ≥ palette size yields a PNG with dangling
palette indices (invalid per many decoders), and `--check` can't catch it since output is
deterministically wrong. (b) `args().any(|a| a == "--check")` ignores unknown flags entirely —
a typo (`--chek`, or any junk) silently runs WRITE mode instead of erroring.
Impact: hand-maintained tables have no bounds guardrail; mistyped invocations write files.
Suggestion: assert `idx < palette.len()` in `rasterize` (or in `encode_indexed_png`), and accept
only `[]` or `["--check"]` — reject anything else with usage + exit 2.

### [LOW] Finding 13: `build_web` leaves a partial `dist/` when wasm-bindgen fails or is absent
File: `xtask/src/main.rs:261-303`
Problem: `index.html` is copied into `dist/` *before* `wasm-bindgen` runs. On bindgen
absence/failure the task exits non-zero and prints recovery commands, but `dist/` still
contains `index.html` (plus any stale bundle from a prior run — `dist` is never cleaned) — a
half-artifact that could be mistaken for a valid package, contra the comment's "never a silent
half-artifact".
Impact: stale/partial bundle on disk after a failed packaging run.
Suggestion: copy `index.html` only after a successful `wasm-bindgen`, and/or clear `dist/` first.

## Verified-correct (no findings)

- `locked_version` dup-version fix: on the real lockfile, `rand 0.9.5`'s `"rand_chacha 0.9.0"`
  spec is correctly excluded by the `source =` gate and `csp-core`'s `"rand_chacha 0.10.0"` pin
  wins — the tungstenite-transitive false positive is genuinely fixed.
- `parse_tree` `[build-dependencies]` headers: verified empirically — cargo renders them as
  continuation-column `│   [build-dependencies]` (no `├`/`└`), so `trim_start_matches(['│',' ','─'])`
  + `starts_with('[')` skips them correctly and build deps land under the right parent.
- Comma-tokenizer fix in `rhai_feature_set` (line 558): confirmed against live output
  `rhai v1.27.0 core-error,hashbrown,libm,no-std-compat,no_std,no_time` — features split correctly;
  `"rhai "` prefix can't match `rhai_codegen`.
- `bless-goldens` ordering: dirty-tree → stage (in-memory `record()` self-verifies) → schema/hash
  refusal → write; refusals run before any byte is written, as documented. `--reason` correctly
  requires non-empty text and only overrides the hash-change refusal.
- `csp-replay` exit-code mapping (0/1/2), arg arity, and the best-effort fingerprint notes on
  HashMismatch are all correct.
- `csp-mcp` reconfigure covers the real §7.3 gap: `restore` re-derives orbits/physics from
  `world.config()` (backend.rs:365, 251-263) and resets `script_engine`/`autopilot_driver` to
  `None` (lib.rs:2785-6); JSON-RPC parse errors return -32700 and keep serving.
- `gen_biome_maps` encoder: CRC32/Adler-32/zlib-stored-block/PNG chunk layout all verified
  correct (0x7801 header valid, ≤65535 stored blocks, BFINAL on last, Adler BE, filter byte 0,
  colour type 3).
- `lint_attr` rule engine: suffix-vs-extension authored check, stale rows, scope check,
  bad-row hard failure, and the no-markers violation all behave as documented.
- `csp-mcp`/`LocalGame::with_reconfigure` ordering — `set_script_op_budget` precedes
  `set_script_engine`, and `set_script_engine` pushes the budget into the engine (lib.rs:2108),
  so the fixture budget lands correctly.
