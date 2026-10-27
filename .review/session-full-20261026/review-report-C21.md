# Review report C21 — View shell: schedules, mirrors, program-command glue, launch path

Files: `crates/csp-view/src/lib.rs` (code L1–5104, tests L5106–6016), `crates/csp-view/tests/{pad_spawn 181, stage_plan 306, startup_colonies 34}.rs`
(assigned as 202 lines; current tree has 306 — grew past the map). Working tree @ `7dd4b86` (clean for these files).
Scope: PLAN §2.2–2.3, §3.5, §7, §2.12, §3.7; cross-read `csp-sim/src/{lib,manufacture}.rs`, `csp-app/src/lib.rs`, `csp-server/src/backend.rs` for the launch/save/host seams.

---

### [HIGH] Finding 1: Revert point is destroyed by capture-after-enqueue — F10/Game-menu revert unavailable in the flagship launch flow
**File**: crates/csp-view/src/lib.rs:2969-2995 (with crates/csp-sim/src/lib.rs:3510-3513)
**Problem**: `launch_craft` enqueues the parked-build `RecoverVessel` (:2971) *before* capturing the
revert snapshot (`self.pre_launch = Some(self.sim.snapshot()…)` :2995). `World::snapshot` refuses while
any command FIFO is non-empty (`check_serializable` → `"commands pending at serialization boundary"`).
**Evidence**: reproduced empirically (scratch test, since removed): seed starter craft → settle →
`launch_craft()` over the parked build → `revert_available() == false`,
`revert_reason() == Some("save-blocking: commands pending at serialization boundary")`. Both in-tree
tests that exercise this flow (`launch_recovers_the_parked_build_instead_of_refusing`,
`launch_deliver_throttle_flies`) never read `revert_available`, so it ships green. Any other command
enqueued the same frame (a same-frame throttle key, an MCP command in an attached session) kills the
revert point the same way.
**Impact**: PLAN:1180 / M9b revert-to-launch — "one of the top-three QoL features of the genre" — is
silently missing exactly when the player iterates on a craft (the advertised parked-build flow). The
F10 path fails with "revert point could not be captured at launch" after the fact.
**Suggestion**: capture the snapshot *before* any enqueue in `launch_craft` (semantically correct too:
the revert world should still contain the parked, un-recovered build), i.e. move the capture to just
after `launch_preflight` returns `None`, before the `if let Some(id) = occupant` block.

### [MEDIUM] Finding 2: Deferred delivery (berth retry) posts "CONSTRUCTION COMPLETE" every retry and the ETA reads ~1 s forever
**File**: crates/csp-sim/src/manufacture.rs:327-341; crates/csp-view/src/lib.rs:4616-4625; crates/csp-app/src/lib.rs:3252-3262
**Problem**: when the pad is occupied at the deadline, `deliver_built_vessel` re-queues the build with
`complete_tick = now + TICK_HZ` and schedules a new `ConstructionComplete`. Every pop lands in
`AdvanceReport::fired_events` (`fire_due_events` pushes unconditionally), so `fired_event_notices`
posts "CONSTRUCTION COMPLETE — build #N" once per sim-second while the berth stays blocked; the
FACILITIES `BuildRow.eta_s` (`b.complete_tick − now`) cycles ≈1 s forever. There is no
"berth blocked" reason anywhere: no notification on the retry path, no distinct row text, no cap.
**Impact**: the player who lands a craft back on the pad (or parks debris there) during a build sees a
delivery-completed toast every second and a build row that perpetually promises 1 s — a paid order
that looks done/lying. At high warp the retries also eat the §2.6.5 event budget (bounded by partial
advance, so warp throttles instead of storming).
**Suggestion**: distinguish retry from delivery — e.g. a `NotificationKind::DeliveryDeferred` record
(dedup-latched) instead of surfacing the retried event as COMPLETE; add a `blocked: bool` to
`BuildRow`; consider exponential backoff on `retry_tick`.

### [MEDIUM] Finding 3: Revert-to-launch is now a retry of the *ordering*, not the flying — every retry re-pays the build wait
**File**: crates/csp-view/src/lib.rs:2995 vs :3015-3023; crates/csp-sim/src/manufacture.rs:361-365; data/balance.ron:70
**Problem**: the revert snapshot is captured before the `ManufactureCraft` enqueue, so it is a
*pre-order* world: no craft, funds un-debited, no build. After W2.6, "revert to launch" therefore
returns the player to the VAB-order moment; each retry must re-click LAUNCH and re-wait
`mass / 50 kg/s` (starter ≈127 s; a 30 t rocket ≈10 sim-minutes of warp) before flying again.
`manufacture.rs:361-365` explicitly anticipates the other design — "construction IS part creation … a
revert to the **post-delivery** snapshot preserves the pre-drawn thresholds" — i.e. the sim side
expects the revert point at delivery (craft latched, θ pre-drawn, flight not started), which is what
PLAN:1180 describes: "Revert is not a reroll; it is a retry of the flying."
**Impact**: the genre-defining retry loop (crash → F10 → fly again) gained a full manufacture cycle
per iteration; pre-drawn-θ preservation is technically intact but the flying never starts at revert.
**Suggestion**: capture the revert point at the delivery boundary (post-`ConstructionComplete`, craft
`SurfaceLatched`, before the player's first command) — e.g. the app snapshots on the fired event at
the drained boundary — or capture both and have `revert_to_launch` prefer the newest post-delivery
point when one exists.

### [MEDIUM] Finding 4: `--serve` host bridging is headless-only — the windowed session can never be attached/observed
**File**: crates/csp-app/src/lib.rs:2965-3001; crates/csp-server/src/backend.rs:820-825
**Problem**: PRD §2.12's table says the Streamable-HTTP MCP host "attach[es] to a running game", and
§2.12.1 decides "attach-to-a-running-game is the HTTP path only". The shipped path (`serve_cli`)
spawns a `GameHost` whose world is built by `View::new()` inside the game thread — its *own*
headless world. `backend.rs:823-825` documents "the shape `csp-app` mirrors when it attaches the same
backend to its frame loop (M9d)" — no such mirroring exists: `GameHost` is referenced nowhere in
csp-app outside `serve_cli`. The telemetry SPA and MCP therefore can never see the player's actual
windowed game; the "host bridging" is a second, parallel game.
**Impact**: §2.12/§2.12.1 DEGRADED — attach semantics reduced to "host a fresh headless game". Agents
can still drive a game (the eval use-case works), but dashboards-over-my-flight does not exist.
Note: the hosted world is also never seeded (`serve_cli` uses `View::new()`'s sim, no
`seed_starter_craft`) — same class as the verified `SessionOp::NewGame` finding below.
**Suggestion**: either wire the promised csp-app frame-loop attach (publish `ArcSwap` snapshot in
`end_frame`, drain ingress in `begin_frame`) or amend §2.12/PROTOCOL wording to "hosts a dedicated
headless session" so the contract matches the code.

### [LOW] Finding 5 (confirmed parallel findings, in-cluster evidence)
- `rebuild_from_snapshot` (crates/csp-view/src/lib.rs:1574-1602) never clears `pre_launch`: after
  `load_game`, the *previous* world's revert point stays armed (`revert_tick` even restages into the
  Game menu at :2258) — F10 after a load restores the old world's snapshot over the loaded one.
  Fix belongs in `rebuild_from_snapshot`: `view.pre_launch = None` (revert/load/new all cross here).
- `View::new_game` (:3132-3146) / csp-app `SessionOp::NewGame` never re-seed the starter craft —
  new game opens on an empty world, unlike boot (`csp-app/src/lib.rs:412` seeds once at startup).

### [LOW] Finding 6: `rebuild_from_snapshot` leaves focus-shaped session state stale across load/revert
**File**: crates/csp-view/src/lib.rs:1594-1601
**Problem**: the reset list covers `LatchedWeldCache`/`StagePlans`/`MapPlanning`/`ScriptLogMirror`/
notice cursor/focus pin, but not `map::MapInput.focus_vessel`, `PartMenuRes`, `ManeuverPanelRes`,
`PlanningPanelRes`, or `GameMenuRes.ui` (armed confirms). Restored snapshots legitimately re-use
vessel ids (§7.2 serializes the id counters), so a same-numbered vessel in the new world silently
inherits the old focus: the program mirror targets it, and a still-open right-click part menu
(`menu.vessel` id-equality check at :729-733 only closes when the id *differs*) can fire
`DeployPart` with the old world's node id on the new same-id vessel.
**Impact**: wrong-vessel command addressing after load/revert in the id-collision case; stale map
focus hijacks the science/recovery panel.
**Suggestion**: extend `rebuild_from_snapshot` to clear `MapInput.focus_vessel`, `PartMenuRes`,
`ManeuverPanelRes`, `PlanningPanelRes`, and `GameMenuRes.ui` armed-confirm flags.

### [LOW] Finding 7: `StartResearch` drop notice names the wrong reason
**File**: crates/csp-view/src/lib.rs:1208-1221
**Problem**: `mirror.lab_slots.get(&lab_node).copied().unwrap_or(0)` — a stale lab node (jettisoned
between staging and click) yields `slots = 0`, `researchers` empty, and the drop cue reads
"RESEARCH DROPPED — NO CREW ABOARD" while crew is aboard.
**Impact**: §3.7 "says why not" violated in copy only; the player is told to board crew they have.
**Suggestion**: distinguish `slots == 0` ("lab node not on the focused vessel") from an empty roster.

### [LOW] Finding 8: `FoundColony` is free — §3.5's "funds → facilities (colony modules)" arrow is unrealized
**File**: crates/csp-view/src/lib.rs:1296-1317
**Problem**: `starter_colony_def` founders a Solar+Battery outpost with no funds debit and no
resource BOM; the comment is honest ("the sim has no colony founding cost — the panel labels the
button 'prototype, no cost'"). PLAN §3.5's economy diagram routes funds into facilities including
colony modules, and `AbandonColony` already recovers 25% of build resources — so founding-free /
abandoning-credited is asymmetric.
**Impact**: colony spam is free (only `FoundColony` per-site slots bound it); minor economy-loop
coverage gap, honestly labeled in the UI.
**Suggestion**: either charge a def cost at apply (funds + site BOM, mirroring `ManufactureCraft`)
or record the deferral next to the §6.4 cut list.

### [NIT] Finding 9: DRY/complexity in the 5.1k-line shell
**File**: crates/csp-view/src/lib.rs
- `stage_program_vessel` (:3681-3948): 265 lines carrying `too_many_arguments` +
  `too_many_lines` + `cognitive_complexity` waivers, threading 7 out-params (three of them the
  `(ctx_biome, ctx_situation, ctx_label)` triple that `stage_program` then re-zips at :3668-3675).
  Refactor: return a `VesselStaging { ctx, keys, recovery_site, … }` struct; the mirror assignment
  at :3658 collapses to field moves.
- Inbound-crew free-seats computation duplicated verbatim (:4744-4761 vessel-arena loop vs
  :4925-4939 focused-vessel extras) — extract `free_seats(sim, id)`.
- `PartFailed`/`IgnitionMisfire` player text exists twice with different shapes (`drain_part_notices`
  :3381-3391 vs `notice_text` :4331-4339) — one `format_failure(kind, part)` serves both.
- `contract_progress`'s `SatelliteOrbit` arm re-matches `params` inside its own arm with an
  unreachable `else { String::new() }` (:4465-4494) — bind all fields in the outer arm.
- Magic `1.0e6` micros divisor at :3585, :3588, :4308 instead of
  `csp_sim::economy::MICROS_PER_FUND` (already used at :3465).

---

## PRD coverage table (in-scope requirements)

| Requirement | Status | Evidence |
|---|---|---|
| §2.2/§2.2.1 command path, absolute verbs, player FIFO | WIRED | `PLAYER_SOURCE_ID` doc :72-78; every view-issued command absolute; rejects surfaced as `COMMAND REFUSED` cues (csp-app:1269-1281) |
| §2.2 control lease (`TakeControl` Player preempts) | WIRED | issued on the pin's `seen` edge only (P04-F3, :2267-2283); drain-reject release (P04-F2, :2185-2198) |
| §3.5 manufacture = costed build order | WIRED | `launch_craft` → `ManufactureCraft` :3015-3023; funds+BOM debit at order (tests `launch_orders_a_manufactured_craft`, `bench_test_reaches_the_stand`); edge defects = Findings 2-3 |
| §3.5 recovery verbs / pad-trap unblock | WIRED | `program_command` :1051-1067 + auto-recovery on launch :2969-2982 (`launch_recovers_the_parked_build_instead_of_refusing`) |
| §3.5 colony founding cost | PARTIAL | Finding 8 (free, labeled) |
| §7.2/7.4 save codec + refusals | WIRED | `save_game` :3062-3071; refusal text propagates; round-trip test :5971-5987 |
| §7 load / revert plumbing | WIRED w/ defects | round-trip + revert tests green; Findings 1, 3, 5 |
| §2.12 `--serve` host bridging (attach to a running game) | DEGRADED | Finding 4 (headless own-world host only) |
| §3.7 feedback contract (dead clicks say why) | WIRED | every `ProgramCommand` edge drops with a cue (:778-791); launch refusals dual-channel (:3515-3519); staged-vs-latched staging refusals cue (:3202-3266) |

## Top 3 issues
1. **[HIGH] Finding 1** — revert point capture ordered after the parked-build `RecoverVessel` enqueue → `snapshot()` refuses → F10/Game-menu revert dead in the flagship iterate flow (empirically reproduced).
2. **[MEDIUM] Finding 2** — berth-blocked deliveries re-fire "CONSTRUCTION COMPLETE" every sim-second with a perpetual ~1 s ETA and no surfaced reason.
3. **[MEDIUM] Finding 3** — revert restores the *pre-order* world: every flight retry re-pays the full manufacture wait, contradicting PLAN:1180 "retry of the flying" and manufacture.rs:361's post-delivery expectation.
