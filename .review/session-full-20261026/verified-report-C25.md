# Verified report C25 — verification of review-report-C25.md

Verifier re-read every cited file:line in the current working tree, traced callers,
re-derived the Finding 2 arithmetic from first principles, and ran `cargo test -p
csp-app --lib` (51 passed; 0 failed — matches the report's claim).

Verdict summary: **4 confirmed, 0 dismissed.** The 6cc2cb4 accumulator-anchor
COMPLETE verdict is upheld. Clean-bill spot checks (4 of 7 areas) all held.

---

### Finding 1: New Game silently drops the M10 "max time warp" settings cap — CONFIRMED

**Original**: `SessionOp::NewGame` replaces the controller wholesale, resetting
`cap_idx` to the ladder top; only `apply_live_settings()` re-applies the cap, and it
never runs after NewGame.

**Verification**:
- `/home/didi/workspace/Clanker-Space-Program/crates/csp-app/src/lib.rs:1089` —
  `self.warp = WarpController::new();` and `WarpController::new()` hard-codes
  `cap_idx: WARP_LADDER.len() - 1` (warp.rs:135) = index 6 = ×100 000.
- `set_cap_multiplier` has exactly ONE production call site in the whole workspace:
  lib.rs:557, inside `apply_live_settings` (lib.rs:542). Its only two callers are
  `App::new` (lib.rs:529) and the settings panel's Apply (lib.rs:1510). No mode
  switch, load, or other path re-applies it.
- `apply_startup_difficulty()` (lib.rs:603–616, called at lib.rs:1090 right after
  the controller swap) touches only `SetLifeSupportEnabled` and
  `set_reliability_config` — no warp contact.
- `do_load` (lib.rs:1153–1209) does NOT replace the controller (only sets
  `world_rebuilt` → `warp.rebase()` at lib.rs:983), so the cap survives Load;
  NewGame is the sole cap-dropping verb. The only other `WarpController::new()` is
  app construction (lib.rs:511), followed by `apply_live_settings` at 529.
- Consequence chain verified: with cap_idx back at 6, `decide`'s fold
  (warp.rs:195–199) clamps `requested` only to the ladder top, so `requested`
  climbs to ×10⁵; `capped_below_ladder_top()` (warp.rs:383–385) returns false, so
  the dead-press notice at lib.rs:1413–1418 says "LADDER TOP", not "SETTINGS
  LIMIT". The UI end of the setting is real: `csp-ui/src/settings.rs:797–802`
  ("max time warp" combo → `gameplay.max_warp_multiplier`, default 100 000 at
  settings.rs:181).
- The suggested fix is sound: on a fresh controller (`effective == 0`),
  `set_cap_multiplier` routes through `demote(cap_idx)` which hits the
  `rung >= self.effective` early return (warp.rs:318–320) → `None` → no spurious
  `SetWarp` is logged, and NewGame sets `read_only = false` so the read-only
  refusal branch in `apply_live_settings` (lib.rs:559–567) cannot fire either.

Confirmed MEDIUM: the M10 cap contract is violated mid-session by a Game-menu
verb; no sim-corruption (all rungs §2.6.2-legal), so MEDIUM is the right severity.

---

### Finding 2: partial-advance demotion drops two rungs, not one — CONFIRMED

**Original**: `demote(4)` contradicts its own comment's rule ("largest rung one
call can reach"), which arithmetically yields rung 5; the comment also mislabels
rung 4 as ×10⁴.

**Verification** (arithmetic re-derived from the constants, not trusted):
- `TICK_HZ = 64` (csp-core/src/time.rs:9). `MAX_TICKS_PER_FRAME = 69 120`
  (csp-sim/src/lib.rs:214; = 1080 sim-s per call at 64 Hz).
- Rule "largest rung one call can reach": ticks/frame = 64×m/60 ≤ 69 120
  ⇒ m ≤ 69 120 × 60/64 = **64 800**.
- Ladder (warp.rs:33): `[1, 2, 10, 100, 1_000, 10_000, 100_000]`.
  - rung 4 (×10³): 64×1 000/60 = 1 066.7 ticks/frame — comfortably inside.
  - rung 5 (×10⁴): 64×10 000/60 = **10 666.7 ≤ 69 120** — reachable in ONE call.
  - rung 6 (×10⁵): 106 666.7 > 69 120 — the only rung exceeding the budget.
- So the largest reachable rung is **5 (×10⁴)**. The code at lib.rs:1324 does
  `self.warp.demote(4)` → effective = index 4 → ×10³. The comment at
  lib.rs:1319–1321 says "⇒ rung 4, ×10⁴" — doubly wrong: rung 4 IS ×10³, and the
  rung the stated rule selects is 5. Self-contradiction confirmed verbatim.
- Impact as reported: a ×10⁵ player hitting `report.partial` (§2.6.5 back-pressure,
  lib.rs:1323–1333) drops ×100 (to ×1 000) where the stated rule drops ×10 (to
  ×10 000); re-promotion then waits the full `PROMOTE_FRAMES = 30` margined window
  (warp.rs:44–46, 217–221). Over-demotion only — ×1 000 is strictly safe in the
  other direction, so MEDIUM (playability + a comment that misdocuments the
  constant it names) is correct.

---

### Finding 3: bug-report log tail can carry the user's absolute home-dir path — CONFIRMED

**Original**: settings-io `log::warn!` lines embed `path.display()`; logring
captures info-and-above into the 500-line tail; bugreport ships it verbatim as
`log.txt`. Token leakage verified absent.

**Verification**:
- `/home/didi/workspace/Clanker-Space-Program/crates/csp-app/src/settings_io.rs:92–95`
  — `log::warn!("settings: cannot read {}: {e} …", path.display())`;
  settings_io.rs:112–115 — the salvage warn, same pattern. `path` is
  `config_dir()/settings.ron` (settings_io.rs:35–57) = `$XDG_CONFIG_HOME` or
  `$HOME/.config` on Linux ⇒ `/home/<user>/...` — the home-dir disclosure is real.
- logring.rs:75–87 — `TeeLog::log` pushes every record with
  `level() <= TAIL_LEVEL (Info)` into the ring; warn qualifies. Ring capped at
  `TAIL_LINES = 500` (logring.rs:13) with oldest-first eviction (logring.rs:52–61).
- bugreport.rs:176 — `write("log.txt", (input.log_tail.join("\n") + "\n")…)`,
  verbatim, no redaction. `collect_bug_report` fills `log_tail: logring::tail()`
  (lib.rs:684); the panic path ships the same tail (bugreport.rs:187–202).
  PLAN.md:933 confirms §2.14's "last 500 log lines" content contract.
- No-token conclusion upheld: the `--serve` bearer token is generated/stored in
  csp-server (serve.rs:93–155, http.rs:351–387); grep over ALL log/print sites in
  csp-app and csp-server finds no statement that formats the token value — the
  only stdout/stderr mention is serve.rs:194–202, an `eprintln!` naming the token
  FILE path (not the token), and stderr is not captured by the ring. Also noted:
  `--serve` mode never runs the frame loop that produces bundles.
- The leak surface is in fact slightly BROADER than filed (does not weaken the
  finding): lib.rs:635 `log::error!("bug-report bundle: {}", dir.display())` and
  the `BundleError` Display (bugreport.rs:87) also embed home-dir paths into
  ring-captured lines, so a later bundle in the same session can carry them too.
  Severity NIT stands: user-voluntary artifact, PRD requires no redaction.

---

### Finding 4: engine rumble keeps playing after a Fatal halt — CONFIRMED

**Original**: the per-frame `backend.update(...)` runs unconditionally after
`end_frame`, outside the `fatal.is_none()` gate; the frozen snapshot holds the
last throttle > 0, so the continuous engine voice never decays.

**Verification**:
- lib.rs:904–929 — the entire observe/decide/`target_tick`/`AdvanceTo` block is
  gated on `self.fatal.is_none()`; `sim_step` runs only `if let Some(target)` —
  the sim is frozen post-halt. But lib.rs:1012–1027: `self.view.end_frame()` and
  the audio `backend.update(...)` block run UNCONDITIONALLY every frame, and the
  render loop never stops (lib.rs:841–858 re-requests redraw each frame).
- `on_sim_fatal` (lib.rs:624–645) touches no audio state; grep over lib.rs shows
  the only audio references are `open` (832), `update` (1019–1027), `ui_confirm`
  (1369) — nothing gates or zeroes the engine voice on fatal.
- audio.rs:563–577 — `engine_target`/`plasma_target` derive from
  `snap.vessels` of the focused vessel's `throttle`; the smoother
  (audio.rs:578–588) only moves toward that target. Post-halt the target is
  constant.
- One mechanical nuance (does not change the outcome): `end_frame` RE-STAGES the
  FlightSnapshot every frame (csp-view/src/lib.rs:2236–2241,
  `flight::capture_sync_input(&self.sim, …)`), so the snapshot is not a stale
  staged struct — it is re-derived from a sim that no longer steps (rolled back to
  the pre-tick shadow). Either way the throttle value never changes, so a focused
  vessel that was under thrust at the halt holds its rumble indefinitely while
  the session is read-only and the clock stopped. Cosmetic; NIT is correct.

---

## Deep-verify (c): the 6cc2cb4 accumulator-anchor COMPLETE verdict — UPHELD

Every anchor path in the report was re-traced in source:
1. **Armed at construction** — warp.rs:125–137, `needs_rebase: true`.
2. **Every downward `decide`** — warp.rs:230–237 (`if next < self.effective {
   self.needs_base = true }` — auto-derate AND manual step-down).
3. **All `demote` paths** — warp.rs:311–324 sets `needs_rebase = true` on every
   real demotion: Clamping/rails refusal `demote(0)` (lib.rs:1307–1316), partial
   `demote(4)` (lib.rs:1323–1333), settings cap `set_cap_multiplier` →
   `demote(cap_idx)` (warp.rs:345–352).
4. **`rebase()` on world swaps at the drained boundary** — lib.rs:975–987:
   `run_session_op()` then `if self.world_rebuilt { self.warp.rebase(); }`;
   `world_rebuilt` is set by exactly Revert (lib.rs:1077), NewGame (1086) and
   Load (1192) — all three world-replacing verbs covered, no other set sites
   (grep over lib.rs).
5. **Fresh controller on NewGame** — lib.rs:1089, which re-arms via (1).
6. **Behind-clock self-heal** — warp.rs:272–281:
   `if self.needs_rebase || self.scaled_nanos < anchor` re-anchors at the live
   clock — covers sim-side forward clock jumps the flag cannot see.
7. **Fatal skip** — lib.rs:904–929: while `fatal.is_some()`, no `target_tick`
   call, no `AdvanceTo`, no `sim_step` — nothing accrues; and `request_session_op`
   (lib.rs:1050–1057) refuses Load/Revert/NewGame while read-only, so no un-rebased
   world swap can occur mid-halt. Recovery from a halt is NewGame only → path (5).
Ordering is also correct: per frame the rebase flag set at the drained boundary is
consumed by the NEXT frame's `target_tick` before its `AdvanceTo` is enqueued.
Regression tests exist and pass: `target_tick_anchors_to_the_live_clock`
(warp.rs:572), `step_down_rebases_the_target` (729), `demote_rebases_the_target`
(771), `cap_multiplier_clamps_requests_and_demotes` (610). COMPLETE verdict stands.

## Clean-bill spot checks (4 areas checked, all held)

- **Settings IO** (report: strict→salvage→defaults, NaN rejection, atomic write):
  settings_io.rs:85–136 (ladder incl. salvage with notes), normalize()
  settings_io.rs:154–175 rejects non-finite `master_volume`/`ui_scale`;
  save_settings_to writes sibling `.tmp` + `rename` with failed-rename cleanup
  (settings_io.rs:405–421); degenerate `window_size` floors at 1 px
  (lib.rs:696–699). Held.
- **Logring** (500-cap, oldest-first eviction, dual-level gating): logring.rs:13,
  52–61, 30–42 (process `max_level` raised to admit the ring floor under a quiet
  RUST_LOG), 68–87 (console keeps env filter, ring keeps Info floor). Held.
- **Audio subprocess lifecycle** (argv constant, Drop teardown, reap): `PLAYERS`
  const + `player_args` produce only compile-time constants and format consts —
  no user-controlled argv, no injection surface (audio.rs:193–239, spawn at
  417–418); `Drop` = stop→kill→join→wait (audio.rs:604–617); per-frame
  `try_wait` reaps a dead player and latches `alive = false` (audio.rs:530–543).
  Held.
- **Press-preview parity / no dead keys**: `request_preview` (warp.rs:376–379)
  folds `pending_step` and clamps to `cap_idx` exactly like `decide`'s fold
  (warp.rs:195–199); `warp_step_request` (lib.rs:1398–1447) lands a reason for
  read-only, ceiling (naming the cap via `capped_below_ladder_top`), rails
  refusal, and above-budget presses; step-downs always engage. Held. (Test
  `warp_press_always_says_why` passes.)

## Empirical checks

- `cargo test -p csp-app --lib` → **51 passed, 0 failed** (matches the report).

## Final verdict

All 4 findings CONFIRMED at their filed severities (2 MEDIUM, 2 NIT); none
dismissed. No false positives found in review-report-C25.md; the clean-bill
section survived every spot check, and the 6cc2cb4 COMPLETE verdict is upheld
with all seven anchor paths re-traced.
