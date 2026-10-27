# Verified Report C24 — App shell (winit loop, input, UI outbox)
Verification pass over `review-report-C24.md` against the working tree at HEAD 7dd4b86
(csp-app lib.rs uncommitted-diff-free; 4800 lines, non-test code L1–3313).

**Result: 7 confirmed / 0 dismissed.** Baseline re-checked: `cargo test -p csp-app --lib`
→ 51 passed, 0 failed (matches the report's claim). The three HIGH findings all survive
deep verification; one (Finding 1) was re-reproduced empirically by this pass with an
out-of-tree test (no repo files modified).

---

### Finding 1: New Game boots an empty world — CONFIRMED
**Original**: `SessionOp::NewGame` calls `view.new_game()`, which on native replaces the
whole View with `View::new()` — no starter seed; nothing re-seeds; player lands in the
"purple void" with no flight focus and an empty VAB session.

**Verification**: Every link traced, then reproduced empirically.
- `crates/csp-app/src/lib.rs:1084-1094` — the NewGame branch calls `self.view.new_game()`,
  then only: `world_rebuilt = true`, `fatal = None`, `read_only = false`,
  `warp = WarpController::new()`, `apply_startup_difficulty()` (a `SetLifeSupportEnabled`
  enqueue + reliability-config write — no vessel spawn), and a notice. No seed.
- `crates/csp-view/src/lib.rs:3132-3146` — `View::new_game()` on native does
  `*self = if self.demo { Self::new_demo()? } else { Self::new()? }`. `new_demo()` seeds
  internally (lib.rs:1937); `View::new()` → `from_source` (lib.rs:1736-1915) seeds nothing
  (colonies yes, vessels no).
- The seed lives in exactly one production place: `App::new_inner`
  (`crates/csp-app/src/lib.rs:406-414`), guarded by `!view.is_demo()`. Repo-wide grep:
  `seed_starter_craft`/`seed_pad_craft` production callers are that line and
  `View::new_demo` only. No save-slot-0 bootstrap, no view-init reseed exists — the
  reviewer's "nothing re-seeds afterwards" was checked and is correct.
- `seed_pad_craft` (`csp-view/src/demo.rs:224`) also builds the craft in the VAB session,
  so the fresh View's editor is blank too — the impact claim holds for both halves.
- **Empirical reproduction (this pass)**: out-of-tree package depending on the public
  API: `View::new()` → `seed_starter_craft()` → begin/step(320)/end → `flight_focus()`
  Some ✓ → `new_game()` → begin/step(320)/end → `flight_focus()` **None** — assertion on
  `is_none()` passed. Exactly the reviewer's repro.
- Test-coverage claim accurate: `new_game_boots_with_a_pad_vessel` (lib.rs:3493) drives
  `App::new_with_settings_path` (constructor path) and never runs the session op.

### Finding 2: Physics warp ×2 unreachable under any rails-warp refusal — CONFIRMED
**Original**: the app applies `rails_warp_refusal()` (a *rails*-warp predicate) to the
whole ladder above ×1: the press gate refuses ×1→×2 too, and the per-frame heartbeat
demotes to rung 0; the atmospheric ascent can never use ×2.

**Verification**: PLAN quote verified verbatim; both code paths verified.
- `PLAN.md:464-468` (§2.6.2 regime table):
  > | **Real time** | ×1 | 64 ticks/s | always |
  > | **Physics warp** | ×2 | 128 ticks/s — *more CPU work, same dt* | **bubble occupied; demoted by the controller** |
  > | **Rails warp** | ×10 … ×10⁵ | no bubble ticks; analytic jump | no bubble-resident vessel under thrust, unsettled contact, or in atmosphere |

  The thrust/contact/atmosphere set is the **rails row's** allowed-when; the ×2 row's
  allowed-when is "bubble occupied" — precisely the situations the refusal describes.
  "Demoted by the controller" is §2.6.3's budget derate, not the rails gate.
- Press gate `crates/csp-app/src/lib.rs:1425-1434`: `if let Some(reason) = self.view.sim.rails_warp_refusal() { notice; return; }` — fires for every `step > 0` with no
  rung discrimination, so the ×1→×2 press is refused during ascent.
- Heartbeat `lib.rs:1307-1316`: `if (clamping || refusal.is_some()) && let Some(level) = self.warp.demote(0)` — `demote(0)` pins `effective` to rung 0 every frame the
  refusal holds (warp.rs:311-324 drops `effective` unconditionally), so even a ×2
  legitimately engaged in vacuum coast is force-demoted to ×1 the moment the vessel dips
  into atmosphere. Both halves of the impact claim verified.
- `rails_warp_refusal` (`csp-sim/src/lib.rs:3707-3755`) confirmed to report exactly the
  rails conditions for InBubble vessels (thrust > 0, unsettled contact, altitude < atm
  top) — during an atmospheric ascent (with or without thrust) it always holds.
- Test-gap claim accurate: `warp_press_always_says_why` (lib.rs:4127-4198) presses
  WarpUp to ×2 while *latched* (refusal vacuous) before throttling up; under refusal,
  `requested` is already rung 1, so every refused press targets ≥ ×10. The ×1→×2
  press under refusal is never exercised.
- The suggested fix (cap at rung 1 under rails refusal, keep `demote(0)` for fired
  `Clamping`) is consistent with the PLAN table; §2.6.3's budget derate is independent
  and still applies underneath.

### Finding 3: Stale wall-side throttle axis survives Load/NewGame — CONFIRMED
**Original**: `throttle_axis`/`throttle_axis_vessel`/`throttle_hold_vessel` are never
reset on world rebuilds; vessel ids are deterministic from 0, so a loaded save's focused
vessel can carry the same id as the pre-load axis vessel, defeating the rebase; the
first Shift tap then unlatches at the stale (full) throttle via the delta-0 path.

**Verification**: every premise independently verified.
- No resets: repo-wide assignment audit — outside tests, the axis fields are written only
  in `throttle_dispatch`/`throttle_press`/`throttle_release`/`tick_throttle_ramp`/
  `throttle_axis_step`/`drop_rebound_holds`/`focus_lost` (window focus loss, not world
  rebuild). `run_session_op` (lib.rs:1060-1096), `do_load` (1153-1209), and the Revert
  branch (1075-1083) touch none of them.
- Id determinism: `csp-sim/src/lib.rs:2504` — `next_vessel_id: 0` in `with_config`, the
  constructor every fresh world routes through; the starter craft is id 0 in every world.
- Id preservation across load: `rebuild_from_snapshot` (csp-view lib.rs:1574-1602) →
  `SimWorld::restore` (csp-sim lib.rs:2926+) restores the vessel arena from the snapshot
  (it validates `next_vessel_id` is ahead of the snapshot's live ids), so the loaded
  vessel keeps its save-time id. Focus after load resolves to it (`FlightFocusOverride::
  resolve` → `focus_vessel` = `vessels.last()`, and `FlightUiData.focus`'s doc confirms
  latched pad vessels are staged and focusable).
- Rebase defeat: `throttle_press` (lib.rs:2238-2243) rebases only when
  `throttle_axis_vessel != flight_focus()`; with the id collision the condition is false,
  and `applied` is `None` (latched — `body()` gone), so the stale axis value survives —
  the P02-F2 keep-rule (test `latched_focus_keeps_the_throttle_axis`, lib.rs:4204).
- Unlatch at stale value: `throttle_axis_step` (lib.rs:2358-2371) — delta 0 at the rail,
  focus `SurfaceLatched`, `!sim.throttle_queued(vessel)` (provably true: the load ran at
  the drained boundary and the rebuilt world's queue is empty), `next > 0` → falls
  through and enqueues `SetThrottle(1.0)`. This exact mechanism is pinned as *intended*
  same-world behavior by the shipped test `shift_press_unlatches_the_relanded_craft`
  (lib.rs:4571-4610, asserts `body.throttle == 1.0` after one Shift press with
  `throttle_axis = 1.0, throttle_axis_vessel == focus`) — b698eb2's fix. After a Load the
  same code path fires on a value from the previous world: **full-throttle pad ignition
  from a 1% tap**. Trigger chain (Z → land/latch → F5 → F9 → Shift) verified step by
  step; save-while-latched is legal per PLAN §2.6.4.
- The class history claim is accurate: dd3612a and b698eb2 verified via `git show`
  (both are stale-axis bugs in this exact state machine).

### Finding 4: "WARP LIMITED — EVENT/TICK BACKLOG" posts every frame while `partial` holds — CONFIRMED
**Original**: no dedup latch on the partial-backlog notice (unlike the refusal's
`last_refusal`); sustained backlog floods the cue stack and the overlay.

**Verification**: `crates/csp-app/src/lib.rs:1323-1333` — the `flight_notice(...)` call
sits **outside** the `if let Some(level) = self.warp.demote(4)`, so it fires on every
frame `report.partial` is true (the demote itself emits once). `partial` is
`clock.ticks() < effective_target` (csp-sim lib.rs:8736) — true every frame of a
sustained backlog. `View::flight_notice` (csp-view lib.rs:3311-3319) pushes onto
`FlightSfx.cues` with `SFX_TTL_FRAMES = 90` (flight.rs:590) and TTL-only eviction
(flight.rs:1948-1952, no cap); `notice_overlay` (csp-ui lib.rs:702-729) renders **every**
live cue as a stacked chip (`for line in lines`, line 715). Steady state ≈ 90 identical
chips at 60 fps. The rails-refusal path's `last_refusal` latch (lib.rs:1300-1306) is the
in-tree precedent the finding says is missing here. Confirmed as a real flood under the
designed back-pressure conditions; MEDIUM severity appropriate.

### Finding 5: `--serve` does not share the game's saves dir — CONFIRMED
**Original**: `serve_cli`'s doc claims the hosted session shares the game's `saves/`
dir, but the host writes to `config_dir()/saves` (absolute) while the app reads/writes
`<cwd>/saves` (relative); the menu can't see host saves.

**Verification**: 
- `crates/csp-app/src/lib.rs:2969-2970` — the doc string makes exactly the claimed
  promise; lib.rs:2974 — `let save_dir = settings_io::config_dir().join("saves")`.
- `settings_io.rs:35-51` — `config_dir()` = `$XDG_CONFIG_HOME/csp` (absolute; APP_DIR
  joined at line 50).
- App side: lib.rs:364 — production `craft_dir = PathBuf::from("crafts")`;
  lib.rs:490-496 — `save_dir = craft_dir.parent().join("saves")` = `"crafts".parent()`
  (`Some("")`) `.join("saves")` = **relative `saves/`** under the CWD. No
  `set_current_dir` anywhere in csp-app.
- `stage_save_list` (lib.rs:1219) scans only `self.save_dir`; `GameHost` writes session
  saves to the injected dir (`csp-server/src/backend.rs:462`,
  `self.save_dir.join(format!("{name}.{SAVE_EXT}"))`). The two coincide only if the game
  is launched with CWD == `$XDG_CONFIG_HOME/csp`, which nothing does. Divergence and the
  false doc claim both confirmed.

### Finding 6: `stage_save_list` slurps entire save bodies — CONFIRMED
**Original**: the "O(header) per save" comment is wrong — `std::fs::read` loads the full
file before `inspect_save`, which needs only the framing + JSON header.

**Verification**: lib.rs:1232-1234 — `std::fs::read(&path).ok().and_then(|b|
csp_view::save::inspect_save(&b).ok())` reads the whole file into memory.
`inspect_save` (csp-sim/src/save.rs:449-470) → `parse_header` (473-489) consumes only
`CSPSAVE\0 || len:u32 || JSON` and returns the body slice unread — so the app-side full
read is pure waste (and the comment at lib.rs:1212 misleading). Bounded frequency
(startup + after each save, per file), so LOW is the right severity; the suggested
bounded-prefix read is viable since the header length is in the first 4 bytes.

### Finding 7 (DRY/CRAP): refusal boilerplate ×10 + ownerless throttle-axis state machine — CONFIRMED
**Original**: §2.13 read-only refusals hand-written at ten call sites with drifting
wording; the throttle axis is five struct fields with invariants across three
interlocking methods — three shipped bugs in the class.

**Verification**: All ten sites present at (or within a line of) the cited ranges, each
with different tail wording: lib.rs:567 ("WARP DEMOTION NOT APPLIED"), 1053 (combined
read-only/demo "saves are off…"), 1404 ("WARP NOT APPLIED"), 1554 ("LAUNCH REFUSED"),
1569 ("MANEUVER NOT APPLIED"), 1604 ("ACTION NOT APPLIED"), 2059 ("ATTITUDE NOT
APPLIED"), 2142 ("THROTTLE NOT APPLIED"), 2392 ("STAGING NOT APPLIED"), 2705
("RECOVER-CONTROL NOT APPLIED"). The five axis fields sit at lib.rs:246-280; the
invariants span `throttle_press` (2209), `tick_throttle_ramp` (2280), and
`throttle_axis_step` (2358). Bug-class history: dd3612a + b698eb2 (verified via git) +
Finding 3 (confirmed above) = three. Maintainability finding, factually accurate;
LOW/NIT-tier severity appropriate.

---

## DEGRADED rows and complaint #4 verdict — VERIFIED
- **Complaint #4 ("warp refuses silently", exploration-map.md §3)**: the *silence* is
  fixed in the tree. `warp_step_request` (lib.rs:1398-1447) lands a reason on every
  dead-press path: read-only ("SESSION IS READ-ONLY — WARP NOT APPLIED"), ceiling
  ("WARP ALREADY AT ×N — SETTINGS LIMIT/LADDER TOP"), refusal ("WARP REFUSED —
  <offender>", every press), budget ("WARP LIMITED — ×N EXCEEDS THE STEP-TIME
  BUDGET"); the post-step side latches the persistent "RAILS WARP ×1 — <reason>" on
  text change (lib.rs:1295-1306). Test `warp_press_always_says_why` pins every-press
  narration (asserts 2 cues for 2 presses under a held refusal). The remaining defect
  is indeed *what* is refused (Finding 2), not silence — the row's
  "WIRED (feedback) / DEGRADED (×2)" split is correct.
- **§2.6.5 row** (Clamping + partial demotion wired; spam latch missing): consistent
  with Finding 4's confirmation.
- **§7/§7.4 row** ("WIRED except NewGame"): drained-boundary application verified
  (lib.rs:966-1011 — session op applied inside the post-`sim_step` success arm at the
  queue-drained moment; world-replacing ops discard the stale report);
  `safe_slot_name` (3183-3191) and the atomic tmp+rename write (1135-1142) as cited;
  NewGame is the exception per Finding 1.

## Verdict on the original report
All 7 findings confirmed at their stated severities (3 HIGH, 2 MEDIUM, 2 LOW). No false
positives found; no scope corrections needed. The report is accurate in its code
citations, test-gap claims, PLAN quotations, and empirical claims (Finding 1's repro
replicated independently by this pass).
