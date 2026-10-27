# C07 verification — reviewer report adjudicated against source + an independent empirical probe

Verification context: report `review-report-C07.md` (transfer.rs + transfers.rs + wiring). Method:
(1) every file:line citation opened and read in context; (2) an **independent probe** rebuilt the map
panel's exact flow outside the repo (public APIs only: `csp_data::load_system(data/systems/home)` →
`PatchedConics::from_body_defs` → map.rs `porkchop_query` shape verbatim → `plan_transfer` →
`departure_node` → impulsive burn at the node tick → chained real SOI egress via
`predict_encounter`/rebase → miss distances against Duva's live ephemeris); (3) the relevant test
suites re-run (`cargo test -p csp-sim transfer::` = 21 pass; `transfer` filter = 37 pass;
`duva_reachable_in_sim_end_to_end` = 1 pass). No file in the project was edited; probe lived in
/tmp and was deleted.

---

### Finding 1: The staged transfer plan's burn cannot reach its destination — CONFIRMED

**Original summary**: `departure_node`'s burn, flown exactly, misses Duva by ~9.4e8 m (≈20× SOI);
no in-sim correction; panel shows the ideal conic's encounter as the outcome.

**Verification — code.**
- transfer.rs:1529–1537: module doc verbatim "**Limitation — this node does NOT compose to arrival
  on its own.** … The correction loop is REQUIRED, not optional". No correction loop exists: grep
  over transfer.rs/csp-view shows `plan_transfer`/`departure_node` are called once each in the panel
  and never iterated.
- The "no correction possible in-sim" claim re-derived from the gates:
  - `physics.dominant` is a per-world single bubble body, set once at world construction
    (lib.rs:2673–2675) and **never reassigned anywhere in the workspace** (grep for `.dominant =`
  has zero non-test hits).
  - After `Exit{terrin}`, the vessel re-parents to solen. `apply_set_maneuver` refuses
    `orbit.parent != dominant` (lib.rs:6652–6655), `fire_maneuver_start` re-checks the same
    (lib.rs:6779–6782), and `apply_to_bubble` (manual flight) also refuses (lib.rs:6531–6534). So
    post-egress the player can neither schedule a correction node nor take the stick — the claim
    holds exactly. (Precision note: a *second* node on the terrin egress hyperbola pre-egress is
    mechanically allowed — `SetManeuver` has no closed-orbit gate — but no in-game surface computes
    such a correction and the map shows no post-egress prediction, so this is not a player-reachable
    correction path; the module's own doc and the M5 test's comment both state the stronger claim.)
  - maneuver.rs:37–43 documents the dominant-parent-only executor and the re-parent degradation
    rule, as quoted.
- The M5 e2e test bypasses `departure_node` exactly as reported: lib.rs:17904–17910 ("no correction
  is possible in-sim: the executor serves only `orbit.parent == physics.dominant`"); the burn comes
  from `solve_egress_hyperbola` (lib.rs:18017) / `design_duva_injection` (lib.rs:18140), both inside
  `mod tests` (lib.rs:11885) — test-only, unreachable from any player surface.
- Panel display of the ideal outcome: map.rs:1101–1110 stores
  `predict_encounter_excluding(model, &plan.conic, …, Some(&plan.origin))` as the plan's encounter;
  map.rs:968–975 copies it into `PlanEncounterRow`; csp-ui lib.rs:1659–1672 renders it as the plan's
  outcome line ("enters duva T−… · Pe … · v∞ …", impact-colored when `impacts`).
- Single-level live prediction: map.rs:1687 is one un-chained `predict_encounter` call on the live
  conic (first crossing only — post-burn that is `Exit{terrin}`, never the solen-leg encounter).

**Verification — empirical (independent probe, panel flow, 200 km LEO, park 8.0e5 m).**
Reproduced the report's numbers to stated precision:
- plan depart_tick **1,132,669,320** (exact match); ideal conic a=1.7593e10 / e=0.2270; panel
  encounter row: `duva Enter`, periapsis ≈ 0.0037 m (`impacts=true` — a sold center-hit).
- staged node tick = window − **2,125 s** (exact); node |dv| 1057.1 m/s vs priced bound 1034.4.
- real `Exit{terrin}` at depart + **20.05 h**; Terrin sweeps **2.82°** around solen in that lag
  (report: ~20 h / ~2.8°).
- real post-egress solen conic a=**1.7837e10** / e=**0.2374** (report: 1.78e10 / 0.237).
- miss at arrival_tick **9.4420e8 m = 19.7 Duva SOI radii** (SOI 4.79e7 m, data-confirmed); closest
  approach **1.7710e8 m = 3.70 SOI radii**, 46.6 sim-days before arrival; **no SOI crossing at all**
  on the real conic before the horizon → `Enter{duva}` never fires.
- What the player experiences: plan row promises a center-hit Duva encounter; the flown burn puts
  them on a different heliocentric orbit that misses Duva by 3.7 SOI radii at closest, with no
  encounter mark, no correctable node (gates above), and no in-map visibility of the developing
  miss. The M5 "Duva reachable" outcome is reachable only through the test's private solve.

Severity HIGH is justified (flagship player flow sells an outcome it structurally cannot deliver;
documented as required-but-missing correction loop). Suggestion (a) matches the test's own
egress-composed approach and is the right shape.

### Finding 2: Window alarm fires at `departure_tick`, but the burn tick is at-or-before it — CONFIRMED

**Original summary**: "wait for the window, then create the node" dead-ends in `ejection: burn
precedes now`.

**Verification — code.**
- transfer.rs:1602–1625: burn tick = **last** burn-longitude passage at-or-before `departure_tick`
  (`burn_s = dep_s + dm/n_v − period_s`), so `burn_tick <= departure_tick` always — asserted by the
  module's own test at transfer.rs:2359 (`node.tick <= plan.departure_tick`).
- transfer.rs:1631–1633: `burn_tick <= now` → `Recoverable("ejection: burn precedes now")`.
- map.rs:1188–1207: `WatchWindow`/window alarm targets `plan.departure_tick`;
  `apply_watch_window` (lib.rs:5760–5783) schedules the Clamping `TransferWindow` event at that tick.
- map.rs:1225–1257: `CreateNode` re-resolves `departure_node(model, orbit, plan, now)` at the
  current clock. When the alarm has fired, `now >= departure_tick >= burn_tick` → refusal
  **always** (including the dm==0 edge: burn==window==now still fails `<=`).
- Recovery is only re-running FindWindow (its `depart_lo` is now+1 sim-day, map.rs:1015).

**Verification — empirical.** The probe called `departure_node` at `now = departure_tick` and at
`departure_tick + 1`: both return "REFUSED: ejection: burn precedes now". Parking period for the
200 km LEO is 2392 s (39.9 min); the staged node sits 2125 s (35.4 min) before the window — matching
the report's ≈40 min / 35 min figures. The only working flow is plan→node immediately (then the
Clamping `ManeuverNodeStart` fires the burn automatically, maneuver.rs:15–18). Confirmed MEDIUM.

### Finding 3: One global transfer rate instead of §3.5's per-port min; concurrent streams each get full rate — CONFIRMED

**Original summary**: spec says `min(source_outflow, dest_inflow)`; the model has a single
`transfer_rate_units_per_s` and N concurrent transfers each run at the full rate.

**Verification.**
- PLAN.md §3.5 (line 1217) verbatim: "rate = `min(source_outflow, dest_inflow)` from the connected
  ports; publishes `TransferComplete` (so it survives warp)". Confirmed quoted accurately.
- transfers.rs:242–253: `TransferConfig` carries one `rate_units_per_s`;
  `DEFAULT_TRANSFER_RATE_PER_S = 10.0` (transfers.rs:259); `data/balance.ron:41` mirrors 10.0.
- No port flow stats in the data model: grep for `outflow`/`inflow` across `csp-data` hits only doc
  comments (balance.rs:122–126: "v1 has no per-port flow stats, so a single rate stands in").
- No contention: `dur_ticks = ceil(amount·TICK_HZ / rate)` is computed per transfer with no
  summation over concurrent transfers sharing an endpoint (transfers.rs:890–892 and 1502–1504);
  the only cross-transfer accounting is capacity/seat reservation netting inbound
  (transfers.rs:866–871, 962–967). Two simultaneous streams therefore each move at 10 u/s.
- The deviation is self-documented as a v1 simplification (transfers.rs:108–119). PLAN.md is
  normative (AGENTS.md), and §6.4's sanctioned v1 clause ("proximity + confirm + a rate") does not
  name the per-port min, so "DEGRADED, unsanctioned detail drop" is a fair adjudication. Confirmed
  MEDIUM (spec-compliance gap with a real, if modest, gameplay effect).

### Finding 4 (DRY): the transfer commit block copy-pasted four times — CONFIRMED

**Original summary**: id allocation + overflow check + complete_tick clamp + insert +
`schedule_event(TransferComplete)` appear verbatim at four apply sites.

**Verification.** All four blocks read and compared — identical lifecycle code, only the
from/to/cargo/duration inputs differ:
- `apply_transfer_resource` transfers.rs:885–916 (id 885, overflow 886–889, dur 890–892, clamp
  893–897, insert 898–910, schedule 911–915);
- `apply_transfer_crew` 969–993 (crew delay clamp 974–978);
- `apply_transfer_colony_resource` 1497–1532 (dur 1502–1504, clamp 1505–1509);
- `apply_transfer_colony_crew` 1601–1631 (clamp 1606–1610).
Confirmed LOW/DRY; the suggested `commit_transfer` extraction is sound.

---

## Verified-sound spot-checks (2 instructed, both hold)

1. **Lambert branch selection + caps**: `leg_at` (transfer.rs:919–979) evaluates `Short` and `Long`
   per cell (line 957) plus every rev branch 1..=max_revs, keeping the strictly cheapest
   `total_dv_mps` (line 943); per-cell solver `Err` is treated as infeasible-cell, not scan failure.
   Caps confirmed: `PORKCHOP_MAX_CELLS = 65_536` (95), `LAMBERT_MAX_REVS = 15` (103),
   `LAMBERT_MAX_ITERS = 64` (83), `EJECTION_MAX_ITERS = 16` (90); validation rejects reversed
   ranges, sub-lattice cell counts, over-cap grids (1016–1077); the strict-local-minima walk is
   edge-honest (1110–1138). No defect found.
2. **Transfer lifecycle beyond the poison case**: latch-merge remap `remap_transfer_vessels`
   (transfers.rs:1668–1681) can produce `from == to`, and `land_crew`'s vessel→vessel arm guards it
   (`f != t`, transfers.rs:1163); breakup partition `split_vessel_records` (1686–1708) splits
   tanks/crew by fragment node set with the caller retaining the rest; colony abandonment mid-flight
   credits frozen stores with recompute gated on `colony_live` (credit_end, 1127–1144). Hash
   coverage re-confirmed: `transfer_cfg` fields (lib.rs:8797–8799), `next_crew_id`/`next_transfer_id`
   (8815–8816), transfers arena (8817–8819), and per-vessel `tank_contents`/`crew`/`joined_colony`
   (lib.rs:11762–11790). No defect found.

## PRD rows — verified

- **M5 "Duva reachable" = PARTIAL**: correct. The e2e test (lib.rs:18269 `duva_reachable_in_sim_end_to_end`)
  does drive the real chain (SetManeuver → executor → Exit{terrin} → Enter{duva}), so PLAN §6's M5
  row is literally satisfied — but its burn is produced by `solve_egress_hyperbola`/
  `design_duva_injection` inside `#[cfg(test)] mod tests` (lib.rs:11885), and the probe shows the
  player path (panel → staged node → flown burn) misses by 9.44e8 m with no correction verb. The
  row's caveat is the honest reading.
- §2.11 planner WIRED-with-F1-caveat (map.rs:1071–1072, 1242–1251 confirmed), M5 window events
  (lib.rs:5760–5783; button arm map.rs:1188–1207) with F2 caveat, encounter prediction WIRED
  (transfer.rs:1360; single-level for live vessels per map.rs:1687), §3.5 process/JoinColony/crew
  rows (transfers.rs:1304–1340 gate is opt-in and liveness-checked; unlatch clears the join,
  lib.rs:3910–3912), rate row DEGRADED per F3 — all citations check out.

## Nits (non-findings)

- Report preamble says "38 transfer-process tests in lib.rs"; `cargo test -- --list` counts 13
  lib.rs tests matching "transfer" (16 with crew/colony-delivery patterns) + 21 module tests. All
  pass either way; count is filter noise, no claim depends on it.

## Verdict

4/4 findings stand: 1 HIGH (confirmed empirically to stated precision, miss mechanism re-derived
from the executor gates), 2 MEDIUM (F2 confirmed analytically + empirically; F3 confirmed against
PLAN §3.5 verbatim), 1 LOW (confirmed at exact lines). REQUEST CHANGES on F1/F2; F3/F4 as stated.
