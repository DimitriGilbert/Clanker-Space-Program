# Cluster C08 — Determinism deep review (event order + hash + script budgets)

Scope: `crates/csp-sim/src/lib.rs` L5975–7430 & L8392–9112, `crates/csp-sim/src/scripts.rs`,
`crates/csp-script/src/lib.rs`, `crates/csp-ai/src/lib.rs`, `crates/csp-script/tests/api_freeze.rs`,
plus the supporting machinery they depend on (`events.rs`, `economy.rs`, `life_support.rs`,
`colonies.rs`, `reliability.rs`, `research.rs`, `joints.rs`, `tech.rs`, `csp-core/src/hash.rs`,
the `step_bubble_range`/`drain_at`/`schedule_*`/`TickShadow` bodies).

Verdict: the machinery is unusually careful (per-event shadows, `(tick,seq)` total order,
`suppress`/`resolve_soi_residence` stale-clock fixes, explicit hash walks). The real problems
that remain are all in the same family the brief names: **rollback coverage**, **clock-vs-event-tick
discipline in firings**, and **one escaping `Recoverable` channel**.

---

### [HIGH] Finding 1: `pending` command queue is not covered by `TickShadow` — script/autopilot `issue`s survive a `Fatal` rollback

**File:** `crates/csp-sim/src/lib.rs` — `TickShadow` 1481–1573, `restore` 1618–1715; `WorldTickIo::issue` 7367–7372; `step_frame` restores at 6791/6819/6831/6837/6848.

**Problem.** The iteration shadow restores drained commands (`shadow.drained` → `push_front`) and `source_seqs` (`seq_restore`), but has no field for commands *pushed* during the iteration. `WorldTickIo::issue` writes `self.pending` directly; `run_script_tick` (6145–6162) and `run_autopilot_tick` (6253–6270) both hand that `&mut` to the engine/driver. Nothing ever removes those pushes on rollback.

**Evidence.** `TickShadow` fields enumerate every mutated store (vessels, heap, seq, science, tech, economy, contracts, colonies, builds, scripts, autopilots, benches, notifications…) — `pending` is absent. `step_frame`'s doc (6746–6748) claims a `Fatal` "restores clock, warp, log, **queue**, counters, arena, opt-in set, heap, and `event_seq`" — the queue claim is false for pushes. Reachability: `control_tick` runs at every integer tick inside `step_bubble_range`; a `Fatal` later in the same iteration is reachable (`batch_joint_defs`/`weld` errors at 3322/3541/8706, `schedule_event` Fatals inside `advance_interval_subsystems`/`fire_due_events`, `apply_surface_impacts`, `finish_burns`). Compounding: `pending` is *deliberately unhashed* (doc 1229–1236, `check_serializable` 2803), so `state_hash` cannot even detect the leak.

**Impact.** A `Fatal` leaves ghost `SimCommand`s in `pending`, stamped and applied by the next `step_frame` — the post-rollback world is not the pre-boundary world; a caller that retries diverges from the replayed timeline in a way the canonical fingerprint cannot see.

**Suggestion.** Record issues in the shadow — e.g. `issued: Vec<((SourceClass, u32), usize)>` counts per source appended during the iteration, popped/truncated on restore (and drop the FIFO entry if it was created by the issue) — or capture `pending` lazily like the other arenas before the first `io.issue`.

---

### [HIGH] Finding 2: `tracked_vessels` mutated without shadow capture in `step_bubble_range` and `joints::apply_latch`

**File:** `crates/csp-sim/src/lib.rs:3697` (`self.tracked_vessels.remove(&vid)` in the `dead_vessels` cleanup); `crates/csp-sim/src/joints.rs:1063–1065` (`tracked.remove(&sec)` / `tracked.insert(prim)` inside `apply_latch`).

**Problem.** `shadow_tracked` (lib.rs:3033) exists and is used by `remove_vessel` (3806) and the `SetVesselTracked` apply (4081), but the two in-range mutation sites never call it: the post-loop `dead_vessels` cleanup removes tracked ids unconditionally, and `apply_latch` mutates `tracked` while holding the shadow parameter without capturing it (the grep shows only `research::shadow_science` captured in `apply_latch`, joints.rs:967).

**Evidence.** `tracked_vessels` is hashed state (lib.rs:6951–6954). `step_bubble_range` calls `latch_pass` (3662) mid-tick and the dead-vessel cleanup at 3697, then continues into `apply_surface_impacts` (3705), `contract_notify` (3710), `finish_burns` (3713), `advance_interval_subsystems`, and `fire_due_events` — all containing `Fatal`-returning paths. `shadow.restore` (1649–1651) restores `tracked_vessels` *only if captured*; it never is on these paths.

**Impact.** A `Fatal` after a latch merge or an in-bubble removal leaves `tracked_vessels` diverged from the restored vessel arena: post-`Fatal` `state_hash` ≠ the pre-boundary hash — a direct violation of §2.13's "the whole state, so `state_hash()` is the pre-boundary hash again" (1615–1616) — and `class_of` subsequently assigns `Silent`/`Clamping` differently for the affected vessel's SOI events.

**Suggestion.** `self.shadow_tracked(shadow)` next to `shadow_autopilots` at 3695 (or inside the `dead_vessels` loop before the first removal), and capture `tracked` into `shadow.tracked_vessels` inside `apply_latch` before line 1063.

---

### [HIGH] Finding 3: events the interval slots publish mid-interval fire at `clock == t_next`, and every firing that schedules/stamps relative to `self.clock` diverges across partitions

**File:** publishes — `economy.rs:691`, `life_support.rs:283`, `colonies.rs:2172`, `reliability.rs:868`; firings — `economy.rs:643–651`, `life_support.rs:328–333`, `colonies.rs:1840/1862/2030/2120/2132/2138` (note `fire_colony_event`'s parameter is literally `_event_tick` — discarded), `lib.rs:3777–3781` vs 6515–6521.

**Problem.** `advance_interval_subsystems` runs with `self.clock == now` (the interval start); each lazy publish computes an *absolute* tick that may land inside `(now, t_next]`. `fire_due_events` then pops `e.tick <= t_next`, so such an event fires in the same iteration while `self.clock == t_next > event.tick`. Under a finer partition the identical event fires with `clock == event.tick`. The publishes themselves are partition-identical (absolute grid, same `seq` order) — the *firings* are not:

- `fire_life_support_check` schedules `VesselLifeSupportDepleted` at `self.clock.ticks() + 1` (life_support.rs:332) → `t_next+1` vs `B+1`. The crew-loss transition lands late — and since the transition changes crew status, downstream readers in the window (salary debits, other events) see different state: a true trajectory divergence, not just a hash cosmetic. The same function correctly re-arms at `event_tick + period` (343–345), which makes the clock-relative depletion stand out.
- `charge_funds` schedules `Insolvent` at `clock + 1` (economy.rs:645–651) → hashed `ScheduledEvent` tick differs across partitions (B+1 vs t_next+1; in the finer partition the event has also *already fired and been consumed* by the time the coarse run still holds it — heap contents themselves diverge).
- `fire_colony_event` recomputes at `now = self.clock.ticks()` (colonies.rs:1862); `recompute_colony` re-anchors `s.anchor_tick = tick` (2030) — `anchor_tick` is **hashed** (1155) — and schedules transitions at `tick + 1` (2120, 2132, 2138). Reachable whenever `publish_colonies` repairs a missing boundary whose needed tick lands inside the interval.
- `finish_burns`' degraded `SoiRescan` republish uses `self.clock.ticks()` — which is `now` at that point (clock restored at lib.rs:3692) — while `fire_due_events`' identical mechanism uses `t_next` (6518): two different anchors for the same recovery publish; the resulting hashed rescan tick differs across partitions.

**Evidence of the intended pattern:** the firings that get this right all key on `event.tick` — `fire_part_failure` (`fire_tick = tick.min(clock)`, reliability.rs:934), `fire_contract_deadline` (`c.deadline_tick != Some(event_tick)`, contracts.rs:918), `fire_salary_due` re-arm (`event_tick + period`, economy.rs:727), `fire_maneuver_start` (`SimTime(tick)`, lib.rs:5211). Triggering the divergence needs no degraded path: the *initial* lazy publish of `SalaryDue`/`LifeSupportCheck` (first `Active` roster, first crewed vessel, `SetLifeSupportEnabled`) fires mid-interval in any coarse first advance spanning a grid boundary.

**Impact.** §2.6.1 partition independence fails on hashed state: a `step_frame(T)` in one call vs several produces different `Insolvent`/depletion event ticks, different colony anchor ticks, and (via the depletion transition) different crew status timelines.

**Suggestion.** Make firings `event.tick`-authoritative for partition-relevant outputs: pass the landing tick into `recompute_colony`/the depletion/`charge_funds` schedules (use `event.tick + d` for derived schedules). For the `event.tick < clock` corner (only the mid-interval case — deferred-flush firings have `tick == clock`), apply the transition inline or skip the stale notification rather than scheduling at `clock + 1`. The structural alternative is re-binding `t_next` to the heap-min *after* the interval publishes so newly published boundaries bound the iteration like every other event.

---

### [MEDIUM] Finding 4: `Recoverable` still escapes `step_frame` through `step_bubble_range`'s `?` chains

**File:** `crates/csp-sim/src/lib.rs:3541` (`transfers::reweld_if_mass_changed(v, true)?`), `lib.rs:3533` (`self.tech.credit_qualification(family, burn_s * rate, 0.0, 0)?`), `lib.rs:3705` → `colonies.rs:1800` (`apply_damage_module?`) → `colonies.rs:2007` (`recompute_colony` `Recoverable`).

**Problem.** `step_frame` propagates *any* `Err` from `step_bubble_range` as `Err` after `shadow.restore` (6818–6821), but `AdvanceReport` documents "`step_frame` only ever returns `Err` for `Fatal`" (1204). Three `?` inside the range can return `Recoverable`: `reweld_if_mass_changed` maps weld failure to `Recoverable` (transfers.rs:620); `credit_qualification` rejects non-finite/negative credits (tech.rs:142–148) — `burn_s * rate` can be non-finite; `apply_surface_impacts`→`recompute_colony` returns `Recoverable` when a live colony on `dominant` lacks a `colony_env` entry (the pre-filter at colonies.rs:1775 only guards dormant/abandoned/module-index).

**Impact.** A non-fatal condition aborts the whole call — the world is rolled back correctly but the caller's `Err`-means-`Fatal` contract is violated (a retry would replay the same tick and hit the same wall — a soft wedge rather than a degradation report).

**Suggestion.** Either map these paths to `SimError::Fatal` (they are invariant failures — a re-weld of an unchanged graph, a NaN burn credit) or push them to `degraded` like the surrounding passes do.

---

### [MEDIUM] Finding 5: wake-fired scripts on `InBubble` vessels double-tick at the wake tick

**File:** `crates/csp-sim/src/lib.rs:6365–6384` (`fire_script_wake`), 6088–6118 (`control_tick`).

**Problem.** `fire_script_wake`'s `live` check is `s.enabled && s.wake_tick == Some(tick)` — no residence check. A script bound to an `InBubble` vessel already ticks once per integer tick in `control_tick`; if it also registered a wake, the `ScriptWake` event fires a *second* `on_tick` at that tick value — extra strikes/poll bookkeeping, extra `persist` writes, double `issue` volume. §2.10 frames the wake as "the tick for a script whose vessel has no bubble ticks" (scripts.rs:40–43) — the redundant firing for a bubble script looks like an oversight, not a contract.

**Impact.** Same-tick double `on_tick` — deterministic, but observable semantics: a `wait_*` call on a bubble script yields two ticks at the wake tick instead of one; budget/strike/poll accounting counts the extra call.

**Suggestion.** Gate `fire_script_wake` on the bound vessel not being `InBubble` (or explicitly document the double-tick as intended semantics and pin it in a test).

---

### [MEDIUM] Finding 6: commands issued by wake-fired scripts stamp at the same tick — zero-tick latency vs the bubble path's one-tick

**File:** `crates/csp-sim/src/lib.rs:7367–7372` (`issue`), `drain_at` 5900–5966, `fire_due_events` 6407+.

**Problem.** A `ScriptWake` fires during `fire_due_events` *after* `clock.try_advance` lands `clock == t_next`. Commands the script issues sit in `pending` and drain at the *next* iteration's boundary drain, stamped at `clock == t_next` — i.e., the same tick value the script's `on_tick` ran at. A control-tick script at tick `t` pays the documented one-tick latency (its commands stamp `t+1`, scripts.rs:34–35 "the same one-tick latency every producer pays"). Wake-fired commands therefore apply one stamp earlier — an asymmetry between the two tick paths.

**Impact.** Deterministic, but a real model asymmetry: a script can get an action applied at tick `T` where a bubble-resident script issuing at `T` applies at `T+1` — exploitable ordering-wise and contrary to the stated latency contract.

**Suggestion.** Either accept and document the wake path's same-tick stamping, or route wake-issued commands with an effective minimum drain tick of `t_next + 1` (e.g., stamp-aware enqueue or a held-until-next-boundary marker).

---

### [LOW] Finding 7: `request_wake` bypasses `schedule_event`'s ahead-of-clock invariant

**File:** `crates/csp-sim/src/lib.rs:7393–7402`, `resolve_wake` 7212–7218.

**Problem.** `WorldTickIo::request_wake` pushes a `ScheduledEvent` directly rather than through `schedule_event`, so the `tick > clock` invariant (3918–3920) is never enforced. `resolve_wake`'s `AtManeuverNode` returns `node.tick` and `AtNextSoiTransition` returns a pending event's tick — both can be `≤ clock` in the deferred-flush corner (a `Scheduled` node whose start sits in an unfired tail). The result is a heap entry with `tick < clock` outside the documented "deferred tail is the only state in which `tick == clock` is legal" (6395–6399) — `<` is not even the tolerated `==`.

**Impact.** The flush machinery handles it (the wake pops in `e.tick <= t_next` order and `fire_script_wake` accepts it since `wake_tick == Some(tick)`), so behavior stays deterministic — but the heap invariant the flush-detection rule ("heap-min ≤ clock ⟺ mid-flush", 6754–6758) documents is violated by a non-tail cause.

**Suggestion.** Clamp resolved ticks to `clock + 1` in `request_wake`, or route through `schedule_event` and treat a stale resolution as the registration-refusal `Recoverable` (`AtManeuverNode`/`AtNextSoiTransition` are resolved fresh each call anyway).

---

### [LOW] Finding 8: `script_log` is not rolled back on `Fatal` — ghost console lines from restored iterations

**File:** `crates/csp-sim/src/lib.rs:6869` (`std::mem::take(&mut self.script_log)`), `TickShadow` 1481–1573 (no field), `run_script_tick` 6182–6195, `log_line` 7421–7423.

**Problem.** `script_log` lines pushed during a rolled-back iteration (script ticks, wake firings, strike/disable notices) are not truncated on `Fatal` — unlike `command_log`, which is (`log_len`, 1621). They surface in the *next successful* report as if the ticks ran.

**Impact.** Observability dishonesty, not state divergence — the console claims a tick (and possibly a strike/disable) that the restored world never committed.

**Suggestion.** Add `script_log_len: usize` to the capture and truncate on restore, mirroring `command_log`.

---

### [LOW] Finding 9: `steer`/`steer_dir` degenerate-input parity gap between the two `ControlApi` impls

**File:** `crates/csp-script/src/lib.rs:150–157` (`IoControlApi::steer_dir`) vs 714–733 (Rhai `steer`).

**Problem.** On a zero direction, `IoControlApi::steer_dir` issues `SetAttitude` with the vessel's *current* telemetry rotation, while the Rhai `steer(0,0,0)` issues `SetAttitude` with `DQuat::IDENTITY`. The doc claims parity between the guest surface and the `ControlApi`; the degenerate input behaves differently depending on which side calls it.

**Impact.** Minor behavioral divergence between the script and autopilot/Rust surfaces; deterministic in each, but the parity claim is not exact.

**Suggestion.** Pick one convention (telemetry-rot is the safer one — "hold current") and apply it in both bindings.

---

### [LOW] Finding 10: `BudgetExceeded` classification by error-string matching

**File:** `crates/csp-script/src/lib.rs:973–986`.

**Problem.** `run_tick` classifies `ScriptFault::BudgetExceeded` via `msg.contains("Terminated") || msg.to_lowercase().contains("too many operations")`. A guest `throw` containing either substring misclassifies a runtime fault as a budget abort. Consequences are bounded (a strike either way; the misclassification only feeds the `heavy` poll detector), but a script can deliberately self-mark as budget-exhausting to steer `poll_demoted`, and a Rhai message-format change silently misclassifies the real case.

**Suggestion.** Match `rhai::EvalAltResult::ErrorSystem` payloads or check `Engine`'s termination error type instead of substring-matching the rendered message.

---

### [LOW] Finding 11: minor hygiene — unconditional autopilot shadow clone; `u32` source-id collapse

**File:** `crates/csp-sim/src/lib.rs:3693–3700`; `crates/csp-script/src/lib.rs:1030`; `crates/csp-ai/src/lib.rs:201`.

**Problem.** (a) `self.shadow_autopilots(shadow)` at 3695 runs unconditionally at the end of every `step_bubble_range` (every iteration containing an `InBubble` vessel), cloning the whole arena even when `dead_vessels` is empty — the comment claims "Lazy capture — no-op when `dead_vessels` is empty", which is false (the *capture* itself is the cost; the call should sit behind `!dead_vessels.is_empty()` or inside the removal loop). Performance only, no correctness issue. (b) `source_id = id.min(u32::MAX)` in both the script command stamping (csp-script:1030) and the autopilot (csp-ai:201) collapses all ids > `u32::MAX` onto one `(class, source_id)` FIFO — deterministic but a shared `source_seq`/ordering channel at absurd scale.

**Suggestion.** (a) Gate the capture on `!dead_vessels.is_empty()`. (b) Document the collapse or widen `source_id` to `u64` when the arena id spaces are next touched.

---

## Notes (verified clean, in scope)

- `fire_due_events` pops in `(tick, seq)` order; per-event `ev_shadow` correctly rolls back only the failed firing while the pop stands; zombie-drops for `ManeuverNodeStart`/`TransferComplete`/research/`ContractDeadline` are consistent.
- `state_hash`'s walk is complete for its documented scope: `source_seqs`, `event_budget`, caps, docking/transfer/science/economy/contract/life-support/reliability configs, all counters, `tracked_vessels`, the heap in canonical `(tick, seq)` order, joints + `joint_refused`, `next_part_instance_id`. Deliberate exclusions (`pending`, `command_log`, `script_log`, `notifications`, `physics.aero`/`thermal_rates`, `site_defs`/`colony_env`/`orbits`) are documented; the `pending` exclusion is *inter alia* what makes Finding 1 invisible to the fingerprint.
- `ScheduledEvent::Ord` is a correct inverted `(tick, seq)` compare; heap-min semantics hold.
- `step_frame`'s deferred-tail/flush logic is sound: events can only reach `tick == clock` via the batch-cap tail; drain postponement is consistent; `tick_budget -= advance` can't underflow (`t_next ≤ now + budget`).
- `scan_candidate` guards `tick > scan.now` (orbit.rs:1328) — no boundary-exact crossing can trip `schedule_event`'s `Fatal`.
- `drain_at` per-command shadow correctly restores seq + touched fields on `Recoverable` rejects; production order per source is `BTreeMap` order.
- `resolve_apsis_ticks` returns strictly `> clock` for accepted apsides; `RhaiHost::run_tick`'s effects-drain is atomic (persist conversion → wake resolution → issue/log).
- `advance_orbits` is read-only over `BTreeMap` order; `control_tick` iterates `ScriptId`/`VesselId` ascending per §2.10.
- The api-freeze test pins both the trait method set (parsed from source) and the guest surface const; the arity check in `control_api_surface_is_registered` is a real negative proof.
- `fire_script_wake`'s `wake_tick = None` consume-before-run and `apply_unload_script`'s scrub+lease-release are correct.
