# Review Report 19 — xtask tooling (bless-goldens, lint engines, biome maps, build-web)
# Reviewer: ruthless-static — session-ruthless-20261025
# Scope: xtask/src/main.rs, lint_deps.rs, lint_attr.rs, gen_biome_maps.rs
# Date: 2026-10-25

---


### [SEVERITY: HIGH] Finding 1: bless-goldens doc comment endorses `csp-replay record`, which bypasses every §8.4 refusal
**File**: xtask/src/main.rs:36-43 (and crates/csp-bench/src/bin/csp-replay.rs:51-69)
**Problem**: `bless_goldens`' doc comment documents `csp-replay record <name> <dir>` as an *equivalent* golden-writing workflow: "Equivalently, `csp-replay record <name> <dir>` writes them directly and the commit carries them; bless afterwards only refreshes fingerprints." But `csp-replay record` (csp-replay.rs:60-61) calls `replay::record` + `write_golden` with none of the refusals xtask enforces: no dirty-tree check, no partial-artifact check, no hash-change-without-`HASH_SCHEMA_VERSION`-bump refusal, no `--reason`.
**Evidence**: csp-replay.rs:60 `let golden = replay::record(recipe, dir)?;` then `replay::write_golden(dir, &golden)?;` — nothing compares against the previously committed `expected-hash.txt`. main.rs:214-224 is the only place the schema gate exists.
**Impact**: The entire §8.4 refusal scheme is advisory. A developer (or AI agent following this doc comment) who breaks determinism can overwrite `expected-hash.txt` via `csp-replay record` with no gate at all — the regression ships silently, exactly what the refusal exists to prevent. Only `record`'s self-check applies (the new artifact replays against itself), which says nothing about drift from the committed golden.
**Suggestion**: Either (a) route `csp-replay record`'s write through a shared refusal (load old expected-hash when present, refuse on hash change without schema bump / `--reason`), or (b) delete the "Equivalently" sentence from main.rs and add a refusal to bless-goldens that detects goldens whose committed hash can't be traced to a blessed run — minimally, make the doc say `csp-replay record` is bootstrap-only for NEW goldens and refuses when `expected-hash.txt` already exists.
