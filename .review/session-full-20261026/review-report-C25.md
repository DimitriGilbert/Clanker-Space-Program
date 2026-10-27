# Review report C25 — App periphery: warp controller, audio, settings IO, bugreport, logring

Scope: `crates/csp-app/src/{warp,audio,settings_io,synth,bugreport,logring,main}.rs` against the
current working tree (HEAD 7dd4b86 + W2.6 uncommitted). Cross-read `csp-app/src/lib.rs` (C24's
file) only where the warp controller's contract depends on the caller. PRD: PLAN §2.6–2.7, §2.12,
§2.14, §3.7, M10. All 51 `csp-app` unit tests pass (`cargo test -p csp-app --lib`).

---

### [SEVERITY: MEDIUM] Finding 1: New Game silently drops the M10 "max time warp" settings cap

**File**: crates/csp-app/src/lib.rs:1089 (state lives in crates/csp-app/src/warp.rs:115,125-137)
**Problem**: `SessionOp::NewGame` replaces the controller wholesale — `self.warp = WarpController::new()`
— which resets `cap_idx` to `WARP_LADDER.len() - 1` (×10⁵). The cap is only re-applied by
`apply_live_settings()` (the sole caller of `WarpController::set_cap_multiplier`), which runs at
startup (lib.rs:529) and on the settings panel's Apply (lib.rs:1510) — never after NewGame.
`apply_startup_difficulty()` (lib.rs:1090) touches only life support + reliability.
**Evidence**: player sets "max time warp ×1 000" (csp-ui/src/settings.rs:797-802, persisted via
settings_io), then starts a New Game: `requested` can now climb to ×10⁵ through the ladder;
`capped_below_ladder_top()` returns false, so dead presses at the top are mis-attributed to
"LADDER TOP" instead of "SETTINGS LIMIT" (lib.rs:1414-1418). The cap silently re-imposes itself
only at next app start.
**Impact**: the M10 settings contract ("a QoL cap on `requested`") is violated mid-session after a
Game-menu verb; no sim corruption (all rungs remain §2.6.2-legal), so MEDIUM not HIGH.
**Suggestion**: after `self.warp = WarpController::new();` call `self.apply_live_settings();` —
on a fresh controller `set_cap_multiplier` demotes nothing (`demote(cap_idx)` returns `None` at
effective 0), so no spurious `SetWarp` is logged. Alternatively give `WarpController::new()` a
cap parameter.

### [SEVERITY: MEDIUM] Finding 2: partial-advance demotion drops two rungs, not one — off-by-one vs its own documented rule

**File**: crates/csp-app/src/lib.rs:1319-1324 (rule referenced from warp.rs's ladder)
**Problem**: on `report.partial` (§2.6.5 back-pressure) the controller demotes to rung 4 (×1 000).
The comment states the rule "largest rung one call can reach (TICK_HZ×mult/60 ≤ MAX_TICKS_PER_FRAME
⇒ rung 4, ×10⁴)" — which is internally inconsistent (rung 4 is ×10³) and arithmetically wrong:
MAX_TICKS_PER_FRAME = 69 120 (csp-sim/src/lib.rs:214), so ticks/frame = 64×m/60 ≤ 69 120 ⇒
m ≤ 64 800 ⇒ the largest reachable rung is **5 (×10⁴, 10 667 ticks/frame)**; only rung 6 (×10⁵,
106 667) exceeds it (verified by computation).
**Evidence**: a player at ×10⁵ hitting an event/tick backlog is demoted ×100 (to ×1 000) when the
stated rule demotes ×10 (to ×10 000), then must wait the full 30-frame §2.6.3 hysteresis to climb
back. Warp collapses an order of magnitude further than designed.
**Impact**: over-demotion on the back-pressure path — playability (warp throughput) plus a comment
that misdocuments the constant it names. No correctness hazard in the other direction (×1 000 is
strictly safe).
**Suggestion**: `self.warp.demote(5)` + fix the comment ("⇒ rung 5, ×10⁴"), or, if the extra rung
of margin is intentional, say so explicitly in the comment and keep `demote(4)`.

### [SEVERITY: NIT] Finding 3: bug-report log tail can carry the user's absolute home-dir path

**File**: crates/csp-app/src/settings_io.rs:92-95,112-115 → crates/csp-app/src/logring.rs:79-86 → crates/csp-app/src/bugreport.rs:176
**Problem**: `load_settings_from`/salvage `log::warn!` lines embed `path.display()` (e.g.
`/home/<user>/.config/csp/settings.ron`); logring captures info-and-above into the 500-line tail;
`write_bundle_dir` ships the tail verbatim as `log.txt`.
**Evidence**: PLAN §2.14 explicitly defines the bundle as "save + command log + data-set hash +
determinism fingerprint + last 500 log lines" — shipping the save itself is sanctioned, and I
verified the `--serve` bearer token is **never** logged (grep over csp-app/csp-server log sites),
so this is only username/home-dir disclosure in an artifact the user voluntarily sends.
**Impact**: minor privacy leak in a user-shared diagnostic; PRD does not require redaction, hence NIT.
**Suggestion**: if desired, strip `$HOME` (or the config-dir prefix) in `TeeLog::log` before the
ring push — one line, and the §2.14 contents are unchanged.

### [SEVERITY: NIT] Finding 4: engine rumble keeps playing after a Fatal halt

**File**: crates/csp-app/src/lib.rs:1018-1027
**Problem**: the per-frame `backend.update(...)` runs unconditionally after `end_frame`, outside
the `fatal.is_none()` gate that stops the sim. After `on_sim_fatal` the staged `FlightSnapshot` is
frozen with the last throttle > 0, so the continuous engine voice holds its level indefinitely
while the session is read-only and the clock is stopped.
**Evidence**: `update`'s voice target is read from `snap.vessels` each frame (audio.rs:563-577);
nothing zeroes `engine_level` on `fatal`.
**Impact**: cosmetic — a halted session with a stuck engine rumble until quit.
**Suggestion**: pass `self.fatal` (or a zero target) into `update` on the fatal path, or gate the
call on `self.fatal.is_none()` like the sim step.

---

## Verified-clean areas (no findings after tracing)

- **Accumulator anchoring (the 6cc2cb4 fix) is complete**: armed at construction (warp.rs:134),
  on every downward `decide` (warp.rs:231-237), on `demote` (Clamping/rails refusal/partial,
  warp.rs:322), on the settings-cap demote via `demote` (warp.rs:351), on world swaps via
  `rebase()` at the drained boundary (lib.rs:976-983 — Load and Revert), and fresh-controller on
  NewGame; behind-clock self-heal covers sim-side clock jumps (warp.rs:274-281). Pause needs no
  anchor: no pause exists, and a Fatal halt skips `target_tick` entirely so nothing accrues
  (lib.rs:904-929).
- **§2.6.3 inequality**: correct per-frame form (`per_tick_ms × TICK_HZ×k/60 × margin < 0.5 ×
  16.67 ms`), 1.5×/30-frame hysteresis bound to a single candidate rung, demotion immediate;
  `observe` input is div-by-zero-safe (`ticks_last_frame().max(1)`, lib.rs:1007) and NaN-folds to
  0.0 (`f64::max` semantics). All three committed perf-log reference scenes reach the rungs the
  test names (tests pass).
- **Preview/press parity** (`request_preview`/`multiplier_at`/`promotable_ceiling` vs actual
  engagement): the preview folds `pending_step` exactly like `decide` incl. the settings clamp;
  every press that cannot take effect lands a reason (read-only, cap/ladder-top naming the cap,
  rails refusal verbatim, budget-limited) — no dead keys found.
- **Refusal heartbeat**: the every-frame `demote(0)` clears the promotion streak (warp.rs:313-317),
  so a held refusal cannot be promoted through (`persistent_refusal_blocks_reprojection` passes).
- **Audio subprocess lifecycle**: argv is compile-time constant (no injection surface), `Drop` =
  stop→kill→join→reap, `try_wait` reaps exited children each frame (no zombies), settle check
  falls through dead servers, writer thread is lock-disciplined and pipe-occupancy-paced
  (MAX_PIPE_LEAD_S), one-shots capped at 16, NaN throttle/flux guarded. Cue multiset matching is
  sound against the actual `FlightSfx` decay (uniform TTL decrement; `flight_present` runs in
  every mode, so cues always age).
- **Settings IO**: strict→salvage→defaults parse ladder with surfaced notes; tmp+rename atomic
  write; NaN rejection for `master_volume`/`ui_scale`; degenerate `window_size` floors at 1 px
  (lib.rs:696-699); char→positional migration and backfill are wired (lib.rs:448,455).
- **Synth**: deterministic, bounded, bank/slot indexing consistent with `OneShot::ALL`.
- **Logring**: 500-line cap per §2.14, correct oldest-first eviction, dual-level gating correct.

## PRD coverage table

| Requirement (PLAN) | Status | Evidence |
|---|---|---|
| §2.6.2 ladder ×1 / ×2 / ×10…×10⁵, ×2 v1 cap, ×10⁵ ceiling | **WIRED** | warp.rs:33 `WARP_LADDER`; no ×3/×4 rungs exist |
| §2.6.2 physics warp ×2 "bubble occupied; demoted by controller" | **DEGRADED** — ×2 unreachable whenever a rails refusal holds (ascent in atmosphere): post-step `demote(0)` (lib.rs:1308) and press-time refusal gate ALL step-ups (lib.rs:1430-1434) — **owned by C24's review, not re-derived here** | lib.rs:1307-1316, 1425-1434 |
| §2.6.2/2.6.5 refusal names the offending vessel | **WIRED** | `rails_warp_refusal` reason → "RAILS WARP ×1 — …" notice + "WARP REFUSED — …" press cue (lib.rs:1300-1305, 1430-1433) |
| §2.6.3 controller entirely in csp-app; every decision a logged command | **WIRED** | warp.rs; SetWarp on decide/demote/cap, AdvanceTo per frame (lib.rs:906-925, 1315, 1326, 569) |
| §2.6.3 demotion inequality + 1.5×/30-frame hysteresis | **WIRED** | warp.rs:167-179, 194-240; per-frame units correct (pre-fix 60× error gone) |
| §2.6.5 Clamping auto-demote | **WIRED** | lib.rs:1257-1260 → `demote(0)` |
| §2.6.5 back-pressure: `partial` demotes warp | **WIRED (off-by-one rung)** | lib.rs:1323-1332 — Finding 2 |
| §2.6.3 warp-target accumulator anchored to the clock (6cc2cb4) | **WIRED — fix complete** | warp.rs:272-289 + tests `target_tick_anchors_to_the_live_clock`, `step_down/demote_rebases_the_target` |
| M10 settings + input rebinding incl. "max time warp" | **WIRED (cap lost on NewGame)** | settings_io.rs + csp-ui settings.rs:797; Finding 1 |
| M10 audio (real sound output, §3.7 license-clean) | **WIRED** | audio.rs subprocess synth bank; zero audio assets → zero attribution rows (synth.rs) |
| §2.14 bug-report bundle (save+log+fingerprint, --bug-report, Fatal, panic) | **WIRED** | bugreport.rs; lib.rs:633, 2954, 3068; slug always constant → no path traversal |
| §2.14 last 500 log lines | **WIRED** | logring.rs:13 |

## Top 3 issues

1. [MEDIUM] New Game resets `WarpController` and loses the settings "max time warp" cap until
   restart (lib.rs:1089).
2. [MEDIUM] Partial-advance demotion targets rung 4 (×10³) where the documented "largest rung one
   call can reach" rule yields rung 5 (×10⁴) — over-demotion by one rung + wrong comment
   (lib.rs:1319-1324).
3. [NIT] Bug-report `log.txt` can leak the user's home-dir path via settings-io warn lines; token
   leakage verified absent (settings_io.rs:92-115).
