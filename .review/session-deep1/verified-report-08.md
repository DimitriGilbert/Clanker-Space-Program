# Verified Report 08 — Cluster C08 (determinism: event order + hash + script budgets)

Verification of `/home/didi/workspace/Clanker-Space-Program/.review/session-deep1/review-report-08.md`
against actual source. Every cited line was read; verdicts below.

**Result: 11/11 findings CONFIRMED.** Two findings carry small reachability nuances
(noted inline) that narrow but do not dismiss them.

---

### [HIGH] Finding 1: `pending` command queue is not covered by `TickShadow` — CONFIRMED

Verified:

- `TickShadow` (lib.rs:1481–1573) has **no field for commands pushed during the
  iteration**. `restore` (1618–1715) only re-queues `drained` via `push_front`
  (1624–1626) and restores `seq_restore` counters (1627–1636). Nothing truncates
  `pending`.
- `WorldTickIo::issue` (7367–7372) does `self.pending.entry(...).or_default().push_back(command)`
  — a direct, unrecorded write.
- Both io construction sites hand it `pending: &mut self.pending`:
  `run_script_tick` (6145–6162, field at 6152) and `run_autopilot_tick`
  (6253–6270, field at 6260). `control_tick` (3247) runs them per integer tick
  inside `step_bubble_range`; `fire_script_wake` → `run_script_tick` can also
  issue during `fire_due_events`.
- `Fatal`-returning code runs later in the same iteration than an `issue` can:
  `batch_joint_defs` `?` (3322), `external_loads`/`joint_loads` `map_err Fatal`
  (3360, 3383), `apply_surface_impacts` `?` (3705), `finish_burns` Fatals,
  `advance_interval_subsystems` `?` (6823–6831), `schedule_event`'s `tick <= clock`
  Fatal (3918–3919), `fire_due_events` Fatal propagation (6841–6849).
  Commands issued at the last in-range tick (`t_next − 1`), or by wake-fired
  scripts, are never drained before such a `Fatal` — they survive `restore`.
- `pending` is deliberately unhashed: excluded from `WorldSnapshot`
  (doc 1229–1236), refused by `check_serializable` while non-empty (2803–2807),
  and absent from the `state_hash_masked` walk (6898+ — the walk goes
  `source_seqs` → … → `tracked_vessels` → `next_event_seq` → heap, never
  `pending`). The leak is invisible to the canonical fingerprint, as claimed.
- The `step_frame` doc (6745–6748) does claim a `Fatal` "restores clock, warp,
  log, queue, counters, arena, opt-in set, heap, and `event_seq`" — the "queue"
  claim holds only for drained commands.

Minor observation (does not change the verdict): restored FIFO ordering stays
coincidentally correct (`drained` commands `push_front`, issued commands sit at
the back). The defect is the ghost commands' *existence*, not their order.

### [HIGH] Finding 2: `tracked_vessels` mutated without shadow capture — CONFIRMED

Verified both sites:

- **lib.rs:3696–3700** — the post-loop `dead_vessels` cleanup runs
  `self.tracked_vessels.remove(&vid)` with only `self.shadow_autopilots(shadow)`
  at 3695 before it. `shadow_tracked` exists (3033–3037) and is used by
  `remove_vessel` (3806) — but never called here. Note `shadow_vessels` (captured
  at 3211) does **not** cover `tracked_vessels` (it captures vessels,
  next_vessel_id, joints, joint_refused, next_part_instance_id — 3017–3030).
- **joints.rs:1063–1065** — `apply_latch` (signature 780–792 takes
  `tracked: &mut BTreeSet<VesselId>` and `shadow: &mut TickShadow`) mutates
  `tracked` with no capture. Grep confirms the only shadow capture inside
  `apply_latch` is `research::shadow_science` at 967; `latch_pass` (1123–1146)
  passes both parameters through but never captures tracked itself.
- `tracked_vessels` is hashed state (6951–6954), and `Fatal`-returning paths
  follow both mutation sites in-range (`apply_surface_impacts` 3705,
  `contract_notify` 3711, `finish_burns` 3713, then
  `advance_interval_subsystems`/`fire_due_events`). Post-`Fatal` `state_hash`
  diverges from the pre-boundary hash — the §2.13 violation the report states.

### [HIGH] Finding 3: mid-interval-published events fire at `clock == t_next` — CONFIRMED

Verified the mechanism end to end:

- `advance_interval_subsystems` (6822–6828) runs while `self.clock == now` —
  `clock.try_advance` happens at 6836, `fire_due_events` at 6841.
- All four lazy publishes compute absolute ticks from `clock == now` and can
  land inside `(now, t_next]`:
  - `publish_economy` — `next = (clock/period + 1) * period` (economy.rs:691–692)
  - `publish_life_support` — same grid form (life_support.rs:283–284)
  - `publish_colonies` — `next_boundary(colony, &light, now)` → repair
    `ColonyInspection` at `needed` (colonies.rs:2172–2185)
  - `publish_reliability` — `tick.max(clock + 1)` (reliability.rs:868, 883–891)
- `fire_due_events` pops `e.tick <= t_next` (6416) after the clock has advanced —
  so such an event fires with `clock == t_next > event.tick`. In a finer
  partition the same event fires with `clock == event.tick`.
- Clock-relative (partition-dependent) firing writes, all verified:
  - `fire_life_support_check` schedules `VesselLifeSupportDepleted` at
    `self.clock.ticks() + 1` (life_support.rs:332) while its own re-arm correctly
    uses `event_tick + period` (343–345) — the contrast the report flags is real.
    The crew-loss transition lands at different ticks across partitions →
    trajectory divergence, not just hash cosmetic.
  - `charge_funds` schedules `Insolvent` at `clock + 1` (economy.rs:645–651) →
    hashed `ScheduledEvent` tick differs; and in the coarse partition the
    `Insolvent` remains pending at the frame end where the fine partition already
    consumed it — heap contents diverge.
  - `fire_colony_event` discards its tick (`_event_tick`, colonies.rs:1840),
    recomputes at `now = self.clock.ticks()` (1862); `recompute_colony` re-anchors
    `s.anchor_tick = tick` (2030) — `anchor_tick` **is hashed** (1155) — and
    schedules `PowerLost`/`PowerRestored`/`LifeSupportDepleted` at `tick + 1`
    (2120, 2132, 2138).
  - `finish_burns`' degraded `SoiRescan` republish uses `self.clock.ticks()`
    (3777–3781) where clock was restored to `now` at 3692 — while
    `fire_due_events`' identical fallback uses `self.clock.ticks() == t_next`
    (6515–6521). Two different anchors for the same mechanism, as claimed.
- Correct-pattern firings confirmed as contrasts: `fire_part_failure`
  `tick.min(clock)` (reliability.rs:934), `fire_contract_deadline`
  `deadline_tick != Some(event_tick)` (contracts.rs:918), `fire_salary_due`
  `event_tick + period` (economy.rs:727–729), `fire_maneuver_start`
  `SimTime(tick)` (lib.rs:5211).
- Nuance (strengthens, not weakens): `fire_colony_event`'s comment (1855–1861)
  *deliberately* re-solves at the clock and claims "the clock is an absolute
  tick, so the publish is identical in every §2.6.1 partition." That reasoning is
  valid only for the deferred-flush corner (`event.tick` at/behind the clock due
  to `MAX_EVENTS_PER_BATCH`); the mid-interval-publish case makes the clock value
  at firing time partition-dependent — the comment's partition claim is wrong for
  exactly the case Finding 3 identifies.

### [MEDIUM] Finding 4: `Recoverable` escapes `step_frame` via `?` chains — CONFIRMED

Verified all three propagation paths:

- `transfers::reweld_if_mass_changed(v, true)?` (lib.rs:3541) → `rebody` maps
  `csp_phys::weld` failure to `Recoverable` (transfers.rs:~620,
  `map_err(|e| SimError::Recoverable(e.reason()))`).
- `self.tech.credit_qualification(family, burn_s * rate, 0.0, 0)?`
  (lib.rs:3533) → `Recoverable` on non-finite/negative (tech.rs:135–148).
- `self.apply_surface_impacts(&impacts, shadow)?` (lib.rs:3705) →
  `apply_damage_module` `?` (colonies.rs:1800) → `recompute_colony`
  (colonies.rs:1730) → `Recoverable` when `colony_env` lacks `colony.body`
  (2006–2009). The pre-filter (1775–1778) guards `body != dominant`,
  `Dormant`/`Abandoned`, and module index — but **not** `colony_env` presence.

`step_frame` propagates any `Err` from `step_bubble_range` after
`shadow.restore` (6818–6821), contradicting "`step_frame` only ever returns `Err`
for `Fatal`" (1204).

Reachability nuance: `apply_found_colony` refuses founding on a body with no
`colony_env` (colonies.rs:1450–1454), and `colony_env` is construction config —
never serialized/hashed, re-installed via `set_colony_env` post-restore
(lib.rs:1894–1895, 2761–2763). So the colony arm needs a restored/reconfigured
world whose env map no longer covers a live colony's body — narrower than "any
live colony on `dominant`" but still a real, unguarded path. The reweld arm
(3541) needs no such setup.

### [MEDIUM] Finding 5: wake-fired scripts on `InBubble` vessels double-tick — CONFIRMED

- `fire_script_wake`'s `live` check (6372–6375) is `s.enabled && s.wake_tick ==
  Some(tick)` — no residence check, verified.
- `control_tick` (6097–6106) ticks every enabled script bound to an `InBubble`
  vessel once per integer tick. `resolve_wake` accepts `AtTick`/`InTicks`
  regardless of residence (7166–7179), so an `InBubble`-bound script can hold a
  live wake. Double `on_tick` at the wake tick is real; the scripts.rs doc
  (39–43) frames wake as the tick for vessels *without* bubble ticks, matching
  the report's "oversight, not contract" reading. (Intent is a judgment call —
  the mechanism is confirmed.)

### [MEDIUM] Finding 6: wake-issued commands stamp at `t_next`, not `t_next + 1` — CONFIRMED

- `ScriptWake` fires inside `fire_due_events` after `try_advance` → io's
  `clock == t_next`. Issued commands sit in `pending` and drain at the *next*
  iteration's `drain_boundary` → `drain_at(self.clock.ticks())` (5885–5886) where
  the clock is still `t_next` → `CommandKey.tick = t_next`, the same tick value
  the wake's `on_tick` ran at.
- Control-path commands issued at tick `t` drain at `drain_at(t + 1)` — the
  documented one-tick latency (scripts.rs:34–35). The asymmetry is confirmed:
  a wake-fired script observes state at `t_next` then lands a command stamped
  `t_next`, which the bubble path cannot do.

### [LOW] Finding 7: `request_wake` bypasses `schedule_event`'s ahead-of-clock invariant — CONFIRMED

- `request_wake` pushes `ScheduledEvent` directly (7397–7402) — never calls
  `schedule_event`, so the `tick <= clock → Fatal` check (3918–3919) is skipped.
- `resolve_wake` `AtNextSoiTransition` returns the min pending `SoiTransition`
  tick (7201–7210); `AtManeuverNode` returns a `Scheduled` node's tick
  (7212–7217). Both can be `≤ clock` in the deferred-flush corner (leftover
  batch-tail events can even be `< clock` — more than `MAX_EVENTS_PER_BATCH`
  events due across ticks `≤ t_next` leave sub-`clock` ticks in the heap, and a
  deferred `ManeuverNodeStart` leaves its node `Scheduled` at `node.tick ≤ clock`).
- Confirmed: a non-tail cause can produce `tick < clock` heap entries, outside
  the documented "tail ⇒ `tick == clock`" invariant (6395–6399).

### [LOW] Finding 8: `script_log` not rolled back on `Fatal` — CONFIRMED

- `TickShadow` has no `script_log` field/capture; `command_log` does get one
  (`log_len`, 1486/1621). Lines pushed during a rolled-back iteration
  (`log_line` 7421–7423; strike/disable notices 6182–6195; poll notice
  6212–6219) persist in `self.script_log` and surface in the next successful
  report via `mem::take` (6869). Confirmed.

### [LOW] Finding 9: `steer`/`steer_dir` degenerate-input parity gap — CONFIRMED

- `IoControlApi::steer_dir` (csp-script/src/lib.rs:150–157): zero direction →
  `SetAttitude` with the vessel's **current telemetry rotation**.
- Rhai `steer` (csp-script/src/lib.rs:714–733): `steer(0,0,0)` → `SetAttitude`
  with `DQuat::IDENTITY` (718–724).
- The autopilot side constructs `IoControlApi` (csp-ai/src/lib.rs:198–204), so
  the divergence is between the guest binding and the `ControlApi` impl — as
  reported.

### [LOW] Finding 10: `BudgetExceeded` classified by error-string matching — CONFIRMED

- csp-script/src/lib.rs:973–986 — `msg.contains("Terminated") ||
  msg.to_lowercase().contains("too many operations")` decides
  `ScriptFault::BudgetExceeded` vs `Runtime`. A guest `throw` containing either
  substring misclassifies; consequences bounded (a strike either way; feeds the
  `heavy`/poll detector at lib.rs:6206–6224). Confirmed.

### [LOW] Finding 11: hygiene items — CONFIRMED

- (a) `self.shadow_autopilots(shadow)` at lib.rs:3695 runs unconditionally at the
  end of every non-trivial `step_bubble_range`, cloning the whole autopilot arena
  even when `dead_vessels` is empty. The comment "Lazy capture — no-op when
  `dead_vessels` is empty" (3693–3694) is inaccurate: the capture *is* the cost —
  confirmed.
- (b) `source_id` collapse: `script.id.0.min(u64::from(u32::MAX)) as u32`
  (csp-script/src/lib.rs:1030) and `vessel.0.min(u64::from(u32::MAX)) as u32`
  (csp-ai/src/lib.rs:201) — all ids > u32::MAX share one `(class, source_id)`
  FIFO/`source_seq` channel. Confirmed (requires >4 G ids — absurd scale, as
  stated).

---

## Spot-check of the report's "verified clean" notes

- `fire_due_events` `(tick, seq)` order + per-event `ev_shadow` — verified
  (6416–6434); zombie-drops for `ManeuverNodeStart`/`TransferComplete`/research/
  `ContractDeadline` present (6441–6470).
- `state_hash` walk completeness and the documented `pending` exclusion —
  verified (6898–6958+).
- `schedule_event` invariant and lazy capture — verified (3917–3933).
- `drain_at` per-command shadow restoring seq + fields on `Recoverable` —
  verified (5937–5961); production order is `BTreeMap` order (5911–5917).
- `resolve_apsis_ticks` strictly `> clock` — verified (scripts.rs:649–656:
  `dt_s > 0` gate, `clock + ceil(dt_s·TICK_HZ)` ≥ `clock + 1`).
- `scan_candidate`'s `tick > scan.now` guard — verified (orbit.rs:~1328).
- Deferred-tail/flush logic and `tick_budget -= advance` non-underflow —
  verified (6799–6801, 6835–6840).

## Verdict summary

| # | Finding | Verdict |
|---|---------|---------|
| 1 | `pending` pushes unshadowed, unhashed ghost commands | **CONFIRMED** |
| 2 | `tracked_vessels` unshadowed at lib.rs:3697 & joints.rs:1063 | **CONFIRMED** |
| 3 | mid-interval events fire at `clock == t_next`; clock-relative firings diverge across partitions | **CONFIRMED** |
| 4 | `Recoverable` escapes `step_frame` (3 paths) | **CONFIRMED** (colony_env arm needs post-restore env divergence) |
| 5 | wake double-tick on `InBubble` vessels | **CONFIRMED** (mechanism; intent is a judgment call) |
| 6 | wake commands pay zero-tick latency | **CONFIRMED** |
| 7 | `request_wake` bypasses `tick > clock` invariant | **CONFIRMED** |
| 8 | `script_log` not rolled back | **CONFIRMED** |
| 9 | `steer`/`steer_dir` zero-dir parity gap | **CONFIRMED** |
| 10 | `BudgetExceeded` string matching | **CONFIRMED** |
| 11 | unconditional autopilot clone; u32 source-id collapse | **CONFIRMED** |

No finding was dismissed. Review-report line citations were all accurate to
within ±2 lines.
