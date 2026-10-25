# Review Report — P02: Flight control verbs & read-only gates

Scope: `crates/csp-app/src/lib.rs` (throttle axis, attitude held/deferred, dispatch,
focus_lost, read-only gates, `handle_ui_outbox`, `on_sim_fatal`,
`apply_live_settings`/`apply_startup_difficulty`), `crates/csp-view/src/lib.rs`
(`flight_notice`, `UiOutbox`, `launch_craft`, `recover_control`, `fire_stage`,
`PLAYER_SOURCE_ID`), `crates/csp-view/src/flight.rs` (`FlightSfx`, `sfx_edges`,
`StagePlan`), `crates/csp-sim/src/lib.rs` (`enqueue`, `drain_at`, `apply_command`
SetThrottle/SetAttitude, `unlatched_body`), `crates/csp-app/src/warp.rs`.

Verified clean (no finding): PLAYER_SOURCE_ID FIFO sharing is real — `World::enqueue`
keys `pending` on `(SourceClass, u32)` and `drain_at` pops per-source FIFO front
(`csp-sim/src/lib.rs:3141-3149`, `6449-6457`), so SetThrottle/SetAttitude/JettisonPart/
ToBubble/TakeControl/SpawnPhysicalVessel all drain in issue order. Every app-side
command producer reachable while halted is `read_only`-gated (audited all 11
`enqueue_command` call sites + `fire_stage`/`recover_control`/`launch_craft` — no
undrained-queue leak found). Attitude deferral replays correctly on `tick_attitude`
(the held-through-unlatch path is test-pinned). `focus_lost` issues the zero-rate
hold via `apply_attitude`. `fire_stage`'s latched/rails/dead-vessel refusal notices
are all present and don't consume the stage. `drop_rebound_holds` + swallowed-release
paths clear held sets correctly.

---

## Findings

### F1 — MEDIUM: `flight_notice` is invisible outside Flight mode — `RecoverControl` (a Map-mode verb) and Map-mode throttle refusals die silently

- **Files:** `crates/csp-view/src/lib.rs:383-420` (`flight_hud` gated by `mode == Flight`),
  `crates/csp-view/src/lib.rs:1500-1525` (`recover_control` pushes refusal cues),
  `crates/csp-app/src/lib.rs:1816-1823` (RecoverControl reachable in Map),
  `crates/csp-view/src/flight.rs:1273-1278` (TTL ages every frame, every mode),
  `crates/csp-app/src/lib.rs:1037-1041, 1054-1055` (outbox refusals claim "fires in any mode").
- **Problem:** `flight_notice` pushes onto `FlightSfx.cues`, which is rendered only by
  `flight_hud` — and `build_ui` calls `flight_hud` only when `mode == Flight`
  (`csp-view/src/lib.rs:383`). But `flight_present`/`sfx_edges` run unconditionally
  every Presentation pass (`csp-view/src/lib.rs:1040-1049`, `flight.rs:1113`), so a cue
  pushed while the player is in Map or VAB ages out (~90 frames) without ever being
  drawn. `RecoverControl` is *explicitly* a map verb — it reads `MapInput.focus_vessel`
  when `mode() == Map` (`csp-view/src/lib.rs:1501-1505`) and `dispatch` allows it in Map
  (`csp-app/src/lib.rs:1816` `mode != ViewMode::Vab`). Its refusal cues — "VESSEL NOT
  RECOVERABLE — NOT ON RAILS HERE" (1511), "NO VESSEL TO RECOVER" (1523), and the
  read-only refusal (`csp-app/src/lib.rs:1819`) — are the only feedback the press can
  produce, and in Map mode they are never rendered. The same applies to the read-only
  throttle notices, which are reachable in Map because the throttle guards only exclude
  VAB (`csp-app/src/lib.rs:1769, 1804`), and to the outbox launch/maneuver refusals
  (`csp-app/src/lib.rs:1041, 1055`) which fire *specifically* from VAB/Map — the comment
  at 1037-1039 says "flight_notice, not the VAB row: a halted session can be sitting in
  any mode," but the flight cue stack is only visible in Flight (the always-visible
  `crash_dialog` at `csp-view/src/lib.rs:649-651` partially mitigates the halt cases).
- **Evidence:** map overlay receives no sfx input (`map_panels`, `csp-view/src/lib.rs:463-510`);
  VAB refusals use the separate `VabEvent::Notice` row — so in Map mode there is *no*
  refusal channel at all.
- **Impact:** In Map mode, pressing the recover key on a non-eligible vessel produces a
  truly silent no-op — exactly the "control does nothing silently" class this cluster
  was written against.
- **Suggestion:** Either draw the `FlightSfx` cue stack in every mode (it is
  session/UI-level, not sim-coupled), or give `flight_notice` a mode-aware sibling that
  routes to the map overlay / `VabEvent::Notice`. At minimum, `recover_control`'s
  refusal paths should also land a map-visible cue.

### F2 — MEDIUM: Throttle verbs have no residence/no-vessel refusal cue — a press on a rails focus silently logs a reject (attitude got `attitude_blocker`; throttle didn't)

- **Files:** `crates/csp-app/src/lib.rs:1516-1546` (`throttle_press` has only the
  read_only gate — no `attitude_blocker` analog), `1595-1612` (`throttle_axis_step`
  returns silently when `flight_focus` is `None`), `1769-1799` (`ThrottleFull`/`Cut`
  `if let Some(vessel)` — silent on `None`), `1579-1586` (the ramp *does* know about
  rails and suppresses per-frame spam — asymmetric with the press edge);
  `crates/csp-sim/src/lib.rs:3321-3325` (`unlatched_body` rejects OnRails /
  unknown vessel as `Recoverable`); `crates/csp-app/src/lib.rs:836-842`
  (`report.rejected` → `log::warn!` only).
- **Problem:** Pressing any throttle verb while the focused vessel is on rails
  (or with no vessel at all) is a dead key: `ThrottleUp`/`Down` enqueue a `SetThrottle`
  tap that the drain rejects `"control: vessel on rails"` — the rejection goes to
  `AdvanceReport::rejected`, which the app only `log::warn!`s; no HUD cue.
  `ThrottleFull`/`Cut` are the same. Worse, the press *rebases the wall-side axis*:
  `throttle_held.len() == 1` re-reads `v.body()` which is `None` for a rails (or
  latched) vessel, so `throttle_axis` silently resets to `0.0` (`csp-app/src/lib.rs:1534-1539`)
  even though the on-rails vessel's stored throttle may be nonzero — the axis and the
  sim diverge with no signal. `tick_throttle_ramp` already suppresses per-frame
  rails rejects (the "don't feed the drain a reject every frame" comment, 1577-1586),
  so the authors knew the residence class exists — but the press edge still enqueues
  one doomed command per tap and says nothing.
- **Evidence:** `attitude_blocker` (`csp-app/src/lib.rs:1449-1464`) names all three
  refusal classes for the same residence checks; the throttle path covers only
  read_only.
- **Impact:** "Throttle does nothing silently" — the headline bug class — survives on
  the throttle verb family exactly where `attitude` was fixed. (Note: `SurfaceLatched`
  must NOT be a throttle blocker — `SetThrottle` is the unlatch command,
  `csp-sim/src/lib.rs:3326+`.)
- **Suggestion:** Add a `throttle_blocker()` mirroring `attitude_blocker`
  (read_only / no-vessel / OnRails → notices; SurfaceLatched → `None`). Also gate the
  tap enqueue on the focus being throttle-commandable, so no reject is issued, and
  don't rebase `throttle_axis` to 0 from a non-`body()` focus.

### F3 — LOW: Mode-gated verbs die silently (no cue at all)

- **Files:** `crates/csp-app/src/lib.rs:1809` (`Stage if mode == Flight`),
  `1810-1812` (`CycleFocus if mode == Map`), `1816` (`RecoverControl` inert in VAB),
  `1769/1804` (throttle verbs inert in VAB), `1844-1860` (Roll inert outside Flight).
- **Problem:** A `Stage` press in Map mode is simply dropped by the match guard —
  no notice, no cue of any kind. Same for `CycleFocus` outside Map and the throttle
  verbs in the VAB. These are "verb unreachable in this mode" refusals — a distinct
  class from the residence refusals, but still silent.
- **Evidence:** `_ => {}` fallthrough (1862) and the `if mode ==` guards produce no
  output; the only cue channel (`flight_notice`) can't render outside Flight anyway
  (F1).
- **Impact:** Space-on-the-map (a plausible mid-flight input) produces nothing;
  players can't tell a mode gate from a dead key. Low because mode-restriction is
  arguably the right *behavior* — only the silence is wrong.
- **Suggestion:** Either emit a short mode-refusal cue (needs F1's mode-agnostic
  channel first) or accept as documented. `Stage` in Map is the highest-value case.

### F4 — LOW: `throttle_press` re-reads the *applied* throttle, ignoring queued-but-undrained commands on the same FIFO

- **Files:** `crates/csp-app/src/lib.rs:1530-1539`.
- **Problem:** A ramp start (`throttle_held.len() == 1`) re-initializes `throttle_axis`
  from `body.throttle` — the *applied* value. Commands queued on PLAYER_SOURCE_ID this
  frame haven't drained yet (drain happens inside the next `sim_step`), so the axis
  rebases to a stale base. Concrete sequence within one drain window (~one frame, longer
  at low fps): press `ThrottleFull` (axis=1.0, `SetThrottle(1.0)` queued) → press
  `ThrottleUp` → axis re-reads applied 0.0 → tap steps to 0.05 → queued `SetThrottle(0.05)`
  lands *after* the 1.0 → final state is 5%, not 100%.
- **Impact:** A snap press can be partially eaten by a ramp-start inside the same
  drain window; also means rapid release/re-press within one frame re-bases from the
  pre-tap applied value (tap-not-cumulative). Bounded to one drain boundary; low.
- **Suggestion:** Track the last *commanded* value (the axis itself already is one —
  only re-read from sim when no command for this focus is in flight), or skip the
  re-read if the last command on the FIFO is newer than the applied mirror.

### F5 — LOW: Held-axis rail suppression + sim-side dry-engine clamp = a held throttle never recovers after propellant returns

- **Files:** `crates/csp-app/src/lib.rs:1595-1599` (axis-at-rail → `throttle_axis_step`
  early-returns, nothing enqueued), `crates/csp-sim/src/lib.rs:4488-4498`
  (`SetThrottle` clamps `body.throttle` to 0 when `!engine_can_draw`).
- **Problem:** Hold ThrottleUp to the rail (axis=1.0, applied=1.0) → propellant
  starves → the sim clamps applied to 0 on the next SetThrottle (or the debit path
  zeros it) → fuel later returns (transfer/pool refill) → the held key emits nothing
  because `throttle_axis` is already at 1.0 and the step is a no-op. The sim keeps
  throttle 0 forever while the HUD shows a held key and the axis insists it's 1.0 —
  the player must release and re-press.
- **Impact:** A held throttle silently goes dead across a starvation/refuel edge —
  the "persisted command != applied state" divergence the axis was designed to
  tolerate becomes permanent without a re-press. Narrow trigger; low.
- **Suggestion:** In `tick_throttle_ramp`, re-issue the SetThrottle when the focused
  body's *applied* throttle diverges from `throttle_axis` (bounded: divergence
  detected once per frame, re-send only on mismatch — a once-per-edge command, not
  a spam).

### F6 — LOW: Warp keys/outbox `warp_step` in a halted session are fully dead — no cue, no queue

- **Files:** `crates/csp-app/src/lib.rs:1765-1766` (`WarpUp`/`WarpDown` →
  `warp.request_step`), `935-937` (outbox warp_step → same), `757-782`
  (`decide()`/`target_tick` skipped entirely while `fatal.is_some()`).
- **Problem:** `request_step` only mutates wall-side `pending_step` (`warp.rs:123-125`);
  with `decide` never called, `requested` never moves — so the press changes *nothing*
  observable (even the "requested" mirror stays put). Positively: nothing queues behind
  the dead clock — the halt-side discipline is correct; what's missing is the cue.
- **Impact:** Minor — the always-visible crash dialog (`csp-ui/src/lib.rs:139-163`)
  gives ambient context. Flag for completeness of the read-only coverage audit.
- **Suggestion:** Cheap `flight_notice` on warp presses while `read_only` (mode-visible
  caveat per F1 — the crash dialog is already every-mode, so this may be acceptable).

### F7 — NIT: Successful stage fire produces two overlapping cues

- **Files:** `crates/csp-view/src/lib.rs:1857` ("STAGED — N PART(S) JETTISONED" push),
  `crates/csp-view/src/flight.rs:1241-1243` (`sfx_edges` `nodes` shrink →
  "STAGE — PART JETTISONED").
- **Problem:** One stage press flashes two different texts in the same stack — the
  app-level confirm and the snapshot edge detector both fire.
- **Impact:** Cosmetic noise only; pick one source of truth for the stage cue.

### F8 — NIT: `fire_stage`'s pop-then-reject race — a residence change between the press-time check and the next drain eats the stage with a misleading "STAGED" cue

- **Files:** `crates/csp-view/src/lib.rs:1817-1841` (residence checked at press),
  `1844-1856` (pop + enqueue).
- **Problem:** The pop consumes the stage the moment the bound vessel checks InBubble;
  if the vessel converts to rails between that check and the drain (auto-ToRails in
  the same frame's `step_frame`), all `JettisonPart`s reject `Recoverable` →
  `log::warn` only — while the "STAGED — N PART(S) JETTISONED" cue already flashed and
  the firing is permanently spent.
- **Impact:** ~1-frame window; when it hits, the player sees a success cue and the
  stage is silently gone.
- **Suggestion:** If a jettison batch fully rejects, restore the stage to the plan
  (needs the rejected-command surface to reach the app — `AdvanceReport::rejected`
  carries the `CommandKey`, which carries source_seq but not the command text;
  alternatively re-check at drain time by keeping the stage until the next frame's
  staged `nodes` count confirms the drop — a staged-mirror confirmation loop rather
  than optimistic consumption).

---

## Audit summary vs. the brief

- **Every verb a dead key can reach → visible notice:** read_only gates all have cue
  text; BUT `flight_notice` doesn't render outside Flight (F1) and the throttle verbs
  have no residence gate at all (F2); mode-gated verbs have no cue anywhere (F3).
- **Commands queued behind a dead clock:** none found — every producer path is
  read_only-gated, and `decide()`/`sim_step` skipping while halted is airtight.
- **Throttle axis semantics:** persist-across-release ✓, rebase-on-snap ✓,
  hold-across-rails ✓ (ramp suppressed, axis retained — but press-time axis squash to
  0 on a rails focus is part of F2).
- **PLAYER_SOURCE_ID FIFO:** issue-order verified (`enqueue` → per-source `push_back`,
  `drain_at` → `pop_front`); all six player-command producers share `(Ui, 1)`
  correctly; warp/maneuver/settings on 0/2/3 as documented.
- **Attitude deferral:** correct — held-through-unlatch steering works and is
  test-pinned (`pad_attitude_press_notices_and_defers_until_liftoff`).
- **focus_lost zero-rate:** verified (`apply_attitude` enqueues zero `ang_vel`;
  read_only early-clear).
- **Outbox arms:** all 7 handled; read_only covered on launch/maneuver/craft_save;
  `craft_load` and `settings_apply` correctly remain allowed (session/file-read and
  settings-only respectively); `warp_step` is dead-but-harmless while halted (F6).
