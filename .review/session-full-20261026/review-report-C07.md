# C07 — Transfer planner (transfer.rs) + transfer processes (transfers.rs)

Scope: `crates/csp-sim/src/transfer.rs` (2,556), `crates/csp-sim/src/transfers.rs` (1,708) + their
wiring (`lib.rs` apply/event paths, `csp-view/src/map.rs` planning panel). All 21 `transfer::` unit
tests, 38 transfer-process tests in `lib.rs`, and `duva_reachable_in_sim_end_to_end` pass (run
today). An empirical probe replicated the map panel's exact flow (porkchop_query → plan_transfer →
departure_node → impulsive burn at the node tick) against the §3.0 home-system fixture; numbers
below are from that probe (scratch test, deleted after the run).

### [HIGH] Finding 1: The staged transfer plan's burn cannot reach its destination — the panel sells an encounter it does not deliver
**File**: crates/csp-sim/src/transfer.rs:1529 — **Problem**: `departure_node`'s burn, flown exactly,
misses the target body by a wide margin, and no in-sim correction is possible. **Evidence**: the
module doc itself (transfer.rs:1529–1537) states the node "does NOT compose to arrival on its own"
(SOI egress is modeled as instantaneous; the burn rides up to one parking period early) and that a
correction loop is "REQUIRED, not optional" — but no correction loop exists anywhere: the executor
is dominant-parent-only (maneuver.rs:37–43, "a vessel that re-parents before its node fires has the
firing degrade and the node dropped"), and the M5 end-to-end test explicitly bypasses
`departure_node` with a custom `solve_egress_hyperbola` Lambert solve because "no correction is
possible in-sim: the executor serves only `orbit.parent == physics.dominant`" (lib.rs:17904–17910).
Meanwhile the map panel stages `departure_node` verbatim on "Create node" (map.rs:1242–1251) and
displays the IDEAL conic's `Enter{duva}` encounter (periapsis/v∞) as the plan's outcome row
(map.rs:1101–1110, csp-ui lib.rs PlanEncounterRow). Probe (panel query, 200 km LEO, park default):
plan depart_tick 1,132,669,320, ideal conic a=1.76e10/e=0.227 (center-hit on Duva); staged node
burns 2,125 s before the window; real egress 20.0 h AFTER the modeled instant departure; the real
post-egress solen conic (a=1.78e10/e=0.237) misses Duva by **9.44e8 m at arrival_tick (≈20× Duva's
4.79e7 m SOI), closest approach 1.77e8 m = 3.7 SOI radii, and produces no SOI crossing at all**
before the horizon — `Enter{duva}` never fires. The dominant error is the instant-egress
assumption: Terrin carries the vessel ~2.8° around its orbit during the ~20 h real egress. **Impact**:
a player who plans Terrin→Duva in the map panel, stages the node, and lets the executor fly it
lands on a completely different heliocentric orbit with no way to correct after egress; the M5
"Duva reachable" outcome is unreachable through the player surface (only via the test's private
egress solve). The vessel's own predicted crossing shown on the map is single-level
(map.rs:1687 — the post-burn hyperbola's first crossing is `Exit{terrin}`), so the miss is not
even visible as it develops. **Suggestion**: make the planner compose with the egress transit —
either (a) lift the e2e test's approach into `plan_transfer`/`departure_node`: propagate the
post-burn hyperbola to the SOI egress, rebase, and aim the Lambert leg from the egress-composed
state (fixed 2–3 deterministic iterations), or (b) at minimum offset the porkchop departure epoch
by an egress-transit estimate. Independently, surface the live vessel's chained (post-egress)
encounter prediction in the map/panel so the miss is player-visible pre-commit.

### [MEDIUM] Finding 2: The transfer-window alarm fires at `departure_tick`, but the burn tick is always at-or-before it — "wait for the window, then stage" is a trap
**File**: crates/csp-sim/src/transfer.rs:1604 — **Problem**: `departure_node` ticks the burn at the
vessel's *last parking-orbit passage through the burn longitude at-or-before* `departure_tick`
(transfer.rs:1604–1621; the module's own test asserts `node.tick <= plan.departure_tick`,
transfer.rs:2359). The panel's `WatchWindow`/window alarm targets `plan.departure_tick`
(map.rs:1188–1207), i.e. up to one parking period (≈40 min for the 200 km LEO fixture; 35 min in the
probe) *after* the only burn pass the plan designs. **Evidence**: a player who arms the M5 window
watch and creates the node when it fires gets `ejection: burn precedes now` (transfer.rs:1631) —
`CreateNode` re-resolves at the current clock and refuses — and must re-run FindWindow for a later
window. **Impact**: the flagship M5 button's natural usage pattern dead-ends at the window; only
the plan-first-node-immediately flow works (then `ManeuverNodeStart` fires the burn automatically
under warp). **Suggestion**: when a staged node exists, target the window watch at the node's tick
(the panel already has `ArmNodeAlarm`), and/or surface "burn passes N min before the window" on the
plan row; alternatively make `departure_node` fall forward to the *next* passage when the last one
is behind `now` (a plan queried at the window is still flyable one parking period later).

### [MEDIUM] Finding 3: Transfer rate is one global constant, not §3.5's `min(source_outflow, dest_inflow)` — and concurrent streams each get the full rate
**File**: crates/csp-sim/src/transfers.rs:259 — **Problem**: PLAN §3.5 verbatim: "rate =
`min(source_outflow, dest_inflow)` from the connected ports". The built model has a single
`transfer_rate_units_per_s` (transfers.rs:242–253, `DEFAULT_TRANSFER_RATE_PER_S = 10`); no port flow
stats exist in the data model, and N concurrent transfers between the same pair each run at the
full rate (no shared-port contention — `dur_ticks` is computed per transfer,
transfers.rs:890–892/1502–1504). The module documents this as a "v1 simplification"
(transfers.rs:108–119). **Impact**: §6.4 item 4's sanctioned formulation ("proximity + confirm + a
rate") is met, but the §3.5 per-port clause is not, and nothing in PLAN sanctions dropping it
further; gameplay-wise a player piping two resources simultaneously gets 2× the throughput of one,
which the spec's model would not allow. **Suggestion**: add an `outflow_units_per_s` field to the
tank/part spec (data model) and take the min across the endpoint pair, or at minimum divide the
global rate among concurrent transfers sharing an endpoint.

### [LOW] Finding 4 (DRY): the transfer commit block is copy-pasted four times
**File**: crates/csp-sim/src/transfers.rs:885 — **Problem**: id allocation + counter-overflow check
+ `complete_tick` clamping (`.saturating_add(dur.max(1)).max(clock+1)`) + `ActiveTransfer` insert +
`schedule_event(TransferComplete)` appear verbatim in `apply_transfer_resource` (885–916),
`apply_transfer_crew` (969–993), `apply_transfer_colony_resource` (1497–1532), and
`apply_transfer_colony_crew` (1601–1631). **Impact**: four sites to keep in sync for what is one
lifecycle rule; a future change to the clamping or pairing rule can drift per-verb.
**Suggestion**: extract `fn commit_transfer(&mut self, from: TransferEnd, to: TransferEnd, cargo:
TransferCargo, dur_ticks: u64, shadow: &mut TickShadow) -> Result<(), SimError>` housing the
counter, clamp, insert, and schedule; each apply then only computes `dur_ticks` and calls it.

## Verified-sound (no findings)
- Lambert solver core: branch selection (Short/Long both evaluated per cell, cheapest wins),
  zero-rev bracketing (hyperbolic expansion + elliptic pole), multi-rev golden-section branch
  minimum + two-root validation, `lands_on` propagation check + `refine_landing_z` polish — all
  with fixed iteration caps (64/64/16/512) and libm-only math; `lambert_curtis_example_5_2`,
  long-way broad sweep (256 cells), multi-rev, bitwise-determinism tests all green.
- Porkchop grid: bounded (65,536-cell cap, revs cap 15), inclusive endpoints, strict-local-minima
  walk honest about edges, refinement is a bounded deterministic pattern search on the tick
  lattice with i128 clamping; validation rejects degenerate/reversed/over-cap queries.
- Transfer-process lifecycle beyond C04's verified poison case: colony Abandoned/Dormant
  mid-flight (frozen-store credit, contract facts gated on `colony_live`), latch-merge remap
  (incl. the `from == to` post-merge guard in `land_crew`), breakup/jettison partition
  (`split_vessel_records`), double-completion (record consumed at fire; stale id no-op; restore
  pairing), degraded-fire zombie-drop (lib.rs:8257–8260), capacity/seat reservations net of
  in-flight inbound. `craft: None` vessels cannot be transfer endpoints (link clauses all require
  a physical residence).
- Determinism/hash: `transfer_cfg`, `next_transfer_id`, `next_crew_id`, transfers arena (ids +
  payloads, ascending), `tank_contents`, `crew`, `joined_colony` all hashed (lib.rs:8794–8830,
  11764–11788); `debit`/`credit`/`choose_seat` iterate `BTreeMap` node order; `dur_ticks` is
  exact-arithmetic `ceil`. All applies shadow before mutate (rollback discipline).
- Wiring: every transfer SimCommand has a production issuer (lint-reach); vessel↔colony full loop
  (Join gate → TransferColonyResource → TransferComplete credit + `ColonyDelivery` fact →
  contract trigger) traceable; `WatchTransferWindow` → `Clamping` `TransferWindow` event wired
  with alarm-book mirror.

## PRD coverage table
| Requirement (PLAN) | Status | Evidence |
|---|---|---|
| §2.11 Transfer job: Lambert + porkchop + local refinement → maneuver nodes | **WIRED** (accuracy caveat F1) | transfer.rs solver suite; map.rs:1071–1072, 1242–1251 |
| M5 transfer-window events; §4 Kerbal Alarm Clock row | **WIRED** (timing caveat F2) | `WatchTransferWindow` lib.rs:5760–5783; button map.rs:1202 |
| M5 encounter prediction | **WIRED** (single-level for live vessel) | predict_encounter transfer.rs:1360; panel row; map marks |
| M5 "Duva reachable" in-sim | **PARTIAL** — proven only via a test that bypasses the player path | lib.rs:18269 + 17904–17910; probe: 9.4e8 m miss via panel flow (F1) |
| §3.5 `ResourceTransfer` process (survives warp, event-queued) | **WIRED** | transfers.rs debit-at-apply + `TransferComplete` (`Clamping`) |
| §3.5 rate = min(source_outflow, dest_inflow) | **DEGRADED** — single global rate (F3) | transfers.rs:242–259, 108–119 |
| §3.5 `JoinColonyInventory` opt-in (never automatic) | **WIRED** | transfers.rs:1304–1340; unlatch clears (lib.rs:3910–3912) |
| §3.5/M6-D crew transfer (docked vessels; colony both ways) | **WIRED** — link via proximity since docking UI is cut (§6.4 note only) | transfers.rs:923–994, 1542–1631; program.rs:611 |
| Determinism: fixed iteration counts, hash/serialization coverage | **WIRED** | LAMBERT_MAX_ITERS etc.; state_hash lib.rs:8794–8830 |

## Top 3 issues
1. **F1 (HIGH)** — the map panel's staged Terrin→Duva burn misses Duva by ~20 SOI radii with no
   in-game correction path; the panel presents the ideal encounter as the outcome.
2. **F2 (MEDIUM)** — the M5 window watch fires after the only burn pass the plan designs;
   "wait for window → create node" dead-ends in a refusal.
3. **F3 (MEDIUM)** — one global transfer rate instead of §3.5's per-port
   `min(source_outflow, dest_inflow)`; concurrent streams multiply throughput.
