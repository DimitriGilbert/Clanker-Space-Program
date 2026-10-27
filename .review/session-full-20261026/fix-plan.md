# Fix Plan — session-full-20261026 (Clanker Space Program)

## Overview

Executable fix plan built exclusively from CONFIRMED findings of the 35 verified review reports
(`verified-report-C01.md` … `verified-report-C35.md`) in this directory. Dismissed findings are
excluded; where a verifier downgraded or corrected a finding, the verifier's severity and nuance
are used (e.g. C14 chute CRITICAL→HIGH with viable-profile note; C15 `data_set_hash` MEDIUM→LOW;
C09 converter-mint latent-only; C12 slope-slide doc-fix only; C14 aero-rebuild-storm replaced by
the mass-blind-staleness finding). Every requirement cites its finding ID(s) `[Cxx-Fy]`.

Scope: ~224 confirmed findings deduplicated into 54 behavior workstreams across Phases 1–5,
9 refactor workstreams in Phase 6, and 7 separable [PRD-GAP] feature phases (Phase 7).
Ordering rule: crash/save-corruption/security → core playability → economy/content closure →
UI/UX remainder → PRD-coverage debt → refactors LAST (to avoid rebase churn).

Working tree: HEAD ≥ `7dd4b86`. Line numbers below are verified anchors from the reports;
treat them as ±5-line approximate and re-locate by symbol.

## Prerequisites

- Rust toolchain pinned by `rust-toolchain.toml` (1.97.1, clippy + rustfmt, wasm32 target).
- Node/npm for `apps/telemetry` (Phase 1.5, 4.11 only).
- Gatekeeping commands (from `docs/BUILD-AND-DEV.md`, all verified accurate post-repair):
  - Tests: `cargo test --workspace` (CI runs it with `CSP_RENDER_GATE_REQUIRED=1`)
  - Fmt/lint: `cargo fmt --all -- --check`; `cargo clippy --workspace --all-targets -- -D warnings`
  - xtask gates: `cargo run -p xtask -- lint-reach` / `lint-deps` / `lint-attribution`
  - Goldens: `cargo run -p csp-bench --bin csp-replay -- verify <dir>` for every dir in `csp-replay list`
  - Server e2e: `cargo test -p csp-server --test e2e`; eval tier: `cargo test -p csp-bench --test eval_suite`
  - SPA types: `npx tsc --noEmit` in `apps/telemetry`
  - Wasm: `cargo check --workspace --exclude csp-server --exclude csp-bench --exclude xtask --target wasm32-unknown-unknown`
- Determinism discipline: any change that alters serialized or hashed state may redden goldens.
  If `csp-replay verify` fails on a hash change: bump `HASH_SCHEMA_VERSION`, re-record via
  `cargo run -p xtask -- bless-goldens --reason "<finding id>"`, and commit the artifacts.
  Save-format changes must go through the §7.7 version chain (never mutate an old schema).
- NO-SLOP policy applies to every dispatch (per subagent-orchestration SKILL.md): no placeholder
  code, no TODO/FIXME, no unused imports/variables, no `unwrap`/`expect` outside sanctioned
  narrow allows (workspace clippy denies them), comments must state reasons.

## Cross-cutting conventions

- Phases are Sequential; sub-phases run in listed order (implementer → validator → fixer loop
  each). Sub-phases marked **(parallel-safe)** touch disjoint files and may be dispatched
  concurrently; a phase-wide validator still runs at phase end.
- Every sub-phase that adds behavior must add its regression test in the same sub-phase
  (AGENTS.md §8.7 "no code lands without its test"). Test-gap findings from C05 are folded into
  the matching fix workstreams rather than separate phases.
- Severity classes below use the VERIFIER-adjusted severity (C = critical, H = high,
  M = medium, L = low, N = nit).

---

## Phase 1: Crash, save-corruption, and security (P0)

**Type**: Sequential. 1.1, 1.4, 1.5 are parallel-safe; 1.2 → 1.3 → 1.6 share `crates/csp-sim/src/lib.rs` and must serialize.

### 1.1 Rhai `ctx.persist` shared-cell panic — one pasted line crashes the host [C26-F1] (C)

**Requirements**:
- Fix the root cause at `crates/csp-script/src/lib.rs:1183`–`:1209`: do not plant a shared
  `Dynamic` cell inside the `ctx` map that rhai 1.27's `calc_map_sizes` (`data_check.rs:86`)
  panics on under the engine's data-size limits (`lib.rs:670-676`). Preferred shape: pass the
  persist map by value snapshot per tick and write back explicitly, or register persist access
  through host functions only — any design where property assignment into `ctx`
  (`ctx.extra = 1;`, `ctx.persist = #{...};`) cannot reach a `Union::Shared` inside a map.
- Add a panic containment boundary around `engine.call_fn` (`lib.rs:1200-1202`) so any future
  rhai-internal panic degrades to a script strike (`Runtime` error) instead of process death
  (player path: AUTOMATION editor paste field `csp-ui/src/program.rs:1740` → `LoadScript`
  applied in-tick on the game thread; verified no `catch_unwind` exists anywhere in crates/).
- Correct the doubly-false doc at `lib.rs:1205-1209` ("reassignment is legal, read whatever it
  left"): reassignment panics today AND the write-back reads the original cell handle
  (`Dynamic::flatten_clone` reads through a shared cell), so document the actual contract.
- Regression tests covering all verified panic forms: `ctx.persist = #{n:1};`,
  `ctx.persist["n"]=1; ctx.persist = #{};`, `ctx.extra = 1;`, `let c = ctx; c.persist = #{n:1};`
  and the ok forms (`ctx.persist["n"] = 1;`, `.remove`, read-only) — assert no panic and sane
  strike behavior.

**Inputs**: Read `crates/csp-script/src/lib.rs` (RhaiHost, limits, run_tick), verified-report-C26.
**Outputs**: Modify `crates/csp-script/src/lib.rs`; add tests in the crate's test module.
**Validation Criteria**:
- `cargo test -p csp-script -p csp-ai` green including the new panic-form tests.
- `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings` clean.
- `cargo run -p xtask -- lint-reach` still OK (script surface unchanged).

**Dependencies**: None (first sub-phase).

### 1.2 Poison save — in-flight crew transfer + source-vessel loss is permanently unloadable [C04-F1, C05-F2] (H)

**Requirements**:
- Fix the restore gate at `crates/csp-sim/src/lib.rs:9952-9967` (`restore_transfer_state`):
  stop `Fatal`ing on "in-flight crew is not seated at the transfer's source". Align restore with
  the module's own documented contract (`lib.rs:9896-9898`: vessel ids may name removed vessels;
  the completion's fallback resolves it) and with fire-time behavior (`land_crew`,
  `transfers.rs:1162-1189`, already degrades to a documented no-op when the seat is gone).
  Either skip/refund per the fallback rule or refuse at `snapshot()` time — the chosen route
  must never produce a save that `restore` rejects.
- The legal producers to handle: `TerminateVessel`, crash/impact removal, breakup reseat,
  seat jettison (all verified to leave the transfer + `TransferComplete` event alive while the
  source seat record dies).
- Add regression tests reproducing the verified probe: two 2-seat crafts, `BoardCrew`,
  `TransferCrew{A→B}`, `TerminateVessel{A}`, then `snapshot()` → `restore()` must round-trip.
- Add the §7.5-shaped mid-window test [C05-F2]: crew transfer in flight, snapshot at mid-window
  (the harness shape already exists at lib.rs:20715 `save_load_mid_transfer` for resources —
  apply it to crew), including source-loss mid-window.

**Inputs**: Read `crates/csp-sim/src/lib.rs` (restore path), `crates/csp-sim/src/transfers.rs`.
**Outputs**: Modify `crates/csp-sim/src/lib.rs`, tests in the inline suite.
**Validation Criteria**:
- New tests pass under `cargo test -p csp-sim --lib`.
- Existing transfer suite green; goldens: `csp-replay` verify (state hash may change if the
  snapshot changes — follow the bless procedure if so).
- `cargo test --workspace` + clippy clean.

**Dependencies**: Phase 1.1 complete (parallel-safe set) — must precede 1.3 (same file).

### 1.3 Poison save — disabled script strands its pending `ScriptWake` [C11-F1, C05-F3] (H)

**Requirements**:
- `apply_set_script_enabled` (`csp-sim/src/lib.rs:5429-5449`) and the strike-disable path
  (`lib.rs:7878-7891`): on disable, scrub the script's pending `ScriptWake` events from the
  heap and clear `wake_tick`, matching the in-repo precedents `apply_unload_script`
  (`lib.rs:8089-8096`) and `apply_set_life_support(false)` (`life_support.rs:225-255`).
- `fire_script_wake` (`lib.rs:8120-8147`): when `!live`, clear `wake_tick` before returning so
  the `Some(t)`/no-event pairing cannot persist (this also stops the stale Clamping event
  demoting warp for a dead script).
- Regression test mirroring the verified probe: arm `request_wake(AtTick(200))` → disable →
  step past 200 → assert no Clamping `ScriptWake` in `fired_events`, `wake_tick == None`,
  snapshot → restore round-trips.

**Inputs**: Read `csp-sim/src/lib.rs` (script lifecycle), `crates/csp-sim/src/scripts.rs`.
**Outputs**: Modify `crates/csp-sim/src/lib.rs`; tests.
**Validation Criteria**:
- `cargo test -p csp-sim --lib` green; workspace tests + clippy clean; goldens verified
  (bless procedure if the hash walk changes).

**Dependencies**: Phase 1.2 complete (same file, ordered).

### 1.4 `session.save` writes non-atomically in csp-server [C27-F2] (M)

**Requirements**:
- `crates/csp-server/src/backend.rs:607` (`store_save`): replace bare `std::fs::write` with the
  temp+rename discipline used by `csp-app/src/lib.rs:1133-1138` and
  `csp-server/src/dashboard.rs:238-255` (tmp sibling, rename, cleanup on rename failure).
  Overwrite-at-cap is intended (`store_save_is_capped_but_overwrites` test) — the fix must
  preserve overwrite semantics while never leaving a torn target.
- Extend the existing backend test to assert no `.*.tmp` remains after a successful overwrite
  and that a mid-write failure path cannot truncate the previous good save.

**Inputs**: Read `csp-server/src/backend.rs`, `csp-server/src/dashboard.rs`.
**Outputs**: Modify `crates/csp-server/src/backend.rs`.
**Validation Criteria**: `cargo test -p csp-server` (lib + `--test e2e`) green; clippy clean.

**Dependencies**: None (parallel-safe with 1.1/1.2).

### 1.5 SPA build can bake the bearer token into the served shell [C28-F3] (M, security)

**Requirements**:
- `apps/telemetry/src/lib/ws.ts:111`: read `import.meta.env.VITE_CSP_TOKEN` only in dev mode
  (gate on `import.meta.env.DEV`); production builds must resolve to `''` unconditionally.
- `scripts/finalize.mjs`: after promoting `_shell.html` → `index.html`, assert no
  `VITE_CSP_TOKEN` value and no token-shaped literal leaked into `dist/client/assets/*.js`;
  fail the build on match (the server serves `/` and `/assets/*` token-free, per
  `csp-server/src/http.rs:346-349` and PLAN §2.12.3).

**Inputs**: Read `apps/telemetry/src/lib/ws.ts`, `scripts/finalize.mjs`, README dev-workflow note.
**Outputs**: Modify `apps/telemetry/src/lib/ws.ts`, `scripts/finalize.mjs`.
**Validation Criteria**:
- `npx tsc --noEmit` clean; `npm run build` succeeds with and without `VITE_CSP_TOKEN` set,
  and the finalize assertion rejects a deliberately-baked token in a scratch check.

**Dependencies**: None (parallel-safe).

### 1.6 Restore-time hardening — notification cap + PartFailure residence coherence [C04-F3, C10-F6] (L/M)

**Requirements**:
- Enforce `notification_cap` at restore (`csp-sim/src/lib.rs:3480` assigns the deque verbatim):
  trim to `reliability_cfg.notification_cap` on restore so a crafted oversized log cannot ride
  until the next push (verified: writer-side trim exists at `reliability.rs:760-762`; impact
  window is "until the next reliability notification fires" — still worth closing).
- PartFailure restore coherence (`lib.rs:3170-3204` residence match + `lib.rs:10636-10642`
  pairing gate): add `PartFailure` to the rails-only residence group, and make
  `fire_part_failure`'s InBubble no-op arm (`reliability.rs:996-998`) clear the pending flag
  (mirroring the fatal-mode branch's own comment about wedging the next restore), so a crafted
  InBubble+pending snapshot cannot brick the follow-up save ("pending failure tick / event
  mismatch").
- Tests: crafted snapshot with oversized log restores trimmed; crafted InBubble+pending+event
  snapshot either fails restore with a clear message or restores coherently (no follow-up-save
  brick).

**Inputs**: Read `csp-sim/src/lib.rs` (restore), `crates/csp-sim/src/reliability.rs`.
**Outputs**: Modify both files; tests.
**Validation Criteria**: `cargo test -p csp-sim --lib` green; goldens verified; workspace tests.

**Dependencies**: Phase 1.3 complete (same file region).

**Phase-level Validation**: All sub-phases individually validated; phase-wide validator reads
all Phase-1 diffs together (no contract drift between save-integrity fixes), then runs the full
gate set: `cargo test --workspace`, fmt, clippy, xtask lints, `csp-replay` verify loop, SPA
tsc. **Dependencies**: None (first phase).

---

## Phase 2: Core playability (P0/P1)

**Type**: Sequential. 2.1→2.6 share `csp-view/src/lib.rs`; 2.2→2.4 share `csp-app/src/lib.rs`;
2.3, 2.5, 2.7, 2.8 are parallel-safe groups by crate but 2.3/2.8 both touch `csp-sim/src/lib.rs`
(order 2.3 before 2.8).

### 2.1 Revert-point integrity — capture-after-enqueue kills revert; stale revert across load [C21-F1 (H), C21-F5 + C22-F1 (L), C21-F3 (M)]

**Requirements**:
- `launch_craft` (`csp-view/src/lib.rs:2950-2995`): capture `pre_launch` BEFORE enqueueing the
  parked-build `RecoverVessel` (`:2969-2978`), or drain/handle the pending queue so
  `World::snapshot` cannot refuse at `:2995` with "commands pending at serialization boundary".
  Verified: the parked-build launch leaves `revert_available() == false` forever and
  `revert_to_launch` errs ("revert point could not be captured at launch").
- Move the revert point to the delivery boundary per the sim's own contract
  (`manufacture.rs:361-365` expects a post-delivery snapshot; PLAN:1177-1180 "retry of the
  flying", "revert is not a reroll") so a revert does not re-pay the build wait
  [C21-F3: verified empty-pad revert restores pre-order funds and zero builds].
- `load_game` (`csp-view/src/lib.rs:3077-3079` → `rebuild_from_snapshot` `:1574-1602`): clear
  `pre_launch = None` on load so F10/Game-menu revert can never replace the loaded world with
  the previous session's snapshot (both C21-F5 and C22-F1 verified the missing reset;
  `new_game` is safe — whole View replaced).
- Regression tests: parked-build launch → `revert_available() == true`; launch A → load B →
  revert refuses or reverts B's own point (place next to `revert_to_launch_restores_pre_launch_snapshot`,
  lib.rs:5995); revert-after-delivery does not re-debit funds.

**Inputs**: Read `csp-view/src/lib.rs` (launch/load/revert), `csp-sim/src/manufacture.rs`.
**Outputs**: Modify `crates/csp-view/src/lib.rs`; tests (csp-view).
**Validation Criteria**: `cargo test -p csp-view` green (incl. `pad_spawn`, `launch_deliver_throttle_flies`);
workspace tests + clippy.

**Dependencies**: Phase 1 complete.

### 2.2 Warp ×2 unreachable + wrong-rung partial demotion + notice flood [C24-F2 (H), C25-F2 (M), C24-F4 (M)]

**Requirements**:
- Press gate (`csp-app/src/lib.rs:1425-1434`): stop applying the rails-warp refusal predicate to
  rung 1. Per PLAN §2.6.2, ×2's allowed-when is "bubble occupied"; refuse only steps above ×2
  while `rails_warp_refusal()` holds (cap at rung 1), so atmospheric ascent can use physics warp.
- Heartbeat (`csp-app/lib.rs:1307-1316`): keep `demote(0)` for fired `Clamping` events, but do
  not pin ×2 to rung 0 merely because the rails predicate holds (an engaged ×2 in vacuum coast
  must survive a transient atmospheric dip per the table's "demoted by the controller" derate
  rule, which is §2.6.3's budget mechanism, already independent).
- Partial-backlog demotion (`csp-app/lib.rs:1323-1333`): `demote(5)` not `demote(4)` — the
  largest rung one call can reach under `MAX_TICKS_PER_FRAME = 69120` is rung 5 (×10⁴;
  64×10⁴/60 = 10 666.7 ≤ 69 120). Fix the comment (currently mislabels rung 4 as ×10⁴).
- Notice flood: latch the "WARP LIMITED — EVENT/TICK BACKLOG" notice behind the demote edge
  (same pattern as `last_refusal` at lib.rs:1300-1306) so a sustained `report.partial` does not
  push ~90 identical chips.
- Add the missing test: ×1→×2 press under a held rails refusal must succeed; ×10⁵ under
  sustained partial must land at ×10⁴ not ×10³.

**Inputs**: Read `csp-app/src/lib.rs`, `crates/csp-app/src/warp.rs`, PLAN §2.6.2/§2.6.3.
**Outputs**: Modify `crates/csp-app/src/lib.rs`, `crates/csp-app/src/warp.rs`; tests.
**Validation Criteria**: `cargo test -p csp-app --lib` green (51 baseline + new); workspace tests + clippy.

**Dependencies**: Phase 2.1 complete (phase ordering); shares csp-app lib.rs with 2.4 — run 2.2 first.

### 2.3 Ascent autopilot termination + auto-rails suppression [C03-F2 + C19-F2 + C26-F2 (H), C26-F6 (M)]

**Requirements**:
- `crates/csp-ai/src/lib.rs:162-165` (phase 4): call `api.disengage_autopilot()` when the
  ascent completes (the verb exists on the exact `IoControlApi` the driver holds —
  `csp-script/src/lib.rs:131-132`, impl `:295-301`), so the record does not sit `enabled`
  forever. Update the phase-4 doc (`:24-25`) and the bench assertion
  (`csp-bench/src/scenarios.rs:986`) to the new terminal state (record removed → vessel
  auto-rails next tick).
- Remove/justify the dead phase-2 safety guard (`csp-ai/lib.rs:141-146`, requires
  apo-Some + t_apo-None which never co-occur per `osculating`) and handle the real open-conic
  strand: engaging on an already-escaping vessel (apo None from tick one) must not burn at
  throttle 1 forever — strike/refuse engagement on open conics or add an abort condition
  [C26-F6, verifier-corrected manifestation: phase 1, not phase 2].
- Add the first csp-ai unit tests (crate currently has zero): completion disengages; open-conic
  engagement aborts; auto-rails then rails a coasting clearing vessel
  (auto-rails exclusion at `csp-sim/src/lib.rs:4802-4804` becomes vacuous once the record is
  gone — verified recovery path).
- Orbiting craft consequences (all verified): map draws no orbit line (`map.rs:2360` gates on
  `v.orbit()`), maneuver planning refused (`lib.rs:6649-6650`), no manual `ToRails` issuer —
  the fix removes the whole class.

**Inputs**: Read `csp-ai/src/lib.rs`, `csp-sim/src/lib.rs:4802-4804`, `csp-script/src/lib.rs`.
**Outputs**: Modify `crates/csp-ai/src/lib.rs`, `crates/csp-bench/src/scenarios.rs`; add csp-ai tests.
**Validation Criteria**: `cargo test -p csp-ai` (new suite) + `cargo test -p csp-bench` green;
workspace tests + clippy.

**Dependencies**: Phase 1 complete; before 2.8 (shares csp-sim lib.rs).

### 2.4 Stale wall-side throttle axis survives Load/NewGame [C24-F3] (H)

**Requirements**:
- Reset the throttle-axis state machine (`throttle_axis`, `throttle_axis_vessel`,
  `throttle_hold_vessel` — `csp-app/src/lib.rs:246-280`) on world-replacing session ops
  (`run_session_op` NewGame/Revert, `do_load`), the same way `focus_lost` resets it. Verified
  impact: deterministic VesselId collision across saves defeats the rebase in
  `throttle_press` (`lib.rs:2238-2243`) and the first Shift tap unlatches at the stale
  full-throttle value via `throttle_axis_step` (`lib.rs:2358-2371`) — full-throttle pad
  ignition from a 1% tap after Z → latch → F5 → F9 → Shift. Same bug class as dd3612a/b698eb2.
- Regression test: set axis to 1.0 on vessel 0, load a save whose vessel 0 is latched, press
  Shift with a small axis delta → assert no `SetThrottle` above the delta-scaled value
  (the shipped same-world behavior `shift_press_unlatches_the_relanded_craft` at lib.rs:4571
  must keep passing).

**Inputs**: Read `csp-app/src/lib.rs` throttle methods, `crates/csp-view/src/lib.rs` (rebuild path).
**Outputs**: Modify `crates/csp-app/src/lib.rs`; tests.
**Validation Criteria**: `cargo test -p csp-app --lib` green incl. both Shift tests; workspace tests + clippy.

**Dependencies**: Phase 2.2 complete (same file).

### 2.5 New Game boots an empty world + drops the warp cap [C24-F1 (H), C25-F1 (M)]

**Requirements**:
- `SessionOp::NewGame` (`csp-app/src/lib.rs:1084-1094`): seed the starter craft on the fresh
  native world (call the same `seed_starter_craft` path `App::new_inner` uses at
  `lib.rs:406-414`, guarded by `!view.is_demo()`), so New Game does not land in the verified
  "purple void" (no flight focus, blank VAB session — `seed_pad_craft` also builds the VAB
  editor craft). Add a session-op-level test (the existing `new_game_boots_with_a_pad_vessel`
  drives only the constructor path).
- Re-apply the settings warp cap after the controller swap: call `apply_live_settings()`
  (or minimally `set_cap_multiplier`) after `self.warp = WarpController::new()` at
  `lib.rs:1089` (verified: `WarpController::new` hard-codes `cap_idx` at ladder top; only
  `apply_live_settings` restores the M10 "max time warp" setting; the fresh-controller route
  through `demote(cap_idx)` is provably side-effect free per C25 verification).
- Test: set max warp to ×100 in settings → NewGame → press WarpUp beyond ×100 → refusal
  notices "SETTINGS LIMIT", not "LADDER TOP".

**Inputs**: Read `csp-app/src/lib.rs`, `crates/csp-view/src/lib.rs:3132-3146`, `crates/csp-view/src/demo.rs`.
**Outputs**: Modify `crates/csp-app/src/lib.rs`; tests.
**Validation Criteria**: `cargo test -p csp-app --lib` green; workspace tests + clippy.

**Dependencies**: Phase 2.4 complete (same file).

### 2.6 Focus handoff on auto-rails + focused-vessel SOI clamping + `active_vessel` [C19-F2 (H), C02-F1 (H), C02-F2 (H), C19-F4 (M)]

**Requirements**:
- Auto-rails focus handoff (`csp-view/src/flight.rs:1059` stages nothing for OnRails): when the
  focused vessel converts to rails, keep camera/HUD tracking it (stage rails vessels for focus
  resolution or hand off to Map with a notice). Verified end states today: camera pops to the
  parked pad craft showing its telemetry, or a near-black dead view with no hint; plus the
  mislabeled "VESSEL REMOVED" cue (`flight.rs:1938-1940` uses total vessel count instead of
  `live_vessels`). Fix the cue to use `live_vessels`.
- Focused-vessel SOI clamping [C02-F1]: issue `SetVesselTracked` (or reclass at schedule time)
  for the view-focused vessel so PLAN §2.6.5's "focused/tracked vessel `SoiTransition`"
  Clamping member is player-reachable — verified today NOTHING production issues the command,
  so a ×10⁵ warp through a moon encounter cannot be clamped (back-pressure: ~18 sim-min/frame).
  Wire it at the view's focus boundary (focus set/cleared → track/untrack command), reusing the
  existing apply + queued-event restamp (`csp-sim/src/lib.rs:5295-5316`).
- `active_vessel` as sim state [C02-F2]: add `active_vessel: Option<VesselId>` to `World` +
  `WorldSnapshot` per PLAN §2.2/§7.2 (serialized + hashed), set by command from the view's
  focus, restored on load — fixing the verified "camera/HUD jump to the newest vessel on every
  load" (`rebuild_from_snapshot` falls to `vessels.last()`). Keep per-vessel `Lease` gating as
  the control-authority mechanism (verified sound); document the relationship at the field.
- Chase-camera reframe [C19-F4]: on focus-id change, reset `FlightInput::distance_m`
  (`flight.rs:85`, `:1492` are the only writers — verified nothing resets it) or fix the
  false doc at `flight.rs:40-41` ("reframed on focus changes"); prefer the behavior fix.

**Inputs**: Read `csp-view/src/flight.rs`, `csp-view/src/lib.rs`, `csp-sim/src/lib.rs`
(World/Snapshot/command), `csp-sim/src/events.rs`.
**Outputs**: Modify `crates/csp-view/src/{lib,flight,map}.rs`, `crates/csp-sim/src/lib.rs`; tests.
**Validation Criteria**:
- `cargo test -p csp-sim --lib` + `cargo test -p csp-view` green; new tests: focused vessel's
  SOI transition demotes warp; snapshot/restore preserves focus; camera reframes on focus change.
- Goldens: hash walk changes → follow the bless procedure (HASH_SCHEMA_VERSION bump + reason).
- Workspace tests + clippy + `lint-reach` (if a new issuer appears for SetVesselTracked, the
  ui-unreachable annotation must be updated accordingly).

**Dependencies**: Phase 2.1 and 2.3 complete (shares csp-view lib.rs and csp-sim lib.rs).

### 2.7 VAB pick pass draws pick-0 decoration over click targets; armed-part drag swallowed [C17-F1 (H), C17-F2 (H), C17-F7 (L)]

**Requirements**:
- `VabRenderer::encode_pick` (`csp-render/src/vab.rs:404-455`): stop uploading/drawing the
  pick-0 decoration stream (ghosts, selection shells, markers — tagged 0 by construction at
  `csp-view/src/vab.rs:793-823`, `:1128-1158`) into the `R32Uint` pick target. Verified: the
  inflated shell strictly encloses the part and beats it at every pixel under `Greater`-wins
  reverse-Z; root-selected crafts decode 0 at 1440/1440 sampled pixels; carrying ghosts shadow
  883–1440/1440. Both advertised gestures ("click a craft part to place it", "click/drag
  selected: move") are dead in the common case.
- Gesture machine (`csp-ui/src/vab.rs:2304-2465`): with `pending_part` armed, a viewport LMB
  drag currently produces no orbit, no drop, no pick, no cancel (orbit_block drops every delta;
  drag-lift requires `carried().is_none()`; both release branches match `None`; app side
  requests no pick past `CLICK_MAX_DRAG_PX`). Make the drag do something honest: lift the part
  into carry on drag-start past threshold (or cancel placement + let the camera orbit) — pick
  one, implement it, test it.
- Add `notice_overlay`'s rect to the pointer masks in VAB mode (`csp-ui/src/lib.rs:702-729`
  returns `()`; drawn at `csp-view/src/lib.rs:810` after mask collection) so right-click on a
  cue chip cannot delete the part behind it [C17-F7].
- Tests: a software-rasterization probe (the C17 verifier's method — real packet through the
  pick pipeline math) or a render-gate-style assertion that the pick pass emits no 0-tagged
  geometry; UI gesture tests for armed-drag.

**Inputs**: Read `csp-render/src/vab.rs`, `csp-view/src/vab.rs`, `csp-ui/src/vab.rs`, `csp-app/src/lib.rs:2456-2501`.
**Outputs**: Modify `crates/csp-render/src/vab.rs`, `crates/csp-ui/src/vab.rs`,
`crates/csp-view/src/lib.rs`, `crates/csp-view/src/vab.rs`; tests.
**Validation Criteria**: `cargo test -p csp-ui -p csp-view -p csp-render` green; pick-related
in-tree tests (the injected-`VabEvent::Pick` tests at `csp-view/src/vab.rs:1401`) still pass;
workspace tests + clippy.

**Dependencies**: Phase 1 complete (parallel-safe with 2.2–2.5 by crate, but ordered here to
keep the phase sequential).

### 2.8 Parachute survivability — designed profile destroys the chute [C14-F1 (H, downgraded from CRITICAL), C13-F4 (same mechanism)]

**Requirements**:
- Ramp from gate-crossing, not the deploy command stamp: `decel_area_m2`
  (`crates/csp-phys/src/simple.rs:2651-2664`) must anchor the 1.5 s inflation ramp at the first
  tick BOTH gates pass (alt ≤ 6 000 m && speed ≤ 260 m/s), not at `deployed_at_s` from the
  command (`csp-sim/src/lib.rs:8941` stamps; verified full 120 m² on the first gate-passing
  tick for deploy-early commands, including staged auto-deploys at `csp-view/src/lib.rs:3283`).
- Bound the canopy load to the mount: effective area reefed by
  `area_eff = min(A, F_max / (0.5·ρ·v²))` (or author the mount for canopy hard-point loads) so
  the verified ~1.9–2.8 MN catch load (13–34× the 150 kN axial / 60 kN shear of
  `data/parts/csp.chute.s125.ron:42-56`) cannot tear the chute at gate-pass. Preserve
  deterministic arithmetic (libm-only, no new float ops outside the ban list).
- Update the part doc/header to match the real survival envelope (verifier: viable profiles
  exist — deploy < ~2.5 km falling packed or already-slow; knife-edge at ~1.2 km; the current
  doc instructs deploy-and-forget which reliably destroys the chute). Keep `max_axial_n` semantics.
- Re-run the e2e with finite limits: `chute_craft()` fixture (`csp-sim/src/lib.rs:14694`) sets
  `StructuralDef::unlimited()` — replace with the authored limits and assert
  `deployed_chute_decelerates_and_lands` still lands intact on the documented profile, plus a
  tear-on-hot-entry test on the abuse profile.
- Optional but recommended (same file, verified by C14 probe): the default top-mounted
  science.canister + battery strip at 15–25 km on orbital-speed entries regardless of the
  chute — either accept + document, or extend the doc. Do not change structural constants here.

**Inputs**: Read `csp-phys/src/simple.rs`, `csp-phys/src/structural.rs`, `data/parts/csp.chute.s125.ron`,
verified-report-C14 (numbers), `csp-sim/src/lib.rs` (deploy stamp + tests).
**Outputs**: Modify `crates/csp-phys/src/simple.rs`, `data/parts/csp.chute.s125.ron` (doc/limits),
`crates/csp-sim/src/lib.rs` (fixtures/tests).
**Validation Criteria**:
- New tests pass: gate-anchored ramp (command-to-catch delay no longer pre-completes the ramp);
  reefed load < mount limits on the designed profile; tear still occurs on the abuse profile.
- `cargo test -p csp-phys` (157 baseline) + `cargo test -p csp-sim --lib` green; workspace
  tests + clippy; goldens verified (aero/physics constants changed → bless if hashes move).

**Dependencies**: Phase 2.3 complete (same crate file csp-sim lib.rs ordering).

### 2.9 Undock is a functional no-op on the live path [C10-F1] (H)

**Requirements**:
- `apply_undock` (`csp-sim/src/lib.rs:7273-7524`): insert the canonical pair into
  `joint_refused` before returning, so the next tick's `joints::lifecycle` detection
  (`joints.rs:526-565`, skips pairs in `refused` at `:532-533`) cannot re-capture the
  still-mated zero-rel-speed pair and the following `latch_pass` re-merge it. Verified live:
  undock → re-captured same tick → re-merged next tick; command reaches MCP agents today via
  the raw `sim.command` passthrough. The refusal self-prunes once the pair leaves capture
  geometry (`joints.rs:518-520`) — reuse that lifecycle.
- Live-loop regression test through the real command path (spawn → capture → latch-merge →
  `SimCommand::Undock` → step ≥ 3 ticks → assert two vessels remain and no joint re-forms);
  fix the masking tests (`undock_splits_at_seam...` lib.rs:18904 steps exactly one tick;
  `merge_and_split_conserve_momentum` calls the apply directly) to also assert the live path.

**Inputs**: Read `csp-sim/src/lib.rs:7273-7524`, `crates/csp-sim/src/joints.rs`.
**Outputs**: Modify `crates/csp-sim/src/lib.rs`; tests.
**Validation Criteria**: `cargo test -p csp-sim --lib` green (363 baseline + new); goldens
verified (`joint_refused` is serialized/hashed — bless procedure if the hash moves);
workspace tests + clippy.

**Dependencies**: Phase 2.8 complete (same file).

**Phase-level Validation**: Full gate set; phase-wide validator checks that no Phase-2 sub-phase
re-introduced a session-state leak across load/revert (grep the world-replacing verbs for every
`View`-resident session field touched), and that csp-sim hash-affecting changes went through
the golden bless procedure with reasons. **Dependencies**: Phase 1 complete.

---

## Phase 3: Economy and content loop closure

**Type**: Sequential. 3.1 → 3.2 → 3.3 (data+sim economy chain); 3.4, 3.5, 3.6, 3.7, 3.8 follow.

### 3.1 Resource loop closure — ore carrier, composite recipe, site-stock refill [C33-F1 (H), C34-F2 (H), C33-F6/C34-F2 comment, C31-F5 + C33-F4 (M)]

**Requirements**:
- Author a cargo part that can hold ore (new `data/parts/csp.tank.ore*.ron` or extend an
  existing tank's resources — verified: the only 4 Tank modules hold LF/ox/EC/O₂/food; nothing
  can hold ore/metal/composite, so no designed path can move ISRU output or complete an ore
  resupply). Follow the existing tank RON shape and the attribution/loc conventions (all
  non-authored assets need `data/ATTRIBUTION.md` rows; loc keys must resolve).
- Author a composite-producing recipe (`data/recipes/` — verified: zero producers of
  `composite`; the pad starts 2 000 metal / 400 composite and a starter craft costs
  163 metal / 16 composite → verified 12–13-launch fuse with no designed recovery). Keep the
  recipe graph acyclic and net-negative (see 3.2's validator).
- Close the §3.5 "resources ◄── colonies/ISRU" edge so Rask metal/composite can fund builds:
  either (a) recovery credits embodied `build_resources` to the site at the recovery
  efficiency (fixing the false `data/systems/home/sites.ron:44-45` "recovery at the pad
  refills it" comment by making it true), or (b) a colony→site (or colony→vessel with cargo
  tanks from this sub-phase) transfer end — pick the smaller surface that satisfies
  PLAN §3.5's diagram (funds └─► facilities; resources ◄── colonies/ISRU) and record the
  decision in the data file headers.
- Fix or remove the false comments: `sites.ron:44-45` (recovery refills) and the stale part
  headers [C33-F5: battery "0.625 m node class" vs 1.25 nodes; probe "0.625-class" vs 1.25].
- Set `max_tracked_vessels: 512` in `data/balance.ron` (PLAN §3.5 authors 512; ships
  u64::MAX = 18446744073709551615 today; sim default is also u64::MAX — align both
  `DEFAULT_MAX_VESSELS` and the data, or author a deliberate recorded deviation per §6.5).
  Also fix the wrong "PLAN §2.9" references at `balance.ron:13` / `balance.rs:74` (it is §3.5).
- Tests: a data-level test asserting the ore tank exists and validates; a sim test moving ore
  vessel→colony; the fuse arithmetic documented in a test (starter craft BOM) so future part
  edits re-price it.

**Inputs**: Read `data/parts/*.ron` (tank shapes), `data/recipes/*.ron`, `data/systems/home/sites.ron`,
`data/balance.ron`, `crates/csp-sim/src/economy.rs` (`recovery_resources` :534-547),
`crates/csp-sim/src/transfers.rs` (TransferEnd), PLAN §3.5.
**Outputs**: Create `data/parts/` ore tank (+ `data/loc/en.ron` keys, ATTRIBUTION if assets),
create composite recipe, modify `data/systems/home/sites.ron`, `data/balance.ron`,
`crates/csp-data/src/balance.rs` (comment/constant), possibly `crates/csp-sim/src/economy.rs`;
tests in csp-data and csp-sim.
**Validation Criteria**:
- `cargo test -p csp-data` (121 baseline) + `cargo test -p csp-sim --lib` green;
  `cargo run -p xtask -- lint-attribution` OK; goldens verified (data changes flip
  `data_set_hash`, NOT golden hashes — fixtures are frozen; only sim-code changes can move
  goldens); workspace tests + clippy.

**Dependencies**: Phase 2 complete.

### 3.2 Colony resupply completable + contract-board decline/expiry [C34-F1 (C), C09-F5, C08-F5 (M), C08-F4 (M), C32-F1 (H), C32-F6 (L)]

**Requirements**:
- Make `contract.colony_resupply` completable: with 3.1's ore carrier, rask×ore becomes
  deliverable; fix the remaining dead draws — drop `duva` from the bodies list (no sites, no
  colonies possible: `apply_found_colony` rejects unknown sites, `SimCommand` has no
  site-creation verb), and either drop terrin×{ore,food,oxygen}/rask×{food,oxygen} or make
  them completable (starter colony has no Store modules → capacity 0 → "insufficient colony
  store capacity"). Target: every authored draw is completable in a shipped game.
- End the free-advance faucet: give `deadline_days` a value OR add a decline/cancel verb
  (verified: `SimCommand`'s contract surface is `AcceptContract` only; duds permanently burn
  1 of 32 active slots at 10k advance each; at 32 duds the whole §3.6 system bricks).
  Add `DeclineContract` (declines an offer, freeing the board slot; `publish_contracts`
  refills) and/or an abandon path for active contracts; update `lint-reach` annotations.
- Offer-board health [C08-F5]: body draws must be capability-gated at generation (rescue draws
  rask for mid-game players who cannot fly it; satellite band math leaves ~41% of draws
  completable by a parked orbit) — minimally: filter resupply bodies by colony/site
  existence (the `draw_params` arm at `contracts.rs:377-391` draws blindly) and re-roll
  unsatisfiable draws instead of jamming slots.
- Survey expiry [C08-F4]: `apply_stop_research` and `fire_research_exhausted` must
  `contract_notify` (all nine existing call sites enumerated in C08; these two are missing),
  so an already-satisfied survey completes instead of sitting active until
  `fire_contract_deadline` charges the penalty unconditionally (add the objective-met check
  before penalty in `fire_contract_deadline`, `contracts.rs:1169-1211`).
- Validator gates [C32-F1, C32-F6]: thread `&SiteFile` into `validate_contracts`
  (`csp-data/src/validate.rs:4127-4133`; `validate_dataset` holds `d.sites` but does not pass
  it) and require ≥1 site per candidate body for ColonyResupply; validate
  `ContractsCompleted { kind }` against authored kinds (mirror of the existing
  UnknownContractType check at `:4237-4246`).
- Fix the stale accept-path test comment [C08-F10] (`contracts.rs:1839-1842` inverts the
  offer-removal order).
- Tests: decline frees a slot and pays nothing; completed-objective survey is not penalized;
  validator rejects duva-resupply and unauthored-kind prereqs; a full resupply lifecycle
  (accept → load ore → deliver → complete) using 3.1's part.

**Inputs**: Read `data/contracts/contract.colony_resupply.ron`, `crates/csp-sim/src/contracts.rs`,
`crates/csp-data/src/validate.rs`, `crates/csp-sim/src/research.rs`.
**Outputs**: Modify `data/contracts/*.ron`, `crates/csp-sim/src/{lib,contracts,research}.rs`,
`crates/csp-data/src/validate.rs`; tests.
**Validation Criteria**: `cargo test -p csp-sim --lib -p csp-data` green; `lint-reach` OK
(new command needs an issuer or annotation — wire the UI button in 3.7/4.8 or annotate
temporarily and wire in the same phase); goldens (bless if hash moves); workspace tests + clippy.

**Dependencies**: Phase 3.1 complete (ore carrier is the enabler).

### 3.3 Converter conservation + recipe-graph validation [C09-F1 (H, latent), C32-F2 (H)]

**Requirements**:
- Sim side (`crates/csp-sim/src/colonies.rs:775-803` `grounded_modules`, `:825-953`
  `allocation_pass`): reject or pin to zero the fractions of any SCC with positive aggregate
  net rate and no Drill/Solar/Generator member — the grounding doc claims to kill exactly
  this class, and the verifier reproduced 10 → 130 units in 600 s from a stock-anchored
  net-positive cycle through the real `FoundColony` path. Latent-only today (the three
  authored recipes are acyclic) but 3.1 adds new recipes — this must land with them.
- Data side (`csp-data/src/validate.rs:2178-2242` `validate_recipes`): add a load-time
  acyclicity + net-yield check (PLAN §8.2 lists "cycles" as a validator duty; only tree/body
  cycles exist today), so a future cyclic recipe pair cannot ship green.
- Tests: the verifier's probe recipes (`a→b 0.5`, `b→a 1.2`) — validator rejects; sim pins
  the cycle at zero net; existing `self_feeding_converter_pins_at_zero` and
  `grounded_cycle_still_runs` (generator-anchored, sanctioned) keep passing.

**Inputs**: Read `crates/csp-sim/src/colonies.rs`, `crates/csp-data/src/validate.rs`, `data/recipes/`.
**Outputs**: Modify both files; tests.
**Validation Criteria**: `cargo test -p csp-sim --lib colonies:: -p csp-data` green; workspace
tests + clippy; goldens verified.

**Dependencies**: Phase 3.2 complete (same file set, ordered).

### 3.4 Recovery must not destroy running lab work + flagship chain test [C08-F1 (H), C05-F1 (H), C05-F4 (M)]

**Requirements**:
- `apply_recover_vessel` (`crates/csp-sim/src/economy.rs:880-1010`) currently banks only pooled
  `v.science.data` then `remove_vessel` → `research::drop_vessel_projects`
  (`research.rs:565-597`, "work-in-progress is lost, no accrual"). Fix: finalize live projects
  on the vessel at recovery (`finalize_project` banks accrual AND returns the unconverted
  remainder to the pool — the intended contrast already exists in `apply_stop_research`), or
  refuse recovery while a project holds escrow (matching the existing `RecoverData` gate) with
  a clear notification. Prefer finalize (the UI copy at `program.rs:2131-2137` and `:590-591`
  frames recovery as banking). Note the pad-clear auto-enqueue
  (`csp-view/src/lib.rs:2969-2978`) can trigger this without a player press — the fix covers both.
- Add the missing flagship-chain coverage [C05-F1]: a single test driving
  launch → orbit → run experiment → science in space → recover, asserting recovered science is
  banked (today `RecoverVessel` appears in exactly 2 lib.rs tests, neither with science
  aboard; use the `auto_rails_converts_a_coasting_orbiter` spawn trick at lib.rs:20046+ as the
  verified anchor). [C05-F4] also run `RecoverVessel` against a vessel with a live
  `StartResearch` project and assert project state explicitly.

**Inputs**: Read `crates/csp-sim/src/economy.rs`, `crates/csp-sim/src/research.rs`, C05 anchors.
**Outputs**: Modify `crates/csp-sim/src/economy.rs` (or research.rs); tests in csp-sim.
**Validation Criteria**: `cargo test -p csp-sim --lib` green; goldens verified; workspace + clippy.

**Dependencies**: Phase 3.3 complete.

### 3.5 Salary band + insolvency consequences [C08-F2 (H), C08-F3 (H)]

**Requirements**:
- Move the salary rate into `data/balance.ron` (PLAN §8.3: "ALL tuning lives here"; verified:
  the shipped default is the view-only hardcode `HIRE_SALARY_PER_S = 0.02` at
  `csp-view/src/lib.rs:1294` = ₣51 840/crew/sim-month = 5.18% of starting funds for ONE crew,
  above PLAN:1196-1200's 2–5% band for a fully-staffed base; 20 crew ≈ 104%/month). Author a
  default inside the band and thread it through the view's hire path; keep
  `apply_hire_crew`'s parameterized shape.
- Make the §8.3 harness gate the SHIPPED rate: `csp-bench/src/economy.rs` currently solves
  the salary from the target drain fraction (`economy.rs:89-97`) so it cannot catch an
  out-of-band default — add an assertion that the shipped default itself lands in 2–5% for
  the staffed base (or drives it from balance.ron).
- Implement insolvency consequences (PLAN:1201-1204 promises them; verified zero code):
  `StaffAttrition` and `FacilityDormant` events on unpaid salaries (attrition: crew go
  `Retired`/leave after sustained insolvency — `CrewStatus::Retired` currently has no setter;
  dormant: facilities suspend), and a facility-upkeep drain (grep-verified absent; the bench
  comment deferring to "M8's colony/economy block" is stale — M8 shipped without it). Keep the
  `Insolvent` Clamping auto-demote as is. Size minimally: one upkeep drain keyed off existing
  facility surfaces (bench/lab sites), events wired into `events.rs` classes and the
  notification log.
- Tests: band assertion on the shipped rate; attrition fires after sustained zero-funds;
  dormant suspends the facility; solvency restores.

**Inputs**: Read `data/balance.ron`, `crates/csp-data/src/balance.rs`, `crates/csp-sim/src/economy.rs`,
`crates/csp-sim/src/events.rs`, `crates/csp-bench/src/economy.rs`, `csp-view/src/lib.rs` hire path.
**Outputs**: Modify the above; tests.
**Validation Criteria**: `cargo test -p csp-sim --lib -p csp-bench` green; workspace + clippy;
goldens (balance.ron data changes do not touch frozen fixtures; sim-code changes → bless if needed).

**Dependencies**: Phase 3.4 complete.

### 3.6 Rask day/night model + authored terminator longitude [C09-F3 + C34-F3 (M), C09-F2 (M)]

**Requirements**:
- Fix the tidal model at `crates/csp-sim/src/colonies.rs:431-436`: for
  `rotation_period_s: None` (tidally locked), the spin ω must be the parent's mean motion
  (locked to the parent per `rask/body.ron`'s own header "tidally locked = the orbital
  period"), and `synodic_rate = omega − n_parent_around_star` — verified correct value
  4.4525e-5 rad/s → solar day 141 115 s, night 70 558 s requiring ~247k EC vs the authored
  5 000 battery (2%). Non-locked bodies keep the current path.
- Rebalance the authored Rask outpost for the real night: battery capacity / module mix /
  `data/systems/home/colonies.ron` (fix the false "permanently daylit" comment at `:5-7`),
  accepting `Degraded{PowerLost}` cycles only if authored deliberately (document in-file).
- Move the rask-mines longitude off the terminator (`data/systems/home/sites.ron:70` sits at
  π/2 with a deciding margin of +6.1e-17 — one ulp flips Day↔Night permanently; verifier
  recommends ~2.4 rad, cos margin 0.737), and add a loader-time terminator-distance assertion
  for zero-synodic bodies in `csp-data/src/validate.rs` so no future site can ship on the line.
- Tests: synodic rate equals parent-derived value on locked moons; the authored colony passes
  the new validator rule; a full night cycle yields the authored behavior (degraded window
  bounded by battery).

**Inputs**: Read `crates/csp-sim/src/colonies.rs` (`body_light` :409-464), `data/systems/home/`
(body/sites/colonies), `crates/csp-data/src/validate.rs`.
**Outputs**: Modify `crates/csp-sim/src/colonies.rs`, `data/systems/home/{sites,colonies}.ron`,
`crates/csp-data/src/validate.rs`; tests.
**Validation Criteria**: `cargo test -p csp-sim --lib colonies:: -p csp-data` green; workspace +
clippy; goldens (colony solve changes → bless procedure).

**Dependencies**: Phase 3.5 complete.

### 3.7 Manufacture hardening — cancel/refund, verified BOM, berth pose, retry UX [C11-F3 (M), C11-F4 (M), C12-F1 (M), C21-F2 (M), C11-F6 (L), C11-F7 (L)]

**Requirements**:
- Add a build cancel/refund verb [C11-F3]: today a mis-order irreversibly consumes funds + full
  BOM (atomic spend at `manufacture.rs:192-249`; exits are delivery, unbounded berth-retry, or
  the site-vanish drop with no refund). `CancelBuild` → refund funds + stock, remove the
  build record + its pending `ConstructionComplete` (scrub the heap per the §1.3 pattern);
  wire a UI button; lint-reach clean.
- Server-side BOM/cost verification [C11-F4]: `apply`'s gate is positivity-only
  (`manufacture.rs:169-176`); the catalog is authoritative (`World.parts` +
  `PartDef::{cost_funds, build_resources}`; the resolver even computes both at
  `csp-phys/src/resolve.rs:1122-1135`). Verify the command's `build_resources`/`part_costs`
  against the catalog for the craft's `part_ids` (empty-`part_ids` free-build bypass must
  close: `check_part_gate` returns Ok when `part_ids` is empty).
- Berth-occupancy probe must pose the candidate exactly as delivery berths it [C12-F1]:
  extract a `berth_pose(def, &craft)` helper (upright `surface_upright(lat,lon)` +
  `altitude_m − belly`) and use it in `delivery_blocked` (`manufacture.rs:295-310`, currently
  IDENTITY/no-belly) — the launch path (`csp-view/src/lib.rs:2906-2941`) already uses the
  delivery convention, so share the helper.
- Berth-blocked retry UX [C21-F2]: surface a "berth blocked" reason and stop the per-sim-second
  "CONSTRUCTION COMPLETE" toast loop (`manufacture.rs:327-341` re-queues at `now+TICK_HZ` and
  every pop lands in `fired_events`; `BuildRow.eta_s` cycles ~1 s forever). Options: a distinct
  retry event kind that does not toast, or a dedup latch in the app — implement the sim-side
  distinct-kind route (cleaner for hash/pairing) and cap/notify the retry chain.
- Fix the wet-mass doc [C11-F6] (`manufacture.rs:88-89` claims wet; the mass walk is dry and
  tanks deliver full) and de-duplicate the launch toast's inline mass arithmetic [C11-F7]
  (`csp-view/src/lib.rs:3004-3026` re-implements `craft_mass_kg`; read `complete_tick` like
  the FACILITIES rows do).
- Tests: cancel refunds exactly; forged BOM refused; blocked berth does not spam events and
  delivers once cleared.

**Inputs**: Read `crates/csp-sim/src/manufacture.rs`, `crates/csp-sim/src/economy.rs`,
`crates/csp-view/src/lib.rs` (launch toast/build rows), `crates/csp-phys/src/resolve.rs`.
**Outputs**: Modify `crates/csp-sim/src/{lib,manufacture}.rs`, `crates/csp-view/src/lib.rs`;
tests.
**Validation Criteria**: `cargo test -p csp-sim --lib -p csp-view` green; `lint-reach` OK;
goldens (bless if needed); workspace + clippy.

**Dependencies**: Phase 3.6 complete.

### 3.8 Colony consequence model + small economy data [C09-F4 (M), C09-F6 (M), C09-F8 (L), C09-F9 (L), C08-F7 (M), C08-F6 + C33-F3 (M), C21-F8 (L)]

**Requirements**:
- Life-support consumption at LS-less colonies [C09-F4]: `ls_depleted` scans only LifeSupport
  modules (`colonies.rs:1132-1145`) and `module_requests` registers LS rows only when a module
  exists — crew at an LS-less colony are immortal and consume nothing. Minimal honest fix:
  colony crew consume from the colony's stores (or founder stores) regardless of module
  presence, and `AssignCrewToColony` warns/refuses when no LS module exists. The authored
  Rask colony and the UI starter def both lack LifeSupport today — adjust the starter def or
  the consumption source accordingly.
- Impact damage [C09-F6]: add a `NotificationKind` for colony damage (currently terminal,
  unnotified; the seam is live via debris within `transfer_radius_m = 100` on every
  surface-touching tick), and either add the long-promised repair command (the in-code doc
  at `colonies.rs:87-93` says it lands with the impact hook — which landed) or record the
  terminal-ness as a §6.5 decision. Minimal: notification + doc; repair command is 7.x scope.
- Terminal colony transitions must clear `vessel.joined_colony` [C09-F8]: abandon
  (`colonies.rs:1707-1764`) and LS-depleted (`:2183-2240`) leave the opt-in dangling;
  auto-clear on terminal transition (the manual leave button exists — make it unnecessary).
- `apply_found_colony` must reject a site already hosting a colony [C09-F9] (verified
  double-found via the raw command path; UI gates on `has_colony` which also blocks
  re-founding abandoned sites — decide and implement one rule: occupancy check at apply +
  re-founding allowed after abandonment, matching the UI's abandon-recovery economics).
- Hire skill [C08-F7]: `apply_hire_crew` hardcodes `skill: 0` (`economy.rs:839`) and the view
  floors to 1 — make skill a hire parameter (UI offers a band), have `apply_start_research`
  cross-check the claimed skill against the roster member (currently trusts the command).
- Dead unlock data [C08-F6 + C33-F3]: `unlocks.facilities`/`unlocks.contract_types` gate
  nothing (bench stand usable from tick 0 despite `facility.test_stand` at rocketry.3). Either
  wire them (bench gating reads the facility unlock; contract generation reads
  `contract_types` in addition to per-template prereqs with a validator cross-check) or
  remove the fields and the duplicated RON prereqs — wire is preferred (two sources of truth
  already differ in strength).
- Colony founding cost [C21-F8]: charge a funds/BOM cost at `apply_found_colony` (currently
  free while `AbandonColony` recovers 25%; §3.5's diagram routes funds into facilities).
  Replace the "prototype, no cost" UI label.
- Tests for each behavior change; update the csp-ui copy labels accordingly.

**Inputs**: Read `crates/csp-sim/src/colonies.rs`, `crates/csp-sim/src/economy.rs`,
`crates/csp-sim/src/tech.rs`, `crates/csp-data/src/techtree.rs`, `csp-view/src/lib.rs`
(starter def), `csp-ui/src/program.rs`.
**Outputs**: Modify the above; tests.
**Validation Criteria**: `cargo test -p csp-sim --lib -p csp-data -p csp-view` green; goldens
(bless); workspace + clippy.

**Dependencies**: Phase 3.7 complete.

**Phase-level Validation**: Full gate set + a phase-wide economy coherence pass: a single sim
test driving accept-resupply → load ore → deliver → complete → spend proceeds on a build,
using only shipped + newly-authored content. **Dependencies**: Phase 2 complete.

---

## Phase 4: UI/UX remainder

**Type**: Sequential (sub-phases grouped by crate; 4.1/4.2 share csp-ui; 4.4/4.5 share csp-sim transfer + csp-view map).

### 4.1 HUD panels draggable + navball parking + clock/chip nits [C23-F1 (H), C23-F2 (H), C23-F4 (N), C23-F5 (N), C23-F6 (N)]

**Requirements**:
- Readout block (`csp-ui/src/lib.rs:753-756` Area + label walls at `:785-819`, `:979-1009`):
  make all row labels `.selectable(false)` (egui 0.37 default-selectable labels win the press
  over the Area move widget — empirically verified; `notice_overlay` at `:723` already does it).
- Navball (`csp-ui/src/lib.rs:1018-1024`): add `.default_size(nav_size)` so the first-open
  sizing pass does not clamp against egui's nominal 600×400 and park the ball 242 px too high
  (verified `[360,200]` instead of `[412,442]` on 960×600; overlaps the readout block on
  narrow windows).
- `fmt_clock` (`csp-ui/src/lib.rs:643-652`): floor instead of round so 119.7 s renders "1:59"
  not "1:60" (used by T-AP/T-PE/DEP/BURN).
- `heading_tape` (`csp-ui/src/navball.rs:337-343`): use the shared `HUD_CHIP_FILL`/
  `paint_hud_chip` rule; extract the duplicated binding-join helper used by
  `help_panel`/`keymap_grid` (`lib.rs:1066-1075`, `settings.rs:866-877`).
- Tutorial card parking (`csp-ui/src/tutorial.rs:329`) overlaps the map orbit readout
  (`csp-ui/src/lib.rs:476-486`): park at left-top (Planning window's `(12,60)` row is free).
- Tests: the `hud_dump` contrast test stays green; add a first-open navball position assertion
  where the harness allows (headless egui run per the C23 verifier's method).

**Inputs**: Read `csp-ui/src/lib.rs`, `csp-ui/src/navball.rs`, `csp-ui/src/settings.rs`, egui 0.37 Area semantics (per C23).
**Outputs**: Modify `crates/csp-ui/src/{lib,navball,settings,tutorial}.rs`.
**Validation Criteria**: `cargo test -p csp-ui` (43 baseline) green; workspace + clippy.

**Dependencies**: Phase 3 complete.

### 4.2 Tutorial rewinds on its own coached recovery path [C23-F3] (M)

**Requirements**:
- `TutorialState::observe` (`csp-ui/src/tutorial.rs:264-267`): scope the
  `step > STEP_LAUNCH && !has_vessel` rewind to the tiers where a lost vessel means failure,
  and/or advance before rewinding — verified: the coached "land and recover" step banks
  science + removes the vessel in one tick, the next frame rewinds steps 8/9 → 2 with
  "vessel lost", and the research step can never complete until a new launch. Use
  `science_banked`/`research_spent` predicates to advance through the coached recovery
  instead of rewinding.
- Empirical test shape: `{step:8, science_banked>0, has_vessel:false}` must advance (a
  standalone replication harness is verified feasible; prefer an in-crate unit test on
  `observe`/`step_complete`).

**Inputs**: Read `csp-ui/src/tutorial.rs`, `csp-view/src/tutorial.rs` (mirror staging).
**Outputs**: Modify `crates/csp-ui/src/tutorial.rs`; test.
**Validation Criteria**: `cargo test -p csp-ui` green; workspace + clippy.

**Dependencies**: Phase 4.1 complete (same crate).

### 4.3 Reentry-plasma window mis-calibrated — ascent glows brighter than reentry [C19-F1] (H)

**Requirements**:
- Recalibrate `PLASMA_ONSET_W_M2 = 5e3` / `PLASMA_FULL_W_M2 = 8e4`
  (`csp-view/src/flight.rs:46-48`): verified intensities with the shipped driver
  (η=0.005, `i = clamp((flux−5e3)/7.5e4)`) put a normal mid-ascent at i ≈ 0.25–0.51 through
  7–24 km while a 40 km reentry reads i ≈ 0.33 — every launch carries a visible sheath and
  ascent/reentry are indistinguishable mid-band, violating the §3.7 effect's stated identity
  ("reentry plasma sheath"). Move the onset above the ascent band (onset such that
  ascent flux 2.4–4.4e4 stays dark and reentry flux ≥ ~8e4 reaches full; e.g. onset ~5e4,
  full ~1.2e5 — derive from the verified flux table) or gate on dynamic-pressure·speed
  product; keep constants documented with the flux math in the comment.
- Test: a parametrized intensity test over the verified altitude/speed table (sea-level
  200 m/s dark; 12 km @ 450 dark or ≤0.1; 35 km @ 2300 saturated; 40 km @ 2200 ≥ 0.5).

**Inputs**: Read `csp-view/src/flight.rs:42-48, 1022-1028, 2555-2577`, verified-report-C19 numbers.
**Outputs**: Modify `crates/csp-view/src/flight.rs`; test.
**Validation Criteria**: `cargo test -p csp-view` green; workspace + clippy.

**Dependencies**: Phase 4.2 complete (phase order).

### 4.4 Transfer planning truthfulness — staged node cannot reach its destination; window-watch dead-end [C07-F1 (H), C07-F2 (M), C06-F2 (M)]

**Requirements**:
- Staged-transfer correction loop [C07-F1]: the panel's `departure_node` burn, flown exactly,
  misses Duva by 9.44e8 m (19.7 SOI radii) with no in-sim correction (post-egress
  `orbit.parent != physics.dominant` refuses nodes/throttle; the module's own doc says the
  correction loop is REQUIRED). Minimal honest fix (pick one, implement fully):
  (a) compose the egress at plan time like the test's private `solve_egress_hyperbola`/
  `design_duva_injection` (lib.rs:17904-18140) — expose that math through csp-sim's public
  transfer API and have `departure_node` price the node that actually arrives; or
  (b) make the panel's outcome row honest (show the post-egress miss / "uncorrected
  transfer"), remove the sold center-hit encounter row, and document the v1 limitation.
  Route (a) is preferred for playability; scope to the sibling-pair (planet↔planet) case.
- Window-watch dead-end [C07-F2]: `burn_tick <= departure_tick` always, so waiting for the
  window alarm then creating the node always refuses "ejection: burn precedes now"
  (`transfer.rs:1631-1633`; map re-resolves at current clock, `map.rs:1225-1257`). Fix: the
  window alarm must fire at `burn_tick`-lead (or `CreateNode` at/past the window must offer
  the NEXT burn pass rather than refusing) — verified parking period 2 392 s vs 2 125 s lead.
- Executor burn lead [C06-F2]: the executor burns forward from `node.tick`
  (`lib.rs:6698/6787-6796/9230-9261`) while `predict_maneuver` applies Δv impulsively at the
  tick — the effective impulse trails by T_burn/2 (the in-repo test
  `duva_reachable_in_sim_end_to_end` manually pulls the node half a burn early at
  lib.rs:18279-18284; players/UI get no correction). Add the half-burn lead at node
  creation (both `SetManeuver` handling and `transfer::departure_node`) or schedule
  `ManeuverNodeStart` at `tick − burn_ticks/2`; keep the executor agreement test green.
- Tests: staged node flown end-to-end reaches the target SOI (or the honest-outcome
  variant asserts the miss row); window alarm → node creation succeeds; a 300 s burn's
  achieved conic matches the predicted conic within the existing 1e-3 element budget.

**Inputs**: Read `crates/csp-sim/src/transfer.rs` (`departure_node` :1529-1633), `csp-sim/src/maneuver.rs`,
`csp-view/src/map.rs` (plan staging, CreateNode, outcome row), verified-report-C07 probe numbers.
**Outputs**: Modify `crates/csp-sim/src/{transfer,lib,maneuver}.rs`, `crates/csp-view/src/map.rs`,
`csp-ui/src/lib.rs` (outcome copy); tests.
**Validation Criteria**: `cargo test -p csp-sim --lib transfer` + workspace green; goldens
(bless — node timing changes hashes); clippy.

**Dependencies**: Phase 4.3 complete.

### 4.5 Map panel dead-clicks and samplers [C18-F1 (M), C18-F2 (M), C18-F3 (M), C18-F4 (L), C18-F6 (N)]

**Requirements**:
- `ArmApsis` dead clicks (`csp-view/src/map.rs:1145-1153`): surface `Err`/`Ok(None)` through
  `plan_error` (Ap on any open conic; Pe once outbound past it) instead of a bare `return`;
  add an "approximate — osculating refit" hint for InBubble focus (per-frame refit at
  `map.rs:2360-2377`).
- Closed-orbit polyline: switch the closed branch (`map.rs:2437-2447`) from uniform-in-M to
  uniform-in-E sampling — verified ν-gap at e=0.9 drops 82.6° → ~12.2° (not the report's
  5.3°; use the verifier's bound), same 129 `state_at` calls, no cache-key change.
- Plan-route polyline key (`map.rs:1740-1744`): key on `(departure_tick, duration_ticks)`
  (conic depends on both), and only set the key when sampling succeeds.
- Same-label manual alarms (`map.rs:876-894`): match `PlayerAlarm` fires by (label, tick)
  or consume the latest-armed first, so "burn" T+1h/T+1min rows do not cross-fire.
- Fix the `porkchop_query` doc ("now + 1 day" vs the 6 h constant at `map.rs:1015`).

**Inputs**: Read `csp-view/src/map.rs`, verified-report-C18.
**Outputs**: Modify `crates/csp-view/src/map.rs`; tests where feasible (sampler gap math as a unit test).
**Validation Criteria**: `cargo test -p csp-view` green; workspace + clippy.

**Dependencies**: Phase 4.4 complete (same files).

### 4.6 VAB readout parity — stage rows model ignitions flight cannot perform; pooled drain makes staging counterproductive [C13-F1 (H), C13-F2 (H)]

**Requirements**:
- Stage engine sets [C13-F1]: the readout (`csp-phys/src/readout.rs:217-222`) models
  per-stage engine ignition ("engines this stage ignites") while the sim/HUD fire every
  attached engine at vessel-wide throttle (`simple.rs` thrust sites;
  `propellant_plans` lib.rs:9027-9050; `burn_step` flight.rs:2089-2197). Verified divergence
  on the stock two-engine fixture: readout burn 274.7 s / TWR 0.857 vs flight 187.3 s / 1.285,
  and `MissionProfile::evaluate` rejects flyable craft. Fix the readout's per-stage engine
  set to match the sim's attached-set model (stage boundary = jettison, not ignition), so
  VAB numbers equal flight delivery. Decide and document the design: either (a) align the
  readout to the shipped sim (recommended, one-directional today), or (b) add per-engine
  activation to the sim (feature work — defer to 7.x if chosen).
- Fuel drain order [C13-F2]: vessel-wide pooling + ascending-id drain
  (`transfers.rs` `debit`; readout `:282-291`, `:333-348`; flight `consume_drawn`) makes the
  upper tank drain first through the decoupler, so staging is strictly counterproductive
  (readout's own test asserts the 0-Δv starved stage-0 row). Minimal fix consistent with (a):
  model the drain order honestly in all three surfaces (they already agree) and add the
  missing validator warning for cross-stage starvation (`missing_propellants` checks
  capability in the supply region, not cross-stage depletion — verified it cannot catch this).
  A real crossfeed/priority model is feature work (7.x).
- Keep `two_stage_readout_separates_mass_and_propellant` semantics updated to the new model;
  add a fixture test asserting VAB stage-1 (burn, TWR, Δv) == flight `burn_step` for the
  canonical stack.

**Inputs**: Read `csp-phys/src/readout.rs`, `csp-sim/src/lib.rs:9027-9050`, `csp-view/src/flight.rs`,
`crates/csp-phys/src/session.rs` (`missing_propellants` :2138-2199).
**Outputs**: Modify `crates/csp-phys/src/readout.rs`, `crates/csp-phys/src/session.rs`; tests.
**Validation Criteria**: `cargo test -p csp-phys` green; new parity test passes; workspace + clippy.

**Dependencies**: Phase 4.5 complete.

### 4.7 VAB editor UX remainder [C17-F3 (M), C17-F6 (L), C17-F8 (L), C10-F3 + C13-F7 (M), C22-F6 (L)]

**Requirements**:
- Split `craft_epoch` from pick-readback generation (`csp-ui/src/vab.rs:1104-1118` bumps it on
  every undo/redo → `vab_present` resets zoom/pan, `csp-view/src/vab.rs:278-282`, `:387-390`):
  introduce `pick_generation` for the stale-pick guard; only `New`/`LoadBytes` bump the
  camera-refit `craft_epoch` (per the fields' own docs).
- VAB confidence must read the live qualification table (`csp-ui/src/vab.rs:3143`
  `FamilyQualifications::default()` → `sim.tech().qualifications`, already staged for
  FACILITIES rows at `csp-view/src/lib.rs:4650-4670`) and the "Mission profile (stub)" panel
  should edit burn segments or honestly label them fixed (`vab.rs:3156-3182` edits only
  Δv/TWR over the fixed `terran_ascent` preset) [C10-F3 + C13-F7].
- Remove or wire the dead `VabAction::Translate` (dispatched, tested, zero production
  issuers — the root-grab + empty-drop path covers the capability) [C17-F6].
- Craft-save serialization failure must not be silent (`csp-view/src/lib.rs:846-850`
  `.ok()` swallows; the module's contract says no op is ever silent) [C17-F8].
- Save-name field: do not clear `state.save_name` before validation
  (`csp-ui/src/game.rs:86-90`); keep the typed text on refusal [C22-F6].

**Inputs**: Read `csp-ui/src/vab.rs`, `csp-ui/src/game.rs`, `csp-view/src/vab.rs`, `csp-view/src/lib.rs`.
**Outputs**: Modify the above; tests where the harness allows.
**Validation Criteria**: `cargo test -p csp-ui -p csp-view` green; workspace + clippy.

**Dependencies**: Phase 4.6 complete.

### 4.8 Program-panel gating and copy [C22-F2 (M), C22-F3 (M), C22-F7 (L), C22-F8 (L), C21-F7 (L), C10-F2 + C22-F5 (M)]

**Requirements**:
- Gate the window's chute "deploy" button on `residence == "bubble"` like the part menu and
  action strip do (`csp-ui/src/program.rs:2060-2076` is the lone ungated surface; the sim
  refuses non-bubble) [C22-F2].
- Gate the window's lab "research" button on crew aboard (the resolver drops with
  "NO CREW ABOARD"; stage a `crew_aboard` flag — crew ids are already staged) [C22-F3].
- Distinguish "lab node not on the focused vessel" from "no crew aboard" in the drop notice
  (`csp-view/src/lib.rs:1208-1221` conflates via `unwrap_or(0)`) [C21-F7].
- Relabel quick-transfer buttons when stock < 10 (buttons labeled "10" send
  `min(10, units)`, duplicating "all") [C22-F7].
- Explain banked-vs-live science on the "need ⚛" tag (headline number includes live accrual;
  gating is banked-only) via hover copy [C22-F8].
- Flight-side stress multipliers [C10-F2 + C22-F5, same finding in both clusters]: add the
  §3.4 stress-rule/multiplier breakdown to the flight RELIABILITY surface (the live product
  is computed per tick at `reliability.rs:511`; the data exists — stage and display it).
  PLAN §3.4: "displayed to the player in editor and flight".

**Inputs**: Read `csp-ui/src/program.rs`, `csp-view/src/lib.rs` (staging), `crates/csp-sim/src/reliability.rs`.
**Outputs**: Modify `crates/csp-ui/src/program.rs`, `crates/csp-view/src/lib.rs`,
`crates/csp-view/src/flight.rs`; tests.
**Validation Criteria**: `cargo test -p csp-ui -p csp-view` green; workspace + clippy.

**Dependencies**: Phase 4.7 complete.

### 4.9 Flight view polish + frame-path perf [C19-F5 (M), C19-F6 (N), C19-F7 (N), C25-F4 (N), C20-F3 (L)]

**Requirements**:
- End the per-frame deep-clone chain (`flight.rs:1013` per-body `Vec<FlightPart>` clone →
  `sync_flight` `clone_from` → tank clone): move to `Arc<Vec<FlightPart>>` fingerprints
  (immutability argument verified at `flight.rs:1000-1012`) and/or `mem::take` the staged
  snapshot after `stage_program` consumes it (verified sole read at `csp-view/src/lib.rs:3597`).
- HUD "STG" fallback: label the borrowed plan ("newest plan — focus has none") or drop the
  fallback so SPACE cannot jettison another vessel's parts silently (`flight.rs:804-815`,
  `lib.rs:3190-3200`) [C19-F6].
- Breakup focus: prefer the retained hull / a non-debris fragment over
  `vessels.last()` (`flight.rs:1965-1967` does not filter debris; fragments take the camera
  by construction) [C19-F7].
- Stop the engine rumble after a Fatal halt (gate `backend.update` on `fatal.is_none()` or
  zero the engine voice on halt; `csp-app/src/lib.rs:1012-1027` runs unconditionally)
  [C25-F4].
- Add a rendered structure for the "test stand" the FACILITIES panel claims
  (`csp-view/src/structures.rs` has none among the `SPACE_CENTER` pieces) or fix the copy
  [C20-F3].

**Inputs**: Read `csp-view/src/flight.rs`, `csp-view/src/lib.rs`, `csp-app/src/lib.rs`, `csp-view/src/structures.rs`.
**Outputs**: Modify the above; tests.
**Validation Criteria**: `cargo test -p csp-view -p csp-app` green (incl. `vab_latency`-style
frame tests); workspace + clippy.

**Dependencies**: Phase 4.8 complete.

### 4.10 Editor↔flight model parity [C20-F1] (M)

**Requirements**:
- Unify `model_key` (`csp-view/src/art.rs:375-435`): one classification shared by the VAB
  (`csp-view/src/vab.rs:319-348`, id-substring + inline 4-way kind) and flight
  (`csp-view/src/flight.rs:1320-1346`, 7-way kind omitting `WeldPart.parachute`).
  Verified swaps on the flagship recovery part and three others: chute renders "chute" in
  VAB / "adapter" in flight; ember renders engine_b (VAB) / engine_a = the big first-stage
  bell (flight); decoupler "decoupler"/"adapter"; dockport "adapter"/"decoupler".
  Extend `part_kind(&WeldPart)` to read `wp.parachute` and route both callers through one
  key function (id-first, kind-second, documented).
- Test: assert VAB and flight resolve the same model key for every shipped part catalog id
  (the packs decode test at `art.rs:445-455` can host it).

**Inputs**: Read `csp-view/src/art.rs`, `csp-view/src/{vab,flight}.rs`, `crates/csp-phys/src/part.rs`.
**Outputs**: Modify `crates/csp-view/src/{art,vab,flight}.rs`; test.
**Validation Criteria**: `cargo test -p csp-view` green; workspace + clippy.

**Dependencies**: Phase 4.9 complete.

### 4.11 Telemetry dashboard — vessels.0 dies after first recovery; program widgets absent [C28-F1 (C), C28-F2 (H), C28-F5 (M), C28-F6 (M), C28-F7 (N), C28-F8 (N), C28-F9 (N)]

**Requirements**:
- Populate `focus.*`: `from_world_focus` (`csp-server/src/telemetry.rs:128`) has zero callers —
  every publish point (`backend.rs:643`, `:710`, `:856-886`) calls `from_world` with
  `focus: None`. Publish the focused/active vessel (Phase 2.6's `active_vessel` is the
  natural source; on the headless host use the newest live vessel) and switch
  `apps/telemetry/dashboards/default.json` from hardcoded `vessels.0.*` (all 8 widgets) to
  `focus.*` — verified: ids are monotonic and never reused, so after the first
  `RecoverVessel` every widget shows "unknown field" forever, and recovery is the standard
  progression endpoint.
- Ship program widgets: `ProgramSnapshot` fields (`telemetry.rs:52-73`) are subscribable but
  zero widgets use them; add program rows/widgets to the default dashboard (funds, science,
  offers, colonies) and a dashboard switcher — `GET /api/dashboards` exists
  (`dashboard_list`) but the SPA never calls it and hardcodes `loadDashboard('default')`
  (`apps/telemetry/src/routes/index.tsx:35`).
- Orbit-map clip: `scale = 88/ra` clips near-circular orbits by up to 8 px vertically
  (`orbit-map.tsx:72`, viewBox 200×160, focus cy=80) — fit both axes.
- Reconnect: add a manual retry affordance after the 5-attempt give-up (`ws.ts:228-244`;
  only path today is "reset token" → re-paste).
- Replace the WS-boundary `any` (`ws.ts:176`) with a typed unknown + narrowing guards, and
  the orbit-map `as` casts (`orbit-map.tsx:35-40`) with runtime guards; delete the dead
  `WIDGET_KINDS` export (`registry.tsx:31`) or use it; unify the `num()` helper
  (triplicated in gauge/graph/orbit-map) into `lib/utils.ts`; fix the `fields[0]`-missing
  message to match the `fields[1]` explanatory branch (`orbit-map.tsx:42-48` vs `:129-131`).
- Tests: first SPA tests (reconnect state machine, field-path fallback) — the package has
  zero tests and no `test`/`typecheck` scripts (CI wiring lands in Phase 5.2); add
  `typecheck` + `test` scripts now.

**Inputs**: Read `csp-server/src/{telemetry,backend,dashboard,http}.rs`, `apps/telemetry/src/**`,
`apps/telemetry/dashboards/default.json`.
**Outputs**: Modify `crates/csp-server/src/{telemetry,backend}.rs`, `apps/telemetry/**`; tests.
**Validation Criteria**: `cargo test -p csp-server` (lib + e2e) green; `npx tsc --noEmit`
clean; new npm test script green; workspace + clippy.

**Dependencies**: Phase 2.6 (active_vessel) and Phase 4.10 complete.

### 4.12 Server surface remainder [C27-F1 (H), C27-F3 (M), C27-F4 (M), C27-F5 (M), C27-F6 (M), C27-F7 (L), C24-F5 (M), C24-F6 (L), C21-F4 (M), C25-F3 (N)]

**Requirements**:
- Parse `--serve` flags [C27-F1]: implement `--port <u16>` and `--bind-lan` in
  `csp-app/src/main.rs` + `serve_cli` (`lib.rs:2996-2999` builds `ServerConfig::default()`
  = hardcoded 127.0.0.1:47900), reject unknown flags loudly. The LAN machinery
  (`BindAllowlist`, `for_bind`) is coded and e2e-tested but unreachable from any shipped
  binary; `csp --serve --port 5000` silently binds 47900. `--bind-lan` must print the
  warning and keep the token requirement per PLAN §2.12.3.
- Quaternion wire order [C27-F3]: document `[w,x,y,z]` in the `vessel.control` tool
  descriptor (`mcp.rs:762-773`), fix the false "DQuat's serde order is (w,x,y,z)" comment
  (`:728`), note the telemetry side emits `{x,y,z,w}` objects, and add a round-trip test
  pinning the order (a permutation passes the norm gate at `csp-sim/src/lib.rs:5279`
  silently — verified).
- Zombie server [C27-F4]: make a `View::new()` failure in the host builder
  (`csp-app/src/lib.rs:2980-2984` panics; thread dies; the pre-spawn empty snapshot keeps
  200-ing forever; mutations answer "game thread gone"; no non-zero exit) exit non-zero with
  a clear message — propagate the error out of `spawn_full`/`run_blocking` instead of
  panicking inside the thread.
- Discoverability [C27-F5]: expose site ids (and minimally craft/part definition summaries)
  over MCP so `ManufactureCraft` is authorable from the protocol (today `ProgramSnapshot.sites`
  is a `usize`; only the raw `sim.command` passthrough exists).
- Docs/comments [C27-F6, C27-F7]: fix the stale "SPA not wired into any binary" comments
  (`http.rs:52-56`, `e2e.rs:78-79`) and the DATA-AND-SCRIPTING "every step is exercised in
  e2e.rs" overstatement (`:748` — science verbs and wire save/load are absent; either add
  the e2e steps or correct the doc).
- Saves-dir divergence [C24-F5]: `serve_cli` writes `config_dir()/saves` while the app uses
  relative `saves/` under CWD — align on one canonical location and fix the false doc
  promise at `csp-app/src/lib.rs:2969-2970`.
- Header-only save listing [C24-F6]: `stage_save_list` (`lib.rs:1232-1234`) slurps whole
  files for `inspect_save`, which needs only the framing + JSON header — read a bounded
  prefix (length is in the first 4 bytes); fix the misleading "O(header)" comment.
- `--serve` attach semantics [C21-F4]: PLAN §2.12 says the HTTP host attaches to a running
  game; the shipped host is a fresh headless world with no starter seed. Either wire the
  frame-loop attach (`backend.rs:820-825` sketches it) or record the v1 deviation in
  docs per §6.5 — minimal: seed the hosted world + document.
- Bug-report log redaction [C25-F3]: redact `$HOME` from `log.txt` paths
  (`settings_io.rs:92-95`, `:112-115`, `bugreport.rs:87`, `lib.rs:635` all embed
  `path.display()` into the ring-captured tail).

**Inputs**: Read `csp-app/src/{main,lib}.rs`, `csp-server/src/{serve,http,mcp,backend}.rs`,
`docs/DATA-AND-SCRIPTING.md`.
**Outputs**: Modify the above + docs; tests (flags, quat round-trip, non-zero exit, prefix read).
**Validation Criteria**: `cargo test -p csp-server -p csp-app` green incl. e2e; workspace + clippy.

**Dependencies**: Phase 4.11 complete.

### 4.13 Physics remainder — aero memo blindness, thermal η, torque arm [C14-F2-replacement (M), C14-F3 (M), C14-F4 (M), C12-F2 (L), C12-F4 (L), C12-F6 (L), C12-F5 (L)]

**Requirements**:
- Aero staleness [C14-F2, verifier-corrected]: the debit/credit/leak path mutates
  `craft.parts[*].mass_kg` in place (`transfers.rs:484`), violating the `CraftHashMemo` write
  rule (`bubble.rs:130-134`) — the memo stays pre-burn forever, so tables (and their
  `com_local` torque arms) never rebuild during burns (verified 0.39 m CoM error after one
  tank) AND any memo fix would turn every burn into a per-quantum 7.6–66 ms rebuild storm.
  Fix both directions: geometry-only aero-table key + patch `com_local` in the re-weld
  path (`transfers.rs:629-648` already re-welds on quantized mass and already clones the
  table). Do NOT simply "fix" the memo.
- Thermal η [C14-F3]: move `CONVECTIVE_HEAT_FRACTION = 0.005` (`thermal.rs:120`) to
  `balance.ron` as its own doc says it should be; leave the shipped value unless design
  wants lethality (lethality calibration is 7.x — verified peaks 208–259 K vs 800–1600 K
  limits on a 2.3 km/s steep entry).
- Joint-load torque arm [C14-F4]: use the snapped attach point (`snap_transform`,
  `session.rs:913`) instead of part origin in `structural.rs:210-217` (`p_j = transforms[id].offset`
  under the KNOWN-DEFERRED note; axial/shear are moment-independent so ascents are safe, but
  the channel governs real breakups with the wrong lever arm).
- Doc-only [C12-F2]: correct the µm-justification in `simple.rs:74-78` — the normal error is
  the slope angle (≤ ~6° on shipped fields), not µm; note the static-friction corollary
  (μ=0.5 ⇒ 26.6° friction angle vs ≤5.8° max shipped slopes — no behavior change sanctioned).
- `facet_planes` laziness [C12-F4]: memoize per-hull planes (`Vec<Option<Vec<Plane>>>` within
  the call) so the O(n⁴) sweep runs only for pairs that pass the AABB filters
  (`simple.rs:1781-1821`; part.rs's own doc already expects pair-candidate scope).
- `cast_ray` [C12-F6]: fix the exit-root normal doc (`backend.rs:126-128`) and note the
  96-step scan's graze-miss limitation in-code (no production caller today).
- Latched-floating [C12-F5]: add a support-departure re-evaluation for `SurfaceLatched`
  records (unlatch when nothing remains beneath — the deterministic self-healing path made
  automatic), or document the known-oddity; prefer the re-evaluation on the unlatch-trigger
  seam (`simple.rs:211-213` lists the unwired triggers).

**Inputs**: Read `csp-phys/src/{simple,thermal,structural,aero,weld,backend}.rs`,
`crates/csp-sim/src/transfers.rs`, `crates/csp-sim/src/bubble.rs`, verified-report-C14/C12.
**Outputs**: Modify `crates/csp-phys/src/*.rs`, `data/balance.ron`, `crates/csp-data/src/balance.rs`;
tests.
**Validation Criteria**: `cargo test -p csp-phys` (157 baseline) + workspace green; goldens
(physics changes → bless); clippy; `csp-perf` spot-run if the tick loop changed
(`cargo run -p csp-bench --bin csp-perf -- --out /tmp/perf.ron`) with no >10% regression.

**Dependencies**: Phase 4.12 complete.

**Phase-level Validation**: Full gate set; phase-wide validator confirms no UI copy promises
mechanics the sim refuses (spot-check the gated buttons), and that the SPA/server contract
(`focus.*`, program paths) matches between `telemetry.rs` and `default.json`.
**Dependencies**: Phases 1–3 complete (2.6 for `active_vessel`).

---

## Phase 5: PRD-coverage and spec-debt

**Type**: Sequential; 5.2/5.3/5.4/5.5 are parallel-safe by file.

### 5.1 §2.6.6 item 4 — `scan_strides` coded but not wired (no surface reports h) [C06-F1] (H)

**Requirements**:
- Wire the stride guarantee into a player/agent-visible surface: expose the SOI scan's
  effective stride `h` (and ideally `v_rel_max`) through the M0 debug panel
  (`csp-ui/src/lib.rs:170-207` currently mirrors adapter/backend/tick/frame/warp only) and/or
  into the bug-report bundle (`csp-app/src/bugreport.rs`, zero soi/stride/scan matches
  today). PLAN §2.6.6 item 4: "`h` is reported in the debug overlay. … the guarantee is
  'never misses a residence longer than h'." `PatchedConics::scan_strides`
  (`crates/csp-sim/src/orbit.rs:1293`) has zero non-test consumers (verified workspace-wide).
- Test: a csp-sim test asserting the reported h matches the scan's own stride computation
  for a fixture conic.

**Inputs**: Read `csp-sim/src/orbit.rs:1288-1305`, `csp-ui/src/lib.rs:170-207`, `csp-app/src/bugreport.rs`.
**Outputs**: Modify `crates/csp-sim/src/orbit.rs` (accessor if needed), `crates/csp-ui/src/lib.rs`,
`crates/csp-app/src/bugreport.rs`; test.
**Validation Criteria**: `cargo test -p csp-sim --lib -p csp-ui -p csp-app` green; workspace + clippy.

**Dependencies**: Phase 4 complete.

### 5.2 CI §8.2 tier — weekly/nightly + missing per-commit gates; bake-art staleness check [C35-F1 (H), C35-F2 (H), C29-F1 (M), C29-F2 (M), C35-F6 (L), C35-F7 (N), C28-F4 (M, CI half), C17-F5 (M, CI half)]

**Requirements**:
- Add the missing §8.2 rows (8 verified absent + 4 partial): coverage (`llvm-cov` +
  `ci/coverage-floors.json`), docs (`cargo doc` + `missing_docs` workspace lint config),
  mutation (cargo-mutants scheduled), MSRV (verify `rust-version = "1.95"` builds),
  nightly golden-full tier (longer scenarios — pairs with 5.6), physics oracle (needs 7.x
  RapierBackend decision — schedule the job only if/when it exists; otherwise record the
  deferral per §6.5), cross-platform report, machete/udeps on the weekly tier, and
  `xtask lint-quality` (or record its deferral). Split per-commit vs weekly/nightly exactly
  as PLAN §8.2 assigns them (Coverage and Docs are per-commit, not weekly — verifier nuance).
- bake-art staleness [C35-F2]: add `--check` to `xtask/src/bin/bake_art.rs` (byte-compare
  like `gen_biome_maps.rs:267-290`) and wire it into CI — 1 of the last 6 bake-input commits
  shipped without a rebake; the packs are `include_bytes!`-embedded and the data-set hash
  deliberately excludes `data/art/**`, so nothing else can catch it.
- Perf hygiene [C29-F1/F2]: triage or re-baseline the two red rows
  (`sim.200-part-atmo` 0.672/0.5, `sim.1000-part-orbit` 4.634/2.0 — 57 commits and +58k LOC
  past the baseline commit), record the deviation reason in the row `detail`, and add a
  ci-host-measured baseline so the 10% band is not applied cross-machine blindly.
- VAB 16 ms release gate [C17-F5]: run the release-profile `vab_latency` assert in CI
  (a release test job or move the probe into `csp-perf`), and add a VAB row to the perf log.
- SPA CI [C28-F4]: add a node job (or xtask step) running `npx tsc --noEmit` + the new SPA
  tests + `npm run build`.
- Small tooling debts [C35-F7]: remove the redundant `dtolnay/rust-toolchain@stable` install
  (the pinned file wins), fix `lint_reach.rs:142` `let _ = &issuers;`, drop the stale
  `.hdr` feature + sky comment in `xtask/Cargo.toml`.

**Inputs**: Read `.github/workflows/ci.yml`, `xtask/src/bin/{bake_art,gen_biome_maps}.rs`,
`crates/csp-bench/src/bin/csp-perf.rs`, `docs/perf/perf-log-devbox-release.ron`, PLAN §8.2.
**Outputs**: Modify `.github/workflows/ci.yml` (new jobs/schedules), `xtask/**`,
`docs/perf/**`, `ci/coverage-floors.json` (new); no product code.
**Validation Criteria**: CI config valid (actionlint or a dry-run where available);
`cargo run -p xtask --bin bake_art -- --check` passes on the current tree;
`cargo run -p xtask -- lint-deps && lint-attribution && lint-reach` OK; `cargo test --workspace` green.

**Dependencies**: Phase 5.1 complete.

### 5.3 lint-reach parser false negatives + §8.2 hygiene [C35-F3] (M)

**Requirements**:
- Fix the two parser classes in `xtask/src/lint_reach.rs:46-88`: (a) a line containing
  `ui-unreachable:` is consumed BEFORE variant-row detection, so a trailing-comment variant
  vanishes from the checked set and the annotation exempts the NEXT variant (empirically
  reproduced by the verifier with a standalone compile); (b) issuer detection is
  `text.contains("SimCommand::Name")` over comment-bearing non-test source — strip
  comments/strings in `production_sources` (`:94-112`) so doc mentions cannot satisfy the
  gate. Keep the lint green on the current 53-variant enum (44 code issuers + 9 annotations).

**Inputs**: Read `xtask/src/lint_reach.rs`, `csp-sim/src/lib.rs` (SimCommand decl).
**Outputs**: Modify `xtask/src/lint_reach.rs`; add parser unit tests covering both classes.
**Validation Criteria**: `cargo run -p xtask -- lint-reach` OK on the tree; new unit tests
prove both false-negative classes are closed.

**Dependencies**: Phase 5.2 complete (xtask ordering).

### 5.4 Data-validator PRD gaps — ID stability, colony cross-fields, launch-site floor, loaders [C32-F3 (M), C32-F4 (M), C32-F5 (M), C31-F1 (M), C31-F2 (M), C31-F3 (M), C31-F4 (M)]

**Requirements**:
- ID-stability tripwire [C32-F3]: PLAN §7.7 requires CI failure on a disappearing `PartId`
  without a migration/`Deprecated(replacement)` entry — add a baseline ID list (committed
  under `data/` or generated) + a validator/xtask check; deleting a part file must fail.
- Colony cross-field validation [C32-F4]: stores-without-capacity (silent 0-anchor),
  over-capacity clamps (warn), drill resource vs the founding site's `resource_yields`
  (barren-ground drill) — inputs already in `validate_colonies`' signature.
- Launch-site floor [C32-F5]: file-level "≥1 launch site" rule (today deleting the pad row
  validates green and every build order fails at runtime with an unnamed cause); add
  negative tests for `starting_stock` (validator exists, tests absent).
- Loader policy [C31-F1/F2]: either implement the §3.1 current-and-previous band beyond
  sites.ron (colonies/balance accept exactly one version today) or record the per-file-type
  policy in a §7.7-adjacent deferral note; split the shared `system::SCHEMA_VERSION`
  constant that gates system/body/biomes/loc (a BodyDef bump would refuse all four);
  remove the per-entry `schema_version` duplication in `SiteDef` (sites.ron declares it 4×);
  fix the stale `UnsupportedSchemaVersion` doc ("v1 and v2" vs band [2,3]) and make
  `check_version` report the accepted band.
- Orphan content [C31-F3]: warn on unreferenced body directories / stray biome maps beside
  `biome_map: None` bodies (manifest-driven load silently ignores them; only the data-set
  hash flips).
- Windows name guard [C31-F4]: reject `:` and DOS device names in `authored_name_ok`
  (`load.rs:382-384`) — `dir.join("C:x")` drops the base on Windows (drive-relative escape).

**Inputs**: Read `crates/csp-data/src/{validate,load,site,balance,colony,system}.rs`, PLAN §3.1/§7.7/§8.2.
**Outputs**: Modify `crates/csp-data/src/*.rs`; tests (negative validation tests per rule).
**Validation Criteria**: `cargo test -p csp-data` (121 baseline + new) green; deleting a part
file in a scratch tree fails the new gate; workspace + clippy.

**Dependencies**: Phase 5.3 complete (parallel-safe by crate).

### 5.5 §7.5 save round-trip gate at N=10 000 + replay schema diagnostics [C35-F4 (M), C29-F3 (M), C29-F6 (L)]

**Requirements**:
- Implement PLAN §7.5's per-commit gate: save → load → N ticks hash-compared against
  uninterrupted N ticks, N = 10 000, across the scenario set (today only single-shot
  round-trip equality + one N=64 seam test at `csp-sim/src/lib.rs:19513` exist). Add it as a
  csp-sim/csp-bench test; keep runtime acceptable (tagged/`#[ignore]`-into-nightly if slow —
  then wire into the 5.2 nightly tier).
- Replay artifact diagnostics [C29-F3]: add a magic/version header to `replay.bin`
  (`crates/csp-bench/src/replay.rs:36-38` is headerless bincode; a stale artifact decodes to
  exit 2 "harness error", indistinguishable from corruption) and consult the sidecar
  fingerprint on decode failure to name the version mismatch. Note §7.6 exempts replays
  from the migration promise — this is diagnostic quality only.
- Fix the stale "Known issue" in `docs/DATA-AND-SCRIPTING.md:377-379` (the flight-launch
  golden passes at HEAD; verified exit 0).

**Inputs**: Read `crates/csp-bench/src/replay.rs`, `crates/csp-sim/src/save.rs`,
`docs/DATA-AND-SCRIPTING.md`.
**Outputs**: Modify `crates/csp-bench/src/replay.rs`, csp-sim/csp-bench tests, docs.
**Validation Criteria**: `cargo test -p csp-bench` + workspace green; `csp-replay verify`
both goldens exit 0; clippy.

**Dependencies**: Phase 5.4 complete (parallel-safe by crate).

### 5.6 Eval tier — WorldSpec::Data + contract/colony §8.5 proofs [C30-F1 (H), C30-F2 (H), C30-F3 (M), C30-F4 (M), C30-F5 (M), C34-F4, C05-F6 (L)]

**Requirements**:
- Add a `WorldSpec::Data { root }` variant (`crates/csp-bench/src/scenario.rs:62-72` has only
  `Empty|Fixture`; the fixture ships no contracts/recipes/experiments/colonies, so
  `AcceptContract`/`ManufactureCraft`/`RunExperiment` are structurally inexpressible — zero
  non-comment issuers in csp-bench) and plumb a `--data-root` option through `csp-eval`.
- Author the §8.5 proofs as committed scenarios in `data/eval-scenarios/`: contract lifecycle
  (board → accept → fly → complete), colony supply loop (found/join → deliver → complete —
  enabled by Phase 3 content), agent-completes-a-mission (beyond the ascent climb), and a
  land/recover science scenario [C34-F4, C05-F6].
- Extend the contracts harness beyond SatelliteOrbit [C30-F2]: lifecycle exercises for
  rescue, survey.data, part_test, colony_resupply (the riskiest predicates: transition-keyed
  rescue, colony-delivery sums, returned-units math).
- Score boundary [C30-F4]: capture `funds_start` and `sim_time_s` before setup drains (or
  expose setup-cost fields) so the §2.12.1 score contract covers setup-phase funds/time.
- Cover `session.save`/`session.load` and `with_reconfigure` in the e2e mission test
  [C30-F3] (currently stops at `session.snapshot`; the bin's reconfigure hook is dead to
  tests).
- Colony probe over authored content [C30-F5]: drive `data/systems/home` + authored
  colonies.ron (the current probe authors a synthetic body/site/def with a 21 600 s day).

**Inputs**: Read `crates/csp-bench/src/{scenario,contracts,economy,eval,agent}.rs`,
`crates/csp-bench/tests/eval_suite.rs`, `data/eval-scenarios/`.
**Outputs**: Modify `crates/csp-bench/src/**`, `crates/csp-server/tests/e2e.rs`; create
`data/eval-scenarios/*.ron`.
**Validation Criteria**: `cargo test -p csp-bench --test eval_suite` + `cargo run -p csp-bench
--bin csp-eval -- --suite data/eval-scenarios` green; new scenarios pass; goldens unaffected
(scenarios are not goldens); workspace + clippy.

**Dependencies**: Phase 3 (content) and Phase 5.5 complete.

### 5.7 §7.7 "list what changed" + script-surface pinning [C11-F5 (M), C26-F5 (M), C26-F7 (L), C26-F8 (L), C26-F9 (N), C26-F10 (N), C09-F7 (L), C02-F3 (M), C02-F4 + C04-F5 (L), C02-F6 (L), C02-F7 (L), C08-F10 folded in 3.2]

**Requirements**:
- Data-drift listing [C11-F5]: the loader warns but does not list what changed
  (`csp-app/src/lib.rs:1176-1183` aggregate hash only). Add a per-file-kind hash breakdown
  to `csp-data/src/datahash.rs` and have the drift notice name the changed kinds (part
  stats, biome maps, balance constants) per PLAN §7.7.
- Pin the `ctx` schema [C26-F5]: extend `crates/csp-script/tests/api_freeze.rs` to pin the
  ctx map key sets (vessel/vessels keys) and document the NaN-for-missing-numeric encoding
  at its use sites (`lib.rs:740`, `:773`) plus the persist NaN fault
  (`dynamic_to_persist` `:550-565`).
- Script host nits [C26-F7-F10]: fix `IoControlApi::log`'s fabricated script id for
  autopilot callers (`lib.rs:268-271` — refuse like `request_wake` does); bound
  `LoadScript` source length on the command path (unbounded compile-in-tick + full-source
  hash/save walk); register host closures once per engine instead of per tick; add the
  vessel `id` key to `ctx.vessel`.
- Honesty pass on stale docs [C09-F7, C02-F3, C02-F4 + C04-F5, C02-F6, C02-F7, C10-F7]:
  fix the pre-M8 impact-hook comments (`colonies.rs:61-65`, `lib.rs:993-998`); rewrite the
  `SpawnVessel` opt-out to name the real path (`ManufactureCraft`/`ConstructionComplete`;
  `BuildVessel` exists nowhere); add the `// §7.4 rule 3:` pointer at `check_serializable`
  (vacuous until deferred voxelization lands — REVIEW2 contemplated it); reword the stale
  "M1 no vessel removal" arena docs; replace the `let _ = reason;` dance with
  `if self.save_blocked.is_some()`; fix reliability.rs's three stale doc claims
  (revert UI exists at M9b; StuckDecoupler/PanelJam ARE wired; mismatched-halves merge is
  refused).

**Inputs**: Read `csp-data/src/datahash.rs`, `csp-app/src/lib.rs`, `crates/csp-script/**`,
`crates/csp-sim/src/{lib,reliability, colonies}.rs`.
**Outputs**: Modify the above + tests.
**Validation Criteria**: `cargo test -p csp-data -p csp-script -p csp-sim` green;
`lint-reach` OK after opt-out rewrites; workspace + clippy.

**Dependencies**: Phase 5.6 complete.

### 5.8 Renderer gates — §5.4 depth row, star/sky regression gate, starfield waste [C16-F5 (L, PRD row MISSING), C16-F1 (M), C16-F4 (L), C16-F2 (L)]

**Requirements**:
- §5.4 depth measurement [C16-F5]: implement and record the 10⁷ m / 1 m-object depth-fight
  measurement in the perf log (PLAN:1459-1460 is normative — "Recorded in the perf log";
  the log has 17 rows, none depth).
- Render gate [C16-F1]: add a star/sky-sensitive assertion to
  `crates/csp-view/tests/render_gate.rs` (current coarse statistics pass with `star_vis = 1`
  injected; `dump_sky` is `#[ignore]`d, assertion-free, and renders a vacuum packet that
  bypasses the gated path) — e.g. a day-sky packet asserting a near-white pixel ceiling in
  the upper sky region.
- Starfield waste [C16-F4]: compute `ins`/`star_vis` before `star_field` and guard the
  27-cell walk (day-side sky fragments pay it for nothing).
- `render_flight_scene_sized` [C16-F2]: derive buffer size and copy extent consistently
  from the parameters (today size mixes param `w` with const `H`; extent uses consts —
  silent crops for non-(W,H) callers).

**Inputs**: Read `crates/csp-view/tests/render_gate.rs`, `crates/csp-render/src/terrain.rs`,
`crates/csp-render/src/terrain.wgsl` (fs_sky).
**Outputs**: Modify the above; perf-log row.
**Validation Criteria**: `CSP_RENDER_GATE_REQUIRED=1 cargo test -p csp-view` green; the new
assertion fails if the gate is broken (validated by the implementer with a scratch
injection, reverted); workspace + clippy.

**Dependencies**: Phase 5.7 complete.

### 5.9 Orbit-solver diagnostics + small spec debts [C06-F4 (L), C06-F5 (L), C03-F4 (L), C01-F4 (L), C15-F2 (L, verifier-downgraded), C15-F1 (M), C15-F3 (M), C15-F4 (L), C15-F5 (L), C32 side note]

**Requirements**:
- Log-the-elements [C06-F4]: on Kepler non-convergence, log the failing `OrbitState`
  elements at the `advance_orbits` call site (`csp-sim/src/lib.rs:7738-7761` pushes only the
  `&'static str`) — PLAN §2.8 item 4 requires it; the orbit.rs:48-52 doc claims it happens.
- `align_rotation` slop [C06-F5]: tighten the near-aligned shortcut
  (`maneuver.rs:259-261`, `cos > 0.999_999` ⇒ IDENTITY, 1.4e-3 rad / ~1.4 m/s transverse
  per 1 km/s) to ~1 − 1e-12, and note the duplicated convention in
  `csp-bench/src/scenarios.rs:309-325` (`thrust_attitude`).
- Tracked opt-in promotion [C03-F4]: either extend `class_of` so a tracked vessel's
  `DebrisDecay`/`PartFailure` promote Silent→Clamping per §2.6.5's sentence, or record the
  narrow reading next to the existing `PartFailure` rationale (events.rs:445-448).
- `StreamTag::Generation` [C01-F4]: remove the dead discriminant (zero production draws;
  removal changes no preimage — free) or mark it reserved with doc + no goldens implying a
  stream.
- Per-part `data_set_hash` [C15-F2 + C13-F5, verifier severity LOW]: stamp the real
  `csp_data::data_set_hash()` at the VAB→session boundary (`stock_build`,
  `csp-ui/src/vab.rs:1913`) and fix the false `BuiltPart::stock` doc
  ("the manufacture path fills every field"); save-level §7.7 warning already exists —
  do not duplicate it.
- `preview_attach` occupancy gate [C15-F1]: share the parent-side bound check between
  `add_part` (`session.rs:1251-1260`) and `preview_attach` (`:1377-1384`) so the pub oracle
  cannot Ok where add refuses (the doc promises "the same checks").
- `merge_graphs` typed precondition [C15-F3]: convert the release-stripped
  `debug_assert!` on `effective_base` (`merge.rs:230-241`) into a `MergeError` return
  (release builds silently alias mass properties today).
- `compact_stages` laundering [C15-F4] and O(n²) child scans [C15-F5]: reject out-of-cap
  indices in `compact_stages` rather than remapping them; extract the one-pass child-map
  helper (three in-tree precedents).

**Inputs**: Read `crates/csp-sim/src/{orbit,maneuver}.rs`, `crates/csp-sim/src/events.rs`,
`crates/csp-core/src/rng.rs`, `crates/csp-ui/src/vab.rs`, `crates/csp-phys/src/{resolve,session,merge}.rs`.
**Outputs**: Modify the above; tests.
**Validation Criteria**: `cargo test -p csp-sim -p csp-phys -p csp-ui` green; goldens
(hash-affecting: align_rotation, rng enum, BuiltPart stamp → bless); `lint-reach` unaffected;
workspace + clippy.

**Dependencies**: Phase 5.8 complete.

**Phase-level Validation**: Full gate set including the new CI tiers locally where runnable
(coverage floors file parses, bake-art --check, lint-reach unit tests).
**Dependencies**: Phase 4 complete; 5.6 additionally requires Phase 3 content.

---

## Phase 6: DRY/complexity refactors (AFTER all behavior fixes)

**Type**: Sequential; sub-phases parallel-safe by crate where noted. Each refactor must be
behavior-preserving: goldens and the full test suite must stay green WITHOUT reblessing
(hash-schema-neutral) unless the refactor intentionally walks new fields (then bless with reason).

### 6.1 Decompose `step_bubble_range` [C03-F3] (M)

- Extract from the 915-line function (`csp-sim/src/lib.rs:3987-4901`, three clippy
  suppressions): `retire_debris_vessel` (the triplicated retirement sequence at
  `:4540-4608`, `:4650-4692`, `remove_vessel` `:4997-5046`), the consequences pass, the
  propellant pass, and `detect_auto_rails_candidates` (`:4778-4829`) — each at an existing
  documenting-comment boundary. Drop the suppressions if the split gets under the budgets.

### 6.2 TickShadow declarative macro + snapshot parallel lists [C02-F5] (M)

- `csp-sim/src/lib.rs:1703-2172`: macro-generate the 26 `Option`-field capture/merge/restore
  bodies (`merge_committed` `:1881-2005`, `restore` `:2014-2172`, both under
  `#[allow]`s); the macro needs rename support (`benches` → `world.bench_runs`). Keep the
  special cases (vessels/event_heap/pending) hand-written. Reduce the 6-8-touchpoint
  parallel lists across `WorldSnapshot`/`snapshot()`/`restore()`.

### 6.3 Split `csp-data/src/validate.rs` [C32-F7] (M)

- 6 580 lines, 7 `#[allow]` sites, two functions over the §8.6 error budget. Split into a
  `validate/` module directory; `lib.rs:114-121` already re-exports everything flat, so no
  external caller changes. Add the `// lint-allow: <reason>` comments §8.6 requires for any
  remaining allows (repo-wide: zero exist today).

### 6.4 csp-phys duplication [C12-F3] (M)

- `simple.rs`: unify `external_loads` (`:1344-1479`) with `accumulate_forces`
  (`:1536-1616`) behind shared channel builders (gravity, placeholder drag, thrust sites);
  extract the twin PGS row-solve blocks (`:2015-2068` vs `:2069-2121`) and the duplicated
  `ang`/`effective` closures (`:840-850` vs `:1044-1054`). The per-part aero core already
  shares `part_force` — dedupe only the channel glue.

### 6.5 Hash-writer primitives + KDF/clippy hygiene [C01-F1, C01-F2, C01-F3, C04-F4] (M)

- Add `StateHasher::dvec3/dquat` writers (`crates/csp-core/src/hash.rs:240-478` has scalar/
  container writers only) and collapse the 16 hand-rolled vec3/quat walks in csp-sim
  (113 `h.f64(` sites; the inertia tensor keeps its own writer). Hash-stream-neutral
  requirement: the byte layout must be IDENTICAL (same order, same widths) or goldens move —
  implement as pure extraction, then verify goldens stay green without reblessing.
- Extend the four `clippy.toml` `disallowed-methods` lists by the ten platform-backed
  methods (`exp2, exp_m1, ln_1p, sinh, cosh, tanh, asinh, acosh, atanh, sin_cos`) per
  PLAN §8.2's libm-exclusive policy (C01-F2; the list is §8.2 + log2/log10 today).
- Fix the HeightField flat-stand-in key collision (`csp-view/src/terrain.rs:306-320`
  `mix64(!seed) ^ dims` collides with the real key `mix64(seed) ^ dims` exactly when seeds
  are complementary; the "can never collide" comment is mathematically false as a general
  claim; the renderer skip at `csp-render/src/terrain.rs:1232-1248` persists across body
  switches) — derive the stand-in key from a disjoint namespace (e.g. `mix64(seed) ^ !0u64`
  marker or a separate tag).
- Extract the pairing-gate idiom family (C04-F4: 7+ sites across restore) into its 2-3
  typed variants, and the heap-scrub idiom (`shadow_events_whole → mem::take → filter →
  collect`, 4+ sites in lib.rs + 3 in research/contracts).

### 6.6 Cross-crate idiom extractions [C03-F5, C07-F4, C09-F10, C10-F4, C13-F3, C19/C20/C21/C22/C24 DRY items, C29-F4, C30-F6] (M/L)

- csp-sim: `site_overlaps` iterator + two folds (`spawn_site_occupied_by`/`spawn_site_occupant`,
  `lib.rs:5888-5990`); transfer commit block ×4 (`transfers.rs:885-916/969-993/1497-1532/
  1601-1631` → `commit_transfer`); colony solve-input/crew-unhook/store-freeze duplication
  (`colonies.rs:2011-2051` vs `:2268-2331`; unhook ×3 + `land_crew`); burn-credit ×3 and
  renewal ×2 and ConfigMask remap ×2 (reliability/joints/lib); banking/fraction duplication
  with its verified divergence (`research.rs:897-915` vs `economy.rs:940-956` — unify the
  missing-def policy deliberately); science-rows triplication in `program_panel`
  (`csp-ui/src/program.rs:1787-2201`, 415 lines under two allows — extract sections sharing
  one emit channel).
- csp-view/csp-ui: Δv/burn engine model dedup (readout vs flight vs sim — three authorities
  today; after 4.6 they agree, so extract the shared consumption walk); VAB planner
  duplication (`plan_axial` vs `attach_auto`) and `emit_part`/`emit_triangles` model branch
  (NOTE the verifier caveat: they are NOT behavior-identical — raw baked albedo vs
  color-tinted; delegation needs a white-tint decision); free-seats duplication ×2 and the
  `PartFailed`/`IgnitionMisfire` text pair and the `1.0e6` vs `MICROS_PER_FUND` literals
  (`csp-view/src/lib.rs`); app refusal boilerplate ×10 (`csp-app/src/lib.rs` §2.13 sites —
  extract a `refuse_read_only(reason)` helper); `structures.rs` site-frame/spin-phase
  triplication + the `terrain.rs:418-423` fourth copy; ACES tonemap duplication across three
  WGSL files (C16-F3 — needs a shader-include mechanism or a generation step; document if
  deferred).
- csp-bench/xtask: stall predicate dedup (`replay.rs:925-929` vs `agent.rs:348-352`);
  `colony_warp_run` shared between the two measures; `found_authored_colonies` error
  laundering (`perf.rs:590-595` `|_|` → "aero-table build failed" — propagate the real
  error); steering/Rhai/pad-pose/world-wiring duplication incl. `mark_facts` reinstating
  5 of 7 `install_config` channels (C30-F6 — route through `install_config`).
- Test-suite DRY [C05-F7]: `SurfaceLatched` literal ×11 and the restore-physics block
  (23 sites, 5 factored) → shared helpers with an env parameter.
- SPA: `num()` helper unification lands in 4.11; keep.

### 6.7 Misc nits sweep [C22-F9, C18-F6 folded, C20-F4 if not done in 4.9, C31-F6 registry-loader dedup, C25 items not yet swept] (N)

- Confirm-dialog centering (`.anchor(CENTER_CENTER)` restoration or measured default_pos);
  registry-loader scaffolding dedup in `csp-data/src/load.rs` (five 20-line clones);
  `datahash.rs:211` `rel[..4]` byte-slice panic on multi-byte UTF-8 boundaries (C31 side
  observation — make it char-safe); `WIDGET_KINDS` dead export (if not already removed).

**Phase-level Validation**: `cargo test --workspace`, fmt, clippy, xtask lints, goldens
verified WITHOUT reblessing (except 6.5 if a byte-layout change is intentional), perf
spot-run (`csp-perf`) for 6.1/6.4 showing no >10% regressions.
**Dependencies**: Phases 1–5 complete.

---

## Phase 7 [PRD-GAP]: separable feature work (each independently deferrable; record §6.5 decisions if cut)

**Type**: Parallel (independent features) — each sub-phase gets its own implementer/validator loop.

### 7.1 §2.11 design assistant + MissionProfile emission [C26-F3] (H)
Greedy + local-search staging proposer over unlocked parts given payload/Δv/TWR/body;
emits the `MissionProfile` the §3.4 confidence number requires (`csp-phys/src/profile.rs`
stubs reference it). Unscheduled by §6.4; either build (recommended: into csp-ai, exposed in
the VAB) or record the cut in `docs/quarterly/` (which does not exist yet — create it per
the §6.5 ritual).

### 7.2 Planes/rovers pillar decision [C33-F2] (H)
Pillar 1 promises "rockets, planes, rovers, stations". Stations are PARTIAL-sanctioned
(dockport+lab exist; docking UI cut per §6.4 item 4). Planes/rovers are a SCHEMA-level gap
(`ModuleDef` has no Wing/Wheel/GroundDrive variant — "just author data" is impossible).
Either extend the schema + author a wing/wheel part family + aero ground contact, or record
the v1 cut decision in `docs/quarterly/`.

### 7.3 Contract kinds 5 of 7 — crew transport + tourist flight [C34-F5] (M)
Templates + predicates; crew machinery already exists (`TransferCargo::Crew`, `land_crew`,
crew location facts). Deferral is documented in code but unsanctioned by §6.4.

### 7.4 ControlApi action groups + `mission.ascend` [C26-F4] (M)
PLAN:723 specces action groups; PLAN:765 specces `mission.ascend(target_apo, incl)` (built:
`engage_autopilot(apo, pe)` — different name/semantics). The freeze test pins the current
surface — post-freeze additions are a major-version event; decide and record.

### 7.5 HTTP attach-to-a-running-game [C21-F4 remainder] (M)
Wire the frame-loop `ArcSwap` publish + ingress drain in csp-app (`backend.rs:820-825`
sketches it) so `--serve` attaches to the player's session as PLAN §2.12 states, or keep
headless-only with a recorded deviation (4.12's minimal path).

### 7.6 Thrust off the dominant body (moon insertion/return) [C06-F3] (M)
Rask is the v1 moon and inside its SOI a vessel can never thrust/schedule/latch
(`orbit.parent != physics.dominant` refusals at `lib.rs:6531/6652/6779`) — PLAN defers
"landable" only. Needs a dominant-switch (bubble re-parent) mechanism or a documented §6.5
cut. Pairs with 4.4's correction-loop route (a).

### 7.7 Lethality + repair calibration [C14-F3 remainder, C09-F6 remainder] (M)
Thermal lethality "in the right places" (M4b exit criterion) via the balance.ron η tunable
from 4.13; colony module repair command (the doc-promised post-M8-A follow-up).

**Phase-level Validation**: per-feature tests + full gate set; each 7.x may be executed or
explicitly deferred with a recorded decision — the plan is complete without them.
**Dependencies**: Phase 3 (for 7.3's content interplay), Phase 4 (for 7.5); otherwise independent.

---

## PRD coverage ledger (carried from the review's verified PRD tables)

Coarse per-area summary of the review's PRD-coverage conclusions (wired / degraded /
unreachable / missing), which this plan's phases close:

| Area | Status at review | Key gaps (findings) | Closed by |
|---|---|---|---|
| Determinism/hash (§7.1) | WIRED, one letter violation | `part_ids` serialized+unhashed [C04-F2]; no dvec3/dquat writers [C01-F1] | 6.5 |
| Save/load (§7.2/7.4/7.7) | WIRED except: rule-3 pointer, drift listing, `active_vessel` | 2 poison-save classes [C04/C11]; drift notice doesn't list [C11-F5]; `active_vessel` absent [C02-F2] | 1.2, 1.3, 5.7, 2.6 |
| Orbits/scan (§2.6.6/§2.8) | WIRED except item-4 overlay + log-elements | `scan_strides` dead [C06-F1]; elements not logged [C06-F4]; graze-miss by design (documented) | 5.1, 5.9 |
| Maneuvers/transfer | WIRED at dominant; PARTIAL timing; HIGH plan-honesty | no half-burn lead [C06-F2]; staged plan misses 20× SOI [C07-F1]; window dead-end [C07-F2]; thrust-off-dominant unsanctioned [C06-F3] | 4.4, 7.6 |
| Physics backend (§2.3/2.4) | WIRED (SimpleBackend); DEGRADED: Rapier oracle absent (sanctioned-ish), berth probe pose, aero staleness blind, thermal non-lethal, torque arm | [C12-F1, C14-F2, C14-F3, C14-F4] | 3.7, 4.13, 7.7 |
| Reliability (§3.4) | WIRED; DEGRADED: flight stress display, VAB quals, profile editing | [C10-F2/C22-F5, C10-F3, C13-F7] | 4.7, 4.8 |
| Progression (§3.2/§3.3) | WIRED core; DEGRADED: lab-model (skill 0, escrow loss), dead unlock lists | [C08-F1, C08-F7, C08-F6/C33-F3] | 3.4, 3.8 |
| Economy (§3.5) | Loop OPEN (dead-end at ~12 launches); salary out of band; insolvency hollow; founding free | [C33-F1, C34-F2, C08-F2, C08-F3, C21-F8] | 3.1, 3.4, 3.5, 3.8 |
| Contracts (§3.6) | 5/7 kinds; resupply 9/9 uncompletable + faucet; no decline; `standing` scalar missing; survey expiry | [C34-F1, C08-F5, C08-F4, C32-F1] | 3.2, 7.3 |
| Colonies (§3.5.1) | Rules 1-5 WIRED with caveats: sun-locked moons, LS-less immortality, terminal impact, dangling joins, free founding | [C09-F2/F3/F4/F6/F8/F9] | 3.6, 3.8, 7.7 |
| Vessels/flight loop | M2 flagship PARTIAL: rails focus handoff, camera reframe, plasma calibration, STG fallback, debris focus | [C19-F2/F4/F1/F6/F7] | 2.6, 4.3, 4.9 |
| VAB (M3) | Readout DEGRADED (fictional stage model, counterproductive staging); pick pass broken; variants WIRED (C17-F4 dismissed) | [C13-F1/F2, C17-F1/F2] | 2.7, 4.6, 4.7 |
| Map/planning | WIRED; MEDIUM dead-clicks/samplers/stale polyline | [C18-F1/F2/F3] | 4.5 |
| UI/HUD | Notices FIXED (complaint #5); panels PARTIAL (drag/parking); tutorial rewinds | [C23-F1/F2/F3] | 4.1, 4.2 |
| App shell | NewGame/warp/throttle HIGHs; revert integrity; serve divergences | [C24-F1/F2/F3, C21-F1, C25-F1] | 2.1–2.5, 4.12 |
| Scripting/AI (§2.10/2.11) | Ascent WIRED w/ terminal defect; ctx.persist CRITICAL; design assistant + action groups absent (unsanctioned) | [C26-F1/F2/F3/F4] | 1.1, 2.3, 5.7, 7.1, 7.4 |
| Server/MCP (§2.12) | Core auth WIRED; flags missing; quat doc wrong; zombie-on-fatal; counts-not-ids; attach DEGRADED | [C27-F1..F8] | 4.12, 7.5 |
| Telemetry SPA (§4 M9d) | Dashboard CRITICAL (vessels.0); program widgets absent; zero tests; token-leak shape | [C28-F1..F9] | 1.5, 4.11, 5.2 |
| Data & validators (§3.1/§8.2) | Loaders solid; version band sites-only; ID-stability + recipe-graph + colony cross-fields missing; cap u64::MAX | [C31, C32] | 3.1, 3.3, 5.4 |
| Bench/CI/tooling (§5.3/§8.2-8.5) | Per-commit core WIRED; 8 §8.2 rows MISSING + 4 PARTIAL; 2 perf rows red untriaged; eval tier can't express §8.5 proofs; bake staleness undetectable | [C29, C30, C34-F4, C35] | 5.2, 5.3, 5.5, 5.6 |
| Content (parts/recipes/contracts) | Ore uncarriable, composite unproducible, Rask mis-modeled, planes/rovers schema-impossible | [C33, C34] | 3.1, 3.6, 7.2 |

## Known-good (explicitly NOT broken — do not churn)

Verified-clean areas; implementers must not "fix" these without new evidence:

- Solver math: Kepler round-trip ≤1e-6 across e ∈ {0…3.0} × Δt 1e-3…1e9 s; Stumpff/Laguerre
  constants spec-literal; `state_at` bitwise-pure [C06].
- PGS determinism: fixed orders, BTreeMap-only discipline, no HashMap in csp-phys, ground
  K / two-body `ang` identity, settle-latch hysteresis [C12].
- Warp anchor fix `6cc2cb4`: all seven accumulator-anchor paths verified COMPLETE
  (construction, every demote, rebase on world swaps, fresh controller, behind-clock
  self-heal, fatal skip) [C25].
- Sky/star gate (complaint #3): thresholds, gate placement, sparse starfield, jitter — CPU
  model independently reproduced [C16].
- Sky fix and alarm book: alarm arm→fire→toast→demote chain, graft/eviction, DismissAlarm
  index stability, planner honesty refusals [C18].
- Attach invariants: adversarial move/reattach/promotion/reroot sequences hold; craft-file
  byte-canonical round-trip; M6 merge bit-identity + full per-arena remap
  (`merge_weld_is_bit_identical_to_from_scratch_weld`) [C15, C10].
- Merge angular-momentum algebra: conserved to |dL| = 0 in an independent probe
  (frame-consistent; `bp.rot` confirmed) [C10].
- KDF stream keys and hash-rule implementation (§7.1 rules 1-8; 3-field StreamKey at every
  draw; MODE_COUNTER_BIT disjoint) [C01, C10].
- Data-set-hash coverage: every file under `data/` hashed or sanctioned-excluded (54 hashed,
  13 test-fixture, 2 art); injective length-prefix framing [C31].
- Server auth core: constant-time token compare, traversal-proof asset gate, Origin/Host
  allowlists, lockfile discipline [C27].
- Goldens green: both registered artifacts verify exit 0 at HEAD (trivial, flight-launch);
  `every_registered_scenario_has_a_verified_golden` [C29].
- Save-level §7.7 data-drift warning exists and fires (`DATA DRIFT` notice) — only the
  listing and per-part halves are gaps [C13-F5 correction, C11-F5].
- egui pointer semantics: movable HUD panels suppress scene picks correctly (C19-F3
  dismissed — do not add masks for click-semantic picks outside VAB press events).
- Variants are wired with real content (both engines ship stock + qualified) — C17-F4
  dismissed; not a content gap.
- Manufacture atomicity, life-support opt-in disable scrubbing, §2.6.5 constants
  (2000/1024), §7.2 container codec, v2 byte-exact save gate [C03, C11].
- SimTime no-underflow at ×10⁵ warp; auto-rails floor oscillation-free (Keplerian periapsis
  invariance) [C01, C06].
- HeightField shared physics/render implementation and pad authored terrain-aware
  (`launch_site_sits_on_rendered_ground` green) [C12, C20].

## Success Criteria

- All executed phases complete with per-sub-phase and phase-wide validation green.
- Full gate set green at every phase boundary: `cargo fmt --all -- --check`;
  `cargo clippy --workspace --all-targets -- -D warnings`; `cargo test --workspace`
  (with `CSP_RENDER_GATE_REQUIRED=1` where render tests run); `cargo run -p xtask --
  lint-deps` / `lint-attribution` / `lint-reach`; every golden in `csp-replay list`
  verifies (or was reblessed with `HASH_SCHEMA_VERSION` bump + reason); `cargo test -p
  csp-server --test e2e`; `cargo test -p csp-bench --test eval_suite`; `npx tsc --noEmit`
  in `apps/telemetry`.
- No requirement from Phases 1–6 left unimplemented; Phase 7 items either implemented or
  recorded as §6.5 decisions in `docs/quarterly/`.
- The PRD coverage ledger's gaps are closed or explicitly carried as recorded deferrals.
