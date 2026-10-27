# Verified report C35 (session-full-20261026)

Verifier: independent re-derivation against working tree at HEAD 7dd4b86 (2026-10-26).
Method: every file:line opened and read in context; parser behavior reproduced empirically with a
standalone compiled copy; both xtask lints executed; git history mined to quantify Finding 2;
workspace dep graph mapped to test Finding 5. No repo files were modified.

Score: 6 CONFIRMED, 1 DISMISSED (Finding 5). One positive-verification sentence in the report's
preamble corrected (lint-deps "no drift in either direction" — see note at the end).

---

### Finding 1: The weekly/nightly half of §8.2 does not exist — CONFIRMED
**Original**: only `push`/`pull_request` triggers; no schedule/cron; no `ci/` dir; 8 §8.2 rows have
no implementation (Code quality 1761, Coverage 1762, Docs 1764, Mutation 1765, MSRV 1766, Golden
full nightly 1768, Physics oracle 1769, Cross-platform report 1770); machete/udeps half of 1763
missing; §6.4 sanctions none of it.

**Verification**:
- `.github/` contains exactly one file: `workflows/ci.yml`. Triggers: `on: push, pull_request` only.
  Two jobs (`check`, `perf`), both push/PR — no `schedule` anywhere. No `ci/` directory exists
  (`ls ci` → not found), so `ci/coverage-floors.json` (PLAN:1762) is nonexistent by construction.
- I re-enumerated PLAN §8.2 myself (lines 1746-1772, 25 gate rows). The 8 MISSING rows named by the
  report are exactly the 8 rows with no implementation:
  - 1761 `xtask lint-quality`: not coded — `xtask/src/main.rs:393-405` dispatches only
    lint-deps/lint-attribution/lint-reach/bless-goldens/build-web. No `cargo-geiger` anywhere.
  - 1762 Coverage: no llvm-cov step in ci.yml; no floors file.
  - 1764 Docs: no `cargo doc` step; `grep -rn missing_docs` over all toml/rs → zero hits (not
    configured in workspace lints).
  - 1765 Mutation: no cargo-mutants.
  - 1766 MSRV: no msrv step; `rust-toolchain.toml` pins channel `1.97.1`, workspace
    `rust-version = "1.95"` (Cargo.toml:7) is never verified.
  - 1768 Golden full nightly: no nightly job (ci.yml:72-77 runs the *full* registered set
    per-commit — stronger cadence than spec, but the nightly row's longer scenarios, e.g. colony
    century, exist nowhere).
  - 1769 Physics oracle: `RapierBackend` appears only in doc comments (csp-phys/src/backend.rs:20,50,
    58,176; csp-sim/src/joints.rs:14) — no type exists; `parry3d-f64`/`rapier3d-f64` are workspace
    declarations with the comment "Workspace declaration only — no crate depends on these yet"
    (Cargo.toml:59-70).
  - 1770 Cross-platform report: no scheduled/hosted job.
- Half-missing 1763: `clippy -D warnings` wired (ci.yml:25); `cargo-machete`/`cargo-udeps` absent.
- §6.4 cut list (PLAN:1543-1561) sanctions only: Advanced render tier, life support, third body,
  M6 docking (keeping proximity transfer), web tier polish. Nothing about CI tiers — confirmed.
- **Nuance (report slightly undersells)**: rows 1762 (Coverage) and 1764 (Docs) are specified
  *per-commit* in PLAN, not weekly/nightly. Their absence is not "slow gates moved off the
  per-commit path" — they are per-commit gates with no implementation at all.

**Verdict**: CONFIRMED as [HIGH]. The count "8 rows" is exact.

### Finding 2: Baked-art staleness is undetectable — CONFIRMED
**Original**: `data/art/*.csar` committed and embedded via `include_bytes!`, no `--check`, no
fingerprint, no CI; decoder falls back only on decode failure; sibling gen_biome_maps has `--check`
explicitly for CI.

**Verification**:
- `xtask/src/bin/bake_art.rs:590` `fn main()` takes no arguments and writes unconditionally
  (`atomic_write` at :653-654). No `--check` path exists anywhere in the binary.
- `crates/csp-view/src/art.rs:87-88`: `include_bytes!("../../../data/art/parts.csar")` /
  `terrain.csar` — confirmed. Fallback to procedural/kitbash only on `decode()` Err (art.rs:103,
  :120). A valid-but-stale pack loads normally and renders wrong with zero signal — the decode
  path has no notion of source freshness.
- Repo-wide search for any staleness mechanism: the only references to the packs outside
  csp-view/xtask are a doc comment (csp-render/src/terrain.rs:397) and
  `csp-data/src/datahash.rs`, which *excludes* `data/art/**` files with art extensions from the
  data-set hash (lines 27-31) — i.e. the sim's own fingerprint deliberately ignores the packs. No
  sidecar hash, no test byte-comparing against a rebake (the `packs_decode` test at art.rs:445-455
  only asserts 5 of 12 model keys decode and terrain entries exist — it pins decode/schema drift of
  the committed bytes against the decoder, not freshness against the sources).
- `gen_biome_maps.rs:267-290` implements `--check` with byte-compare and a stale list, plus strict
  flag parsing at :292-299 — the exact mechanism bake_art lacks. Confirmed contrast.
- 12 vs 16 confirmed: `build_all` (parts/mod.rs:1290-1317) authors exactly 12 keys; `data/parts/`
  contains 16 RON files; `art.rs:375-435 model_key` collapses the difference by id-substring/kind
  fallthrough (Port→decoupler, Crew→pod, `.tank.`→tank …). A new part modeled in `parts/` without a
  rebake silently keeps the reused silhouette; a new *key* without a rebake silently falls back.
- The intra-run guard comment (bake_art.rs:596-601, C19bake-F3) is present verbatim — the code
  guards mixed generations within one run but nothing guards across commits.
- **Quantification (asked for)**: git history — 6 commits touched the bake inputs
  (bake_art.rs ∪ parts/ ∪ assets/src): b644a73, 20660df, 396e12c, 1e8c6da, 6df2d2e, ac7a5ae.
  5 of them also committed `data/art/`. **1 (6df2d2e, +100/-41 in bake_art.rs — including real
  pipeline changes like the subdivision-stage removal) shipped without any rebake.** Whether the
  packs were byte-stale at that commit is unprovable in both directions — which is the finding.
  Current tree is fresh in shape (pack headers read: terrain.csar = 3 entries
  albedo/normal/water_nrm, no removed sky entry; parts.csar = 12 entries, first `engine_a`), so
  the risk is prospective and recurring (~1 in 6 historically).

**Verdict**: CONFIRMED as [HIGH].

### Finding 3: lint-reach has two false-negative classes — CONFIRMED
**Original**: (a) a line *containing* `ui-unreachable:` is consumed before variant-row detection;
(b) issuer detection is `text.contains("SimCommand::Name")` over comment-bearing non-test source.

**Verification**:
- (a) **Empirically reproduced.** I compiled the parser verbatim (`sim_command_variants`,
  lint_reach.rs:46-88) standalone and ran it on:
  `pub enum SimCommand { Foo, // ui-unreachable: reserved for scripts | Bar, | Baz, }`.
  Output: `[("Bar", Some("reserved for scripts")), ("Baz", None)]`.
  Two simultaneous silent failures: `Foo` vanishes from the checked set entirely (marker check at
  :61-64 `continue`s before row detection at :78-85), and the pending annotation binds to `Bar`,
  exempting it from the issuer requirement. A repo that regresses both `Foo` and `Bar` issuers
  stays green. The marker check also fires on payload lines and before the `d0 != 1` guard, so a
  marker anywhere inside the enum body poisons the next variant row.
- (b) Confirmed by code: `production_sources` (:94-112) only cuts the file at `#[cfg(test)]`; no
  comment/string stripping; issuer test is `text.contains(...)` at :133. Live comment-class
  instances verified in producer crates: `crates/csp-app/src/warp.rs:2-3` (module doc naming
  `SimCommand::SetWarp`/`SimCommand::AdvanceTo`), `warp.rs:192,310,344` (doc comments), and
  `crates/csp-ui/src/settings.rs:219` (`SimCommand::TakeControl` in a variant doc). These variants
  have real constructions today (csp-app/src/lib.rs:572,910,1315,4660…), so no live false pass
  exists *now* — I classified all 53 variants: 44 have code-site issuers, 9 carry annotations
  (lint-reach: OK verified by running it), 0 are comment-only — exactly the prospective framing
  the report used: delete the real constructions, keep the docs, lint stays green.

**Verdict**: CONFIRMED as [MEDIUM].

### Finding 4: §7.5 save round-trip N=10 000 gate not implemented as specified — CONFIRMED
**Original**: PLAN:1759 promises "§7.5, N = 10 000" per-commit; what exists is single-shot
round-trip equality and byte-stability, not reload-vs-uninterrupted hash comparison at N=10,000
across scenarios.

**Verification**:
- PLAN:1671-1677 (§7.5): "Test (per-commit): save → load → N ticks, hash-compared against
  uninterrupted N ticks, for N = 10 000, across the scenario set." Confirmed normative text.
- save.rs:618 `save_round_trip_current` (encode→decode equality) and ~:650
  `save_encoding_is_byte_stable` (double-encode equality) confirmed — neither ticks a world.
- The four `0..10_000` loops at lib.rs:15421/15620/15682/15937 opened and read: all are
  spawn→settle/latch observation loops on landing fixtures, no save/load involved. Confirmed.
- Repo-wide search for any reload-vs-uninterrupted test found exactly one miniature:
  `seam_record_round_trips_save_load` (lib.rs:19513) — snapshot→restore→step 64 ticks→state_hash
  equality, single merged-vessel fixture. It is a germ of the §7.5 gate (and the report's evidence
  section omits it), but it is N=64 on one fixture, not N=10,000 across the scenario set. The
  PARTIAL row stands; the §7.5 named bug class ("a cache that is a function of history rather
  than of state") has no tripwire at the specified scale.

**Verdict**: CONFIRMED as [MEDIUM] (PARTIAL row correct; note the seam test as existing partial
coverage).

### Finding 5: wasm purity asserted for only 5 of the 8 wasm-built members — DISMISSED
**Original**: `WASM_PKGS` covers 5 members; csp-core/csp-data/csp-script/csp-ai "compile for
wasm32 yet their trees are never scanned"; guard is one feature-flip from a blind spot.

**Verification** (dismissal on two grounds):
1. **The count is wrong, and in the wrong direction for the finding.** Workspace members =
   `crates/*` (12 crates) + xtask. CI wasm-builds all minus {csp-server, csp-bench, xtask} →
   **10** wasm-built members (the report's "8" forgot csp-phys and also that apps/* are not
   workspace members). Unscanned *roots*: 5 (csp-core, csp-data, csp-phys, csp-script, csp-ai),
   not 4.
2. **The substantive claim ("their trees are never scanned") is false.** The purity check
   (lint_deps.rs:416-444) iterates *all* `t.nodes` of each scanned root's
   `cargo tree --target wasm32-unknown-unknown` output — it is a transitive check by construction.
   Dep graph verified from the Cargo.tomls: `csp-view` (a scanned root) directly depends on
   csp-core, csp-sim, csp-script, csp-ai, csp-data, csp-ui, csp-render, csp-phys — i.e. every one
   of the five "omitted" members sits inside a scanned tree, and any banned crate entering through
   them is reported with its parent edge. Target-gated deps are correctly pruned by cargo tree
   (csp-app's `csp-server` dep lives under `cfg(not(target_arch = "wasm32"))` — verified — which
   is precisely why the scan passes today). The "feature-flip blind spot" scenario does not
   materialize: a default-on feature propagates into the parent's resolved tree (caught); a
   default-off feature is not built by the CI wasm check either (nothing to guard).
- Residual value of the suggestion (derive `WASM_PKGS` from workspace members): future-proofing
  against a *new* wasm member not reachable from csp-view/csp-app — real but speculative; that is
  a hardening nit, not a present gap. The module's own comment (lint_deps.rs:172-175) already
  states the poisoning rationale that the transitive scan implements.

**Verdict**: DISMISSED (optionally refile as nit: derive the root list from workspace members).

### Finding 6: bake cross-platform float determinism is informal — CONFIRMED
**Original**: std-libm transcendentals (`powf`, `acos`, Lanczos3) make cross-machine byte-stability
unproven; fine today (weld grid + f32 quantization absorb ulps, miniz_oxide pure-Rust); relevant
once a cross-machine `--check` lands.

**Verification**: all sites opened and confirmed: `t.powf(2.4)` (parts/mod.rs:479),
`powf(n_exp)`/`powf(-1.0/n_exp)` (:1089), `dot(...).acos()` (bake_art.rs:391), Lanczos3 resize in
`load_rgba8` (bake_art.rs:460-466), 1e-6 weld grid (parts/mod.rs:88-99). `xtask/Cargo.toml` has no
libm dependency (image/miniz_oxide/csp-bench/serde_json only), while `libm = "=0.2.16"` is a
workspace pin (Cargo.toml:31) — the suggested fix is available. Nothing is broken today (the
report itself scopes this as forward-looking w.r.t. Finding 2). Correctly rated [LOW].

**Verdict**: CONFIRMED as [LOW].

### Finding 7: small tooling debts — CONFIRMED
- lint_reach.rs:142 `let _ = &issuers;` — present verbatim; issuers used only for emptiness.
- xtask/Cargo.toml:8-17 — comment names "(JPG source textures, Radiance .hdr sky)" and the image
  feature set includes `"hdr"`; no `.hdr` file exists anywhere under assets/ or data/, and no HDR
  decode call exists in either xtask bin (only PNG `IHDR` chunk names in gen_biome_maps.rs:237-241).
  The sky was removed (bake_art.rs:609-613 comment). Stale comment + unused feature confirmed.
- ci.yml:15 installs `dtolnay/rust-toolchain@stable` while `rust-toolchain.toml` pins `1.97.1`;
  the action sets no toolchain override, so the file wins and the stable install is redundant
  noise that reads like the floating-stable problem. Confirmed as written.

**Verdict**: CONFIRMED as [NIT] (all three sub-items).

---

## PRD coverage table verification

- **8 MISSING rows re-derived independently**: 1761, 1762, 1764, 1765, 1766, 1768, 1769, 1770 — all
  verified absent (see Finding 1). With Finding 5 dismissed, the 1750 row's parenthetical gap note
  ("only 5 of 8 wasm members") should be deleted; the row is WIRED without qualification.
- **4 PARTIAL rows verified**: 1755 (ID-stability tripwire absent — no `Deprecated`/baseline-ID
  mechanism anywhere in csp-data; the validate.rs functions *labeled* "§8.2 ID stability"
  (:1664, :3466) are locale-key resolution, a different check; V-C32 respected), 1757 (`proptest`
  absent from every Cargo.toml in the workspace), 1759 (Finding 4), 1763 (machete/udeps have no
  step; clippy half wired).
- **Clean verdicts spot-checked (two required, four done)**:
  - *Attribution ledger*: every file-path row in data/ATTRIBUTION.md resolves on disk (scripted
    check: zero missing); zero Kenney rows; both `.csar` rows and all 10 `assets/src/*.jpg` rows
    present; `lint-attribution` exits OK (executed).
  - *Wasm flags*: ci.yml:47-48 is token-for-token PLAN:1749's excludes
    (`--workspace --exclude csp-server --exclude csp-bench --exclude xtask --target
    wasm32-unknown-unknown`). Exact match.
  - *lint-deps*: executed — `lint-deps: OK`; `check_membership` (lint_deps.rs:609-…) pins the
    authored lists to real members. **Correction to the report's preamble**: "ALLOW lists match the
    actual direct-dep sets … no drift in either direction" is wrong in the permissive direction —
    every determinism allowlist is a documented forward-looking superset (csp-sim allows blake3 +
    rand_chacha which its Cargo.toml does not declare; csp-core +3 unused; csp-data +3; csp-phys
    +5 including parry/rapier, per the comment at lint_deps.rs:80-81 "still being built out"). The
    enforced direction (actual ⊆ allow) holds, so the WIRED verdict for row 1751 stands.
  - *Bake idempotence*: structural support verified (fixed-order `build_all` vec, BTreeMap-only
    state — zero HashMap/Instant/SystemTime/rand hits in bake_art.rs and parts/mod.rs,
    pure-Rust miniz_oxide). Not executed (a rebake rewrites committed binaries; out of scope for a
    no-edit verification).
- **Adjacent rows**: V-DATA re-confirmed — docs/DATA-AND-SCRIPTING.md:351 itself documents
  gen_biome_maps `--check` as "not CI-gated (no workflow or test invokes it)". V-C29 (csp --smoke)
  respected.

## §8.2-delta verdict

**Confirmed: 8 §8.2 gate rows entirely absent, 4 partial.** The per-commit core (fmt, clippy
-D warnings, native tests + lavapipe render gate, wasm check, lint-deps/attribution/reach,
cargo-deny, full registered golden replays, per-commit perf job) is wired as the report says.
Findings 1 (HIGH) and 2 (HIGH) are the actionable core; Finding 5 is dismissed; the report's
preamble sentence on lint-deps drift is corrected above.
