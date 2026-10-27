# Verified report C21 — verification of review-report-C21

Method: every file:line opened in the current tree; the HIGH re-derived from code AND re-run
empirically (scratch binary in /tmp/csp-repro, path-dep on the workspace crates, repo untouched);
Findings 3's quantitative claim measured the same way. Verdicts below.

---

### Finding 1: Revert point destroyed by capture-after-enqueue — CONFIRMED (HIGH)
**Original**: `launch_craft` enqueues the parked-build `RecoverVessel` before `self.pre_launch = Some(self.sim.snapshot()…)`; `World::snapshot` refuses on a non-empty pending queue, so the flagship iterate-on-a-craft flow loses F10/Game-menu revert.

**Verification**: confirmed by trace and empirically.
Trace (all read in the current tree):
- `crates/csp-view/src/lib.rs:2950-2956` resolves the pad occupant (surface-latched filter); `:2969-2978` enqueues `SimCommand::RecoverVessel` via `enqueue_command` → `View::enqueue_command` (`:2133-2140`) → `SimWorld::enqueue` (`crates/csp-sim/src/lib.rs:3635-3643`) which only pushes onto the `pending` FIFO — drain happens inside `step_frame`, and `launch_craft` runs no `step_frame` between the enqueue and `:2995`. (`launch_preflight` `:3441+` only reads state/posts notices; `flight_notice` `:3311-3319` only pushes an sfx cue.)
- `:2995` `self.sim.snapshot()` → `check_serializable` (`csp-sim/src/lib.rs:3497-3514`) returns `Err(SaveBlocking("commands pending at serialization boundary"))` when any pending FIFO is non-empty — it is (the RecoverVessel). `SimError` Display prefixes the class (`crates/csp-core/src/error.rs:65-75`) → exactly `"save-blocking: commands pending at serialization boundary"`.
- `revert_available` (`:3083-3085`) is `matches!(pre_launch, Some(Ok(_)))` → `false`; `revert_reason` (`:3099-3104`) → the refusal text; `revert_to_launch` (`:3110-3119`) → `Err(Recoverable("revert point could not be captured at launch"))`.
- No later capture recovers availability: the only assignments to `pre_launch` in the file are the `None` init (`:2108`) and `:2995`. The Game-menu gate is the sole reader of `revert_available` repo-wide (`crates/csp-app/src/lib.rs:2644`); the parked-build flow tests (`crates/csp-view/tests/pad_spawn.rs:148` `launch_recovers_the_parked_build_instead_of_refusing`, in-lib `launch_deliver_throttle_flies` `:5879`) never read it — the defect ships green.

Empirical re-run (scratch bin mirroring pad_spawn's `frame()` helper, `View::new()` → `seed_starter_craft()` → 1 frame → `launch_craft()`):
```
revert_available: false
revert_tick: None
revert_reason: Some("save-blocking: commands pending at serialization boundary")
revert_to_launch: Err(Recoverable("revert point could not be captured at launch"))
```
Counterfactuals: empty-pad launch (no enqueue before the capture) → `revert_available: true, reason: None` — the enqueue is the discriminator; and after the parked-build launch, draining frames does not help — `pre_launch` stays `Some(Err(…))` forever. Matches the reviewer's reproduction exactly, including the refusal string.

---

### Finding 2: Berth-blocked delivery re-fires "CONSTRUCTION COMPLETE" every retry; ETA reads ~1 s forever — CONFIRMED (MEDIUM)
**Original**: `deliver_built_vessel` re-queues with `complete_tick = now + TICK_HZ` and a new `ConstructionComplete`; every pop lands in `fired_events`, so the toast repeats every sim-second and `BuildRow.eta_s` cycles ≈1 s with no "berth blocked" reason anywhere.

**Verification**: confirmed at every link.
- `crates/csp-sim/src/manufacture.rs:327-341`: on `delivery_blocked`, re-inserts the build with `complete_tick = clock + TICK_HZ` (`TICK_HZ = 64`, `csp-core/src/time.rs:9` → 1 s) and `schedule_event(ConstructionComplete, retry_tick)`, returning `Ok(())` — the event "commits".
- `crates/csp-sim/src/lib.rs:8212` (`fire_due_events`): `fired.push(event.clone())` unconditionally for every pop, retry or not.
- `crates/csp-app/src/lib.rs:1338-1340` + `:3249-3262`: every fired `ConstructionComplete` → `flight_notice("CONSTRUCTION COMPLETE — build #N")`. `.take(8)` bounds per frame only; no cross-frame dedup/latch, so while the berth stays blocked the toast repeats once per sim-second (and the retry chain is unbounded — each fire re-schedules +1 s while still blocked).
- `crates/csp-view/src/lib.rs:4616-4625`: `BuildRow.eta_s = b.complete_tick.saturating_sub(now)` — after each retry fire the deadline is again 1 s out, so the row perpetually promises ~1 s.
- Grep for a surfaced "berth blocked"/deferred reason in csp-view/csp-app/csp-ui: none (only code comments use "berth"). Severity MEDIUM is apt: reachable whenever any vessel/debris hull occupies the pad at the deadline (e.g. a craft landed back on the pad during the build window).

---

### Finding 3: Revert retries the ordering, not the flying — every retry re-pays the build wait — CONFIRMED (MEDIUM)
**Original**: the snapshot at `:2995` precedes the `ManufactureCraft` enqueue (`:3015-3023`), so revert restores the pre-order world; each retry re-orders and re-waits `mass / 50 kg/s`, contradicting PLAN:1180 "retry of the flying" and manufacture.rs:361-365's post-delivery expectation.

**Verification**: confirmed empirically (empty-pad launch, same scratch harness):
```
funds: pre-order 1000000000000 -> post-order 974200000000 (craft mass 6350 kg, wait 127.0 s)
after delivery: vessels 1 builds 0
after revert: vessels 0 builds 0 funds 1000000000000 = pre-order funds? true
```
- The measured starter craft is 6350 kg wet → exactly the reviewer's "starter ≈127 s" at `manufacture_rate_kg_per_s: 50.0` (`data/balance.ron:70`).
- After revert: zero vessels, zero builds, funds back to pre-order — the player is at the VAB-order moment and must re-click LAUNCH and re-wait the full manufacture cycle. Debit-at-order confirmed by `manufacture_spends_at_order_delivers_at_event` (manufacture.rs tests).
- `crates/csp-sim/src/manufacture.rs:361-365` verified verbatim: "construction IS part creation … a revert to the **post-delivery** snapshot preserves the pre-drawn thresholds" — the sim side expects the revert point at delivery.
- `PLAN.md:1177-1180` verified verbatim, including "Revert is not a reroll; it is a retry of the flying." One nuance: PLAN's literal words say "restores the pre-launch snapshot" — but post-W2.6 "launch" is a build order, and the pre-drawn-θ sentence plus the sim-side comment make the reviewer's reading (the point belongs at the delivery boundary) the coherent one. Note this finding applies to the empty-pad path; in the parked-build path the capture fails outright (Finding 1), so the two findings compound.

---

### Finding 4: `--serve` host bridging is headless-only — CONFIRMED (MEDIUM)
**Original**: PRD §2.12 says the HTTP host "attach[es] to a running game"; the shipped `serve_cli` hosts its own headless world, no csp-app frame-loop attach exists despite backend.rs's comment promising one.

**Verification**: confirmed.
- `PLAN.md:799`: "**MCP — Streamable HTTP** (`csp-server`) | … ; attach to a running game" and `PLAN.md:831`: "attach-to-a-running-game is the **HTTP** path only" — both verbatim as quoted.
- `crates/csp-app/src/lib.rs:2972-3001` (`serve_cli`): `GameHost::spawn_full` with builder `|| View::new().sim` — the world is built inside a dedicated game thread and is a fresh headless `View::new()` world, not the windowed session's.
- Repo-wide grep for `GameHost` in csp-app: only `:2966` (doc comment) and `:2980` (the spawn). No `ArcSwap` publish in any csp-app frame path, no ingress drain — the promised mirroring does not exist.
- `crates/csp-server/src/backend.rs:820-825` comment verified: "the shape `csp-app` mirrors when it attaches the same backend to its frame loop (M9d)".
- Seeding sub-claim confirmed: the builder returns `v.sim` without `seed_starter_craft()`; app startup seeds at `csp-app/src/lib.rs:411-413`, `serve_cli` never does — the hosted world opens craftless.
- DEGRADED is the right coverage call: agents can drive the hosted game, but "attach to a running game" as written (dashboards over the player's windowed flight) does not exist. (If M9d simply hasn't landed, the finding stands as a PRD-vs-code mismatch to either wire or re-word.)

---

### Finding 5: Stale revert point across load; New Game never re-seeds — CONFIRMED (LOW)
**Original**: `rebuild_from_snapshot` never clears `pre_launch`, so after `load_game` the previous world's revert point stays armed (staged into the Game menu); `View::new_game` / `SessionOp::NewGame` never re-seed the starter craft.

**Verification**: confirmed, one minor imprecision in an aside.
- `crates/csp-view/src/lib.rs:1574-1602` (`rebuild_from_snapshot`): resets notice cursor, `LatchedWeldCache`, `StagePlans`, `MapPlanning`, `ScriptLogMirror`, focus override, mode — no `pre_launch` write. Only `pre_launch` assignments in the file: `:2108` (init `None`) and `:2995` (capture). `load_game` (`:3077-3079`) routes through it, and `end_frame` restages `GameMenuRes.revert_tick = self.revert_tick()` every frame (`:2258`) — so F10/Game-menu revert after a load restores the OLD world's snapshot over the loaded save. Real bug.
- Minor imprecision: the fix aside "(revert/load/new all cross here)" is wrong about *new* — `new_game` (`:3132-3146`) replaces the whole View (`*self = Self::new()`), which resets `pre_launch` via `:2108`; only load (and revert itself) cross `rebuild_from_snapshot`. Does not affect the finding's validity for load.
- Seeding: `View::new_game` calls only `Self::new()`/`Self::new_demo()`; the app's `SessionOp::NewGame` handler (`csp-app/src/lib.rs:1084-1094`) never calls `seed_starter_craft`; startup does (`:411-413`, with the comment explaining the empty-world "purple void" the seed exists to prevent). New Game reproduces exactly that. Confirmed.

---

### Finding 6: Focus-shaped session state stale across load/revert — CONFIRMED (LOW)
**Original**: the reset list misses `MapInput.focus_vessel`, `PartMenuRes`, `ManeuverPanelRes`, `PlanningPanelRes`, `GameMenuRes.ui`; same-numbered vessels in the restored world inherit stale focus, and the part menu closes only when the id differs.

**Verification**: confirmed.
- All named resources exist and are untouched by `rebuild_from_snapshot` (`:1592-1601`): `map::MapInput.focus_vessel` (`crates/csp-view/src/map.rs:273`), `PartMenuRes` (`lib.rs:267`), `ManeuverPanelRes` (`:407`), `PlanningPanelRes` (`:412`), `GameMenuRes.ui` armed confirms (e.g. `confirm_revert`, `:3158`).
- Id-collision premise is real: `next_vessel_id` rides the snapshot (`crates/csp-sim/src/lib.rs:3565`), so restored worlds legitimately re-use ids; every campaign mints low ids, so a loaded early-game save colliding with the pre-load session's ids is the common case.
- `lib.rs:729-733`: the part menu closes only when `menu.vessel != mirror.vessel` — same id keeps the stale menu open, and `DeployPart { node }` (`:1189-1193`) fires against the old world's node id on the new same-id vessel.
- `lib.rs:925+` reads `map_input.focus_vessel` for the planning/science surfaces — a stale focus silently retargets onto the new world's same-id vessel. LOW severity (contingent on collision) is right; the mechanism is real.

---

### Finding 7: `StartResearch` drop notice names the wrong reason — CONFIRMED (LOW)
**Original**: `mirror.lab_slots.get(&lab_node).unwrap_or(0)` conflates "lab node not on the focused vessel" with an empty roster, so the cue says "NO CREW ABOARD" while crew is aboard.

**Verification**: confirmed at `crates/csp-view/src/lib.rs:1208-1221`. A `lab_node` absent from `lab_slots` (stale between staging and click) yields `slots = 0` → `.take(0)` → `researchers` empty → the `:1219` cue "RESEARCH DROPPED — NO CREW ABOARD". The two distinct causes share one message; §3.7's "says why not" is violated in copy only, as the reviewer scoped it.

---

### Finding 8: `FoundColony` is free — §3.5's funds→facilities arrow unrealized for founding — CONFIRMED (LOW, with framing nuance)
**Original**: founding carries no funds debit or resource BOM while `AbandonColony` recovers 25% — asymmetric; §3.5's diagram routes funds into facilities including colony modules; honestly labeled in the UI.

**Verification**: confirmed.
- `crates/csp-view/src/lib.rs:1296-1317`: `starter_colony_def` = Solar + Battery + founding EC, no cost; the comment admits it; the UI label exists verbatim ("prototype, no cost", `crates/csp-ui/src/program.rs:1392`).
- `apply_found_colony` (`crates/csp-sim/src/colonies.rs:1588+`) contains no funds/economy debit anywhere in the pipeline (grepped the function).
- `abandon_recovery: 0.25` (`data/balance.ron:60`) salvages colony stores into the site inventory at 25% (`colonies.rs:1704`, `:1724`) — founding-free / abandoning-credited is real.
- §3.5 language, checked directly: the diagram arrow is "funds └─► facilities (labs, test stands, pads, colony modules)" (`PLAN.md:1192`); the §3.5 *prose* names "Recurring drain: salaries + facility upkeep" but never explicitly mandates a founding charge. The reviewer's PARTIAL row rests on the diagram arrow and says exactly that; fair as an honest coverage gap (per-site slots are the only bound on colony spam), not a violated explicit sentence.

---

### Finding 9: DRY/complexity in the 5.1k-line shell — CONFIRMED (NIT)
**Verification**: all five sub-items check out in the tree.
- `stage_program_vessel` (`lib.rs:3681+`): carries `too_many_arguments` + `too_many_lines` + `cognitive_complexity` waivers; 7 params with the `(ctx_biome, ctx_situation, ctx_label)` triple as out-params.
- Free-seats duplication: `lib.rs:4744-4761` (vessel-arena loop) vs `:4925-4939` (focused vessel) — identical inbound-crew filter + `vessel_seats(v).saturating_sub(crew).saturating_sub(inbound)` chain, verbatim.
- `PartFailed`/`IgnitionMisfire` player text twice with different shapes: `:3381-3391` (`drain_part_notices`, interleaved `vid.0`/`part`) vs `:4331-4338` (`notice_text`, `v()`/`p()` prefixes).
- `contract_progress` `SatelliteOrbit` arm `:4465-4494`: outer arm already matched `P::SatelliteOrbit { body, .. }` then re-matches `params` inside with an unreachable `else { String::new() }`.
- Magic `1.0e6` at `:3585`, `:3588`, `:4308` while `:3465-3466` uses `csp_sim::economy::MICROS_PER_FUND` (= `1e6`, `crates/csp-sim/src/economy.rs:91`).

---

## PRD coverage table check
- §2.2/§2.2.1 WIRED — `PLAYER_SOURCE_ID` doc `:72-78` verified; rejects surfaced as "COMMAND REFUSED" cues (`csp-app:1269-1281`) verified.
- §2.2 control lease WIRED — `TakeControl` on the pin's `seen` edge (`:2267-2283`) and the drain-reject release (`:2185-2198`) both read and verified.
- §3.5 manufacture WIRED — `launch_craft` → `ManufactureCraft` `:3015-3023` verified; debit-at-order verified empirically (Finding 3 run) and by `manufacture_spends_at_order_delivers_at_event`; edge defects correctly flagged as Findings 2-3.
- §3.5 recovery verbs / pad-trap unblock WIRED — `program_command` `:1051-1067` and auto-recovery `:2969-2982` verified; parked-build test verified.
- §3.5 colony founding cost PARTIAL — justified (Finding 8).
- §7.2/7.4 save codec WIRED — `save_game` `:3062-3071`; round-trip test `:5971-5987` (`player_save_load_round_trips`, state-hash equality + post-load determinism) verified.
- §7 load/revert plumbing WIRED w/ defects — consistent with Findings 1, 3, 5.
- §2.12 DEGRADED — justified (Finding 4).
- §3.7 feedback contract WIRED — command drop cues `:778-791`, dual-channel launch refusals `:3515-3519` (`launch_refused`), staging refusal cues `:3202-3266` all spot-verified; the copy-only gap is Finding 7.

## Verdict
9/9 findings confirmed (1 HIGH, 3 MEDIUM, 4 LOW, 1 NIT). Zero dismissals; no false positives found.
The two corrections are cosmetic and do not change any verdict: Finding 5's aside that *new game*
crosses `rebuild_from_snapshot` (it does not — the whole View is replaced), and Finding 8's §3.5
basis is the diagram arrow rather than an explicit founding-cost sentence (the reviewer's own text
already words it that way).
