# PRD-03 Review — csp-sim tick machine (drain · apply · schedules · warp · events · residence transitions)

**Cluster:** `crates/csp-sim/src/lib.rs` L1143–8840 — `step_frame` (7855), `drain_boundary`/`drain_at` (6849), `apply_command` dispatch + all in-file `apply_*` (4773–6825), `advance_interval_subsystems`/`advance_orbits` (6969–7059), `control_tick`/`run_script_tick`/`run_autopilot_tick` (7082–7281), `fire_due_events`/`fire_event`/`apply_soi_transition` (7434–7806), `step_bubble_range` + `finish_burns` + `remove_vessel` + `schedule_*` (3656–4766), `TickShadow` capture/merge/restore (1749–2099), `WorldTickIo` (8503–8860), `event_names_vessel`/`shadow_events_split` (8862–8980), `transition_latched` (10190+).

**Method:** traced the §2.6.2 advance loop line-by-line against PLAN §2.5/§2.6, walked every residence transition for player vs agent producers (grep'd all non-csp-sim crates for `SimCommand::` issuance — full census below), and audited the drain/apply/rollback channels for silent drops.

---

## 1. Headline findings

1. **CRITICAL — there is no bubble→rails path for a player vessel, and no auto-conversion.** `ToRails` is produced by nothing outside csp-sim tests, `csp-bench/scenarios.rs:549`, and MCP `sim.command`; the §2.3 bubble-radius model (20 km / 18 km hysteresis / 1-km lattice recentering) is *entirely absent* — `bubble.rs` documents a body-centric unbounded bubble and `step_bubble_range` never tests a residence-exit condition. A launched vessel stays `InBubble` forever; since `apply_set_maneuver` requires `v.orbit()` (L6018), **the player can never reach orbit, never set a maneuver node, and never exercises SOI transitions, transfers, or `OrbitAchieved` contracts.** Details in D1.
2. **HIGH — `apply_to_bubble` materializes with no surface-clearance or occupancy gate** (L5908–5964). Dead-stick recovery on a suborbital conic stamps a body inside the terrain; two bodies can be materialized into one volume — the exact ghost-overlap hazard `apply_spawn_physical` refuses at L5507–5516. Details in D2.
3. **MEDIUM — the event budget splits a same-tick batch mid-tick**, violating the spec the code itself quotes: "fire ALL events at `t_next` is verbatim spec; a half-fired tick is exactly the corrupt mid-state back-pressure exists to avoid" (L7398–7402) vs the break at L7948–7952. Details in D3.
4. **The drain/apply/rollback machinery is otherwise honest and engineered to spec**: per-command `TickShadow` restores, seq-stamp rollback, drained-FIFO restore, issued-command pop accounting, clock anchored at `event.tick` for partition independence, one-drain-per-tick with deferred-tail drain postponement, constant `SUBSYSTEM_ORDER`, deterministic BTreeMap/ascending-id iteration everywhere (zero `HashMap` in the file). Rejections surface to the player as a `COMMAND REFUSED` flight notice (`csp-app/lib.rs:944–956`). No apply on the ~17 player-reachable commands silently eats input.
5. **Correction to the exploration map:** `apply_spawn_physical` is *not* free — it charges `economy::craft_cost_micros` via `charge_funds(cost, Manufacture)` at L5582 and rejects on insufficient funds (L5495–5500). The launch bypass is *instant delivery + no site inventory*, not no-charge.

---

## 2. Findings

### D1 — CRITICAL: residence machine is a dead end for players (no bubble→rails conversion)

**File:** `crates/csp-sim/src/lib.rs` — `apply_to_rails`/`apply_to_rails_at` L5615–5749; dispatch arm L4837; `step_bubble_range` L3656–4451; `rails_warp_refusal` L3382–3430.

**Problem:** Three residence transitions exist:
- `InBubble → SurfaceLatched`: **automatic** via `transition_latched` (L10190+, driven by the backend's settle counter, L4087–4110). Works.
- `SurfaceLatched → InBubble`: **player-reachable** implicitly — `unlatched_body` (L3542–3630) re-materializes on any throttle/attitude command, per §2.6.4. Explicit `Unlatch` (L5266) is unreachable but redundant. Works.
- `InBubble → OnRails`: **command-only, and no player producer exists.** `apply_to_rails` is called from (a) `SimCommand::ToRails` — issued only by `csp-bench/src/scenarios.rs:549` and MCP `sim.command`, and (b) `finish_burns` (L4506) — which requires a `Burning` node, which requires `apply_set_maneuver` to have succeeded, which requires `v.orbit()` — i.e., OnRails already. There is no §2.3 auto-conversion: nothing in `step_bubble_range` or anywhere else checks a vessel's distance from a bubble origin and converts it.

**Evidence (producer census):** the complete set of `SimCommand` variants issued from `csp-view`/`csp-app`/`csp-ui` production code: `SetWarp, AdvanceTo, SetThrottle, SetAttitude, SpawnPhysicalVessel, ToBubble, SetManeuver, DeleteManeuver, JettisonPart, TakeControl, RunExperiment, TransmitData, RecoverData, ResearchTech, ResearchUpgradeLevel, AcceptContract, SetLifeSupportEnabled` — 17 of 49 variants. `ToRails`, `Unlatch`, `SpawnVessel` (tests only), `SetVesselTracked`, `SetAlarm`, `WatchTransferWindow`, `InstallAeroTables`, `SetAeroConfig`, `Undock`, all transfers/crew/colony/manufacture/research-project/script/autopilot/bench commands have **no** player producer. Matches PRD-02's ~15/51.

**Impact:**
- `apply_set_maneuver` (L6018–6020) rejects every player attempt: "maneuver: vessel not on rails". The map maneuver panel honestly filters to on-rails vessels (`csp-view/lib.rs:665–669`) → the picker is permanently **empty** for a player. No silent refusal — a silent absence, which is worse for discoverability.
- `recover_control` → `ToBubble` (csp-view:1946) can never find an eligible vessel → permanently "NO VESSEL TO RECOVER".
- `OrbitAchieved` contract trigger (L5345, 5739, 7802) never fires for a player craft; the §2.6.6 SOI machinery, `schedule_next_soi` (L4721), `resolve_soi_residence` (L5799), debris decay, `ManeuverNodeStart`/`fire_maneuver_start` (L6119), the whole burn executor (`drive_node_burn`), `WatchTransferWindow`, and Duva reachability are unreachable in a player run — the sim's entire orbital layer is bench/MCP-only.
- **Warp collapses in flight.** `rails_warp_refusal` (L3401–3428) refuses only on thrust, contact, or atmosphere — a coasting `InBubble` vessel above `atm.top_m` passes. Warp then runs with the bubble occupied: `step_bubble_range` steps *every integer tick* in `now..t_next` (L3705), up to `MAX_TICKS_PER_FRAME = 69,120` (L214) per frame — ~18 sim-minutes of full physics per frame at ×10⁵. The §2.6.3 controller demotes on measured step cost, so the outcome is honest-but-useless: permanent ×1–×2 warp while any vessel flies. The design's intent (rails = analytic jump) is defeated by the missing conversion.
- The §2.6.1 ×1-vs-×10⁵ equivalence property is untestable in the player-visible game since the player path never leaves the bubble.

**Suggestion:** the residence machine needs its missing edge. Options in order of spec-fidelity: (a) implement §2.3 — bubble membership derives from range-to-`active_vessel` with 10% hysteresis and lattice recentering; vessels outside the radius convert via `apply_to_rails_at` automatically at tick boundaries; (b) under the implemented body-centric model, auto-convert when the ToRails refusal predicates are all false (out of atmosphere, no live contact, conic clears surface) — this is a small, honest rule and makes "achieve orbit" a physical outcome rather than a UI verb; (c) at minimum, expose `ToRails` in the UI so the manual path exists while the auto rule is designed. Also give `apply_set_maneuver` an in-bubble path (a `Scheduled` node that materializes at its tick is already the executor's model — `drive_node_burn` runs per `InBubble` tick at L3912).

### D2 — HIGH: `apply_to_bubble` lacks surface-clearance and occupancy gates

**File:** `crates/csp-sim/src/lib.rs` L5908–5964; eligibility mirror `csp-view/src/lib.rs:1970–1997`.

**Problem:** The apply validates: vessel exists, `v.orbit()`, `orbit.parent == dominant`, craft exists, `state_at` solves, weld succeeds. It then stamps `body.pos = r + center − rot·com_local` **unconditionally**. Compare:
- `apply_to_rails_at` refuses a conic that intersects the surface — `rp > env.radius_m` (L5684–5693).
- `apply_spawn_physical` refuses an occupied site via `spawn_site_occupied_by` (L5514) — motivated at L5507–5513: "coincident hulls produce no contacts … the pair ghost-overlapped forever."

`apply_to_bubble` has neither check, and `to_bubble_eligible` (the pre-pin mirror) doesn't either — so the pin arms and the command *applies* rather than rejecting with a typed refusal.

**Impact:** `recover_control` on a suborbital/decaying conic materializes the vessel inside the terrain or inside another body. Outcomes are whatever the contact solver does with an interpenetrated state — ejection at unphysical velocity, or a structural breakup — presented as legitimate physics, not a refusal. For non-debris vessels there is no impact-removal rule, so the solver owns the wrongness. Same for vessel-vessel overlap: the sim deliberately has no rule against two hulls in one volume (only spawn sites are gated).

**Suggestion:** mirror the rails-side gates: reject `Recoverable` ("to-bubble: conic intersects the surface") when the materialized body's lowest point penetrates the terrain datum at the stamped tick, and run `spawn_site_occupied_by(&body)` against InBubble/latched residents. If "recover a doomed vessel" is a deliberate feature, the honest version is a refusal with a reason, not an underground spawn.

### D3 — MEDIUM: event budget splits a same-tick batch mid-tick

**File:** `crates/csp-sim/src/lib.rs` L7434–7595 (`fire_due_events`), L7948–7952 (`step_frame` break).

**Problem:** §2.6.5: the 2000-events-per-frame budget is "checked **between same-tick batches** — the loop stops only at a tick boundary. A single tick's due batch is atomic against the budget." The implementation checks `events_processed >= self.event_budget` after *every* `fire_due_events` call. `fire_due_events` returns after `MAX_EVENTS_PER_BATCH = 1024` pops, leaving the same tick's tail in the heap (`flushing`, L7874–7877, detects this next iteration and skips the drain — correct). But the budget break doesn't check whether the heap head is still at the clock: a same-tick batch larger than `event_budget` fires `ceil(batch/1024)` iterations until `events_processed` crosses 2000, then breaks **mid-tick** with the remainder still pending at `clock`.

Concrete: 3000 events at tick T → 1024 + 1024 fired, 976 left at tick T, `partial: true`, presentation reads the half-applied state for a frame.

**Impact:** deterministic (no replay divergence — a resumed frame flushes the rest), but exactly the "corrupt mid-state" the spec and the function's own doc (L7398–7402) name as the thing to avoid: contracts partially expired, half the colonies' `TerminatorCrossing`s applied, etc., rendered for one frame. Requires >2000 events at one tick — rare, but the pathological-density scenario §2.6.5 prescribes is exactly where a same-tick pile-up can occur (synchronized colony recomputes + debris decay + cascades inserting same-tick events mid-batch).

**Suggestion:** break on the budget only at a tick boundary — e.g., `events_processed >= self.event_budget && self.event_heap.peek().is_none_or(|e| e.tick > self.clock.ticks())`.

### D4 — MEDIUM-LOW: `finish_burns` evaluates the terrain/contact predicate at the range-start phase

**File:** `crates/csp-sim/src/lib.rs` — `self.clock = now` restored at L4424, `finish_burns` called at L4450, `apply_to_rails_at`'s `physics.backend.set_sim_time(self.clock)` at L5668.

**Problem:** `apply_to_rails_at`'s `ground_contact` check samples body-fixed terrain at `self.clock`'s spin phase. Called from `finish_burns`, the clock sits at range start `now` while the body was integrated to `t_next` and the conic is stamped at `t_next`. On a spinning dominant body the terrain under the vessel at `t_next` is `ω·(t_next−now)` ahead of the sampled phase — the guard can pass (or refuse) against the wrong terrain.

**Impact:** bounded — the post-burn vessel is almost always airborne, so the contact check is a formality. But the predicate exists to refuse railing a vessel in contact, and on a rotating body it reads a stale phase. Honest fix: `set_sim_time(SimTime::from_ticks(t_next))` for the conversion's checks (or have `apply_to_rails_at` take the eval tick like it takes `epoch`).

### D5 — LOW: `apply_set_aero_config` silently substitutes a different mask

**File:** `crates/csp-sim/src/lib.rs` L6405–6414.

**Problem:** when the requested mask's table can't be built (cache at `MAX_CONFIG_MASKS`), `resolved` becomes the nearest cached mask by `(hamming, id)` — the apply then commits `v.aero_mask = resolved` and logs the command *applied*. The logged command requests mask X; the world stores mask Y.

**Impact:** no signal to the caller that the substitution happened (no `degraded`, no rejection) — the UI must read back `aero_mask` to notice. Deterministic, replay-safe, and unreachable in practice (no deployable parts — PRD-02 D3-class). Suggest rejecting `Recoverable` on cap, or reporting the substitution on `degraded`.

### D6 — LOW: `reject_kind` types exactly one variant

**File:** `crates/csp-sim/src/lib.rs` L1232–1237.

**Problem:** the `RejectedCommand.kind` field exists so consumers "match a kind, never `SimError` text" (L1215–1222) — but only `ToBubble` gets a kind; all 48 other variants collapse to `RejectKind::Other`. The one consumer that needs it (the `ToBubble` pin release, `csp-view/lib.rs:1496–1501`) is served; the mechanism invites the assumption of coverage that doesn't exist.

**Impact:** none today. Either populate kinds per command family or narrow the enum's contract to "the kinds consumers actually branch on."

### D7 — LOW: at-or-past target ticks are silently re-anchored to `clock + 1`

**File:** `crates/csp-sim/src/lib.rs` L5214 (`apply_set_alarm`), L5248 (`apply_watch_window`), L6033 (`apply_set_maneuver`).

**Problem:** a command carrying `tick ≤ clock` is rewritten to `clock + 1` and applied — the §7.6 log records the original argument while the world schedules the clamped one. Deterministic and documented ("may-not-rewrite-the-past clamp"), but the caller gets `applied` for an effect it did not ask for. For `SetManeuver` specifically, a player placing a node a tick in the past gets a node at `clock+1` — nearly always what they meant anyway. Consistent house rule; flagged only because it is the one place an apply rewrites its arguments rather than rejecting.

### D8 — LOW: `physics.aero` writes are unshadowed

**File:** `crates/csp-sim/src/lib.rs` — `apply_install_aero` L6322–6340, `apply_set_aero_config` L6379–6403, `apply_to_bubble` scrub, `ensure_aero`, `remove_vessel` L4585–4587.

**Problem:** the `physics.aero` table cache has no `TickShadow` field — every other mutable structure is captured. It's safe *today* because the cache is §7.3 derived state: readers re-validate by `graph_hash` (L6386) and `aero_tables_stale`, and `ensure_aero` rebuilds missing entries. A `Fatal` rollback can therefore strand at most a stale-but-correctly-ignored cache entry. Worth an explicit comment or an assertion, since the invariant "every mutation is shadowed" is the subsystem's whole correctness argument (§2.13) and this is the one silent exception.

### D9 — INFO: `SetWarp` stores any `u32`; `partial`/`effective_target` are app-unused

**File:** `crates/csp-sim/src/lib.rs` L4800, L2111–2113, L7955–7965.

**Problem:** `SetWarp(999)` applies and is logged — documented non-interpretation ("the sim interprets nothing"; test asserts `u32::MAX` stores at L11257). Consistent with §2.6.3: the ladder is the app's business. Likewise `AdvanceReport.partial`/`effective_target` are unread by `csp-app`/`csp-view` — but they *are* consumed by `csp-bench` (`economy.rs:123–126`, replay resumption), so the fields are not dead; the live path just self-corrects by re-targeting each frame. No change needed; noted for the record.

### D10 — INFO/GAP: fired notification-class events have no presentation consumer

**File:** `crates/csp-sim/src/lib.rs` — `fired_events` populated at L7457, documented at L1254–1260; `csp-app/src/lib.rs:931–934`.

**Problem:** `PlayerAlarm`, `TransferWindow`, `Insolvent`, `ResearchProjectComplete`, `ConstructionComplete`, `ContractDeadline`, colony power/store transitions all fire honestly into `fired_events` — but the app reads the list only for `class == Clamping` demotion. No label, deadline name, or completion message reaches the player. The sim side is honest; the surfacing gap lives in presentation (PRD-15/16 territory). Compounds with reachability: `SetAlarm` itself has no player producer, so even the hidden channel is unused.

### D11 — INFO: `advance_orbits` reports an unthrottled `degraded` per unsolvable conic per frame

**File:** `crates/csp-sim/src/lib.rs` L7041–7059.

**Problem:** a persistent `state_at` Recoverable (e.g., a degenerate conic) pushes onto `degraded` every frame forever — the report grows one entry per vessel per call. Honest (it *is* a degradation each interval) but noisy; consider dedup or rate-limit if such a state can persist.

---

## 3. What is verified honest (no defect)

- **Drain semantics.** `drain_at` (L6871–6960) pops in `(source_class, source_id)` BTreeMap order = `CommandKey` order; stamps per-source `source_seq`; rejected commands roll back their seq stamp, effects, and log entry via per-command `TickShadow` (L6921–6957) — "post-drain state is identical to the command never having existed" holds. `Fatal` aborts the iteration and restores the drained FIFOs verbatim via `shadow.drained` (L6891, 1947–1949).
- **Deferred-tail rule.** `flushing` (L7874–7878) postpones the drain while a same-tick tail is pending — "a tick value still hosts exactly one drain, at the departing iteration once the batch clears" is implemented (modulo D3's mid-tick *break*, which skips straight to `partial`).
- **One-tick latency.** `WorldTickIo::issue` stamps `not_before = clock + 1` (L8795); external `enqueue` stamps `0` (L3323) → first boundary. In-range drains stamp the in-range tick `t` (L3728).
- **Clock discipline.** `self.clock` tracks the in-range tick for drain+control (L3719), restores to `now` (L4424), and anchors at `event.tick` during each firing (L7468) — the §2.6.1 partition-independence argument is engineered, not just claimed; the doc at L7409–7421 explains the `event.tick + K` rescan anchor and it's implemented (L7582–7589).
- **Interval advance.** `advance_interval_subsystems` iterates `SUBSYSTEM_ORDER` verbatim (`events.rs:97`); every non-empty slot is honestly publish-only: `advance_orbits` mutates nothing (epoch-anchored conics — correct §2.6.1 reading), Colonies/LifeSupport/Reliability/Contracts slots repair lazy publishes (L6985–7017), and the no-op slots are documented as no-op *by definition* (L7018–7025).
- **Event machinery.** Per-event `ev_shadow` rollback (L7462–7487); zombie cleanup for `ManeuverNodeStart`/`TransferComplete`/research-pair/`ContractDeadline` failures (L7493–7522); `SoiRescan` republish at `event.tick + DEGRADED_RESCAN_TICKS` for vessel-named failures (L7582); `finish_burns` mirrors the whole fallback including the republish-on-post-stamp-failure case (L4509–4528).
- **Classes.** `events::class_of` matches the §2.6.5 table (tracked-SoI opt-in promoted via in-heap class rewrite, L4849–4866); extras (`VesselLifeSupportDepleted`, `PowerLost`/`Restored`, `TransferComplete`, `ResearchDataExhausted`) are reasonable `Clamping` choices — `TransferComplete` clamping on *background* transfers is the only debatable one (a colony resupply demotes warp even when the player doesn't care — opt-in tracking doesn't extend to transfers).
- **Control pass.** `control_tick` runs enabled scripts ascending `ScriptId` then autopilots ascending `VesselId`, `InBubble` only (L7091–7110); `fire_script_wake` consumes the registration and refuses to double-tick a bubble-resident script (L7380–7391); strike-disable vacates leases (L7227–7229).
- **Lease layer.** `check_control_lease`/`apply_take_control` implement the §2.2.1 matrix including `Ui` preempts, holder-transfers-at-will, and `Agent(client)` existence proven via `source_seqs` (L5103–5114).
- **`EngageAutopilot` refuses programs no installed driver flies** (L5006–5019) — prevents the "applied but forever inert" lie.
- **Spawn funds.** `apply_spawn_physical` charges `charge_funds(cost, Manufacture)` (L5582) after all validation; the pad-occupancy gate (L5514) covers InBubble *and* re-materialized latched hulls.
- **Determinism surface.** No `HashMap` in the file; `BTreeMap` arenas, ascending-`VesselId` batch order, heap `(tick, seq)` total order, `libm` in `schedule_debris_decay`/`bubble.rs` — T1 discipline holds in the tick machine.
- **Warp plumbing.** `warp_level` is stored/hashed/never interpreted (L2111); `rails_warp_refusal` names the offending vessel (L3405–3427); the app's `Clamping`-demote and refusal-demote are wired (`csp-app/lib.rs:926–979`) — the *machinery* is honest end to end; what's missing is the state that would make it useful (D1).

## 4. Items for other clusters / the parent agent

- **PRD-16 (view glue) owns the fix surface for D1** — the sim correctly provides `ToRails`; the missing piece is the auto-conversion rule (§2.3 bubble model or the refusal-predicate inversion) and/or a UI verb. PRD-16's launch-bypass note should also be corrected: launch *is* charged funds (L5582); what's skipped is `ManufactureCraft`'s site inventory + build time.
- **PRD-15/16:** fired notification events (`PlayerAlarm`/`TransferWindow`/completions) have no display consumer (D10).
- **PRD-05 (orbit layer):** `advance_orbits`'s per-frame `degraded` spam (D11) and whether `state_at` Recoverables can persistently recur.
- **PRD-04 (restore/tests):** verify a save/restore test covers the deferred-tail state (heap entries at `clock`) — `World::restore` is documented to accept `≥` (L7406) but the snapshot/refusal interplay with a mid-flush clock deserves a golden.
- `csp-view/src/lib.rs:1487–1501`'s typed `ToBubble`-reject pin release is the *only* consumer of `RejectKind` — if D6's enum is kept thin, document that in the type.
