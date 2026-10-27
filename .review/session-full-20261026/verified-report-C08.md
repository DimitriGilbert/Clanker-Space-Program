# Verified report C08 — Progression: science / tech / economy / contracts

Verification agent pass over `review-report-C08.md` (session `session-full-20261026`,
tree at HEAD 7dd4b86). Every finding was re-checked against the actual source at the
cited lines plus surrounding context; the three HIGHs were deep-traced end to end. The
report's test claim was reproduced: `cargo test -p csp-sim --lib -- research:: tech::
economy:: contracts::` → **38 passed; 0 failed**.

Score: **9 confirmed, 1 dismissed** (Finding 9 — its math is wrong in direction and
magnitude). The PRD coverage table and the completability section were spot-checked
row by row and are accurate.

---

### Finding 1: `RecoverVessel` silently destroys a running lab project — CONFIRMED
**Original**: recovery banks only pooled science, then `remove_vessel` →
`drop_vessel_projects` loses the escrowed data and accrued science with no accrual,
notification, or refund.

**Verification** — every link in the chain holds in the source:
- `apply_recover_vessel` (crates/csp-sim/src/economy.rs:880-1010) banks exactly
  `v.science.data` (loop at economy.rs:934-958) and then calls
  `self.remove_vessel(vessel, shadow)` at economy.rs:1000.
- The escrow is invisible to that loop by construction: `apply_start_research`
  moves the pooled units at the key OUT of `v.science.data` into the project
  (research.rs:1055-1063: `m.remove(key)` + `retain`), so a running project's
  escrow never appears in `v.science.data`.
- `remove_vessel` (lib.rs:4997) unconditionally calls
  `research::drop_vessel_projects` (lib.rs:5009-5015), whose comment states the
  behavior outright: "work-in-progress is lost, no accrual"
  (research.rs:565-597 — `projects.retain(|_, p| p.vessel != vessel)`, no
  `finalize_project`, no remnant return, no notification).
- The intended contrast is real: `apply_stop_research` (research.rs:1084-1130) →
  `finalize_project` (research.rs:506-531) banks the accrual AND returns the
  unconverted remainder to the pool; `fire_research_exhausted`
  (research.rs:1135-1167) banks `η·D0`.
- `StartResearch` has no vessel-state/residence restriction (research.rs:955-1079
  checks config, vessel/craft/lab-node existence, researchers aboard, slot count,
  duplicate lab, key-closed, `d0 > ε`, `T > 0` — nothing about `SurfaceLatched`),
  so a project is routinely live in exactly the state `RecoverVessel` requires.
- No guard exists view-side either: `C::RecoverVessel` maps straight to the
  command (csp-view/src/lib.rs:1051-1060), and the "parked build" pad-clear even
  auto-enqueues `RecoverVessel` for the occupying vessel
  (csp-view/src/lib.rs:2969-2978) — a lab vessel parked on the pad can lose its
  project without the player ever pressing "recover".
- UI copy: program.rs:2131-2137 — stop button hover "bank the accrued science and
  release the rest"; program.rs:590-591 — "RecoverVessel (site credit + part
  payout + crew home + pooled science banks)". The player expectation that lab
  work is bankable is set by the first and not contradicted anywhere on the
  recovery surface.
- Magnitude check: Terrin cap 30 units (data/experiments/exp.materials_study.ron),
  `lab_science_per_data: 1.0`, `science_funds_per_point: 10.0` (balance.ron) →
  30 science = ₣300 milestone lost, plus accrued points, silently.

Verdict: real HIGH. Player loses escrow + accrual on a *successful* recovery action
the UI frames as banking.

### Finding 2: shipped default salary blows the §3.5 "2–5% per sim-month" band; knob lives outside balance.ron — CONFIRMED
**Original**: `HIRE_SALARY_PER_S = 0.02` (view-only hardcode) is 5.18%/month of the
starting balance for one crew; the §8.3 harness solves its own rate so it cannot
catch this.

**Verification**:
- Rate: crates/csp-view/src/lib.rs:1294 `const HIRE_SALARY_PER_S: f64 = 0.02;`
  with a doc comment admitting "the sim has no salary table". Consumed at
  csp-view/src/lib.rs:1084 (`salary_per_s: mirror.input.hire_salary_per_s`) fed
  from lib.rs:3589. Grep over the tree: the ONLY non-test producers of
  `SimCommand::HireCrew` are the view and `#[cfg(test)]` fixtures
  (colonies.rs tests start :2461, life_support.rs :425) — so ₣0.02/s is the
  shipped default, full stop.
- balance.ron line 1: "Global balance constants. ALL tuning lives here (PLAN §8.3)"
  — and there is no salary-rate field (only `salary_period_days: 30`). The sim's
  `apply_hire_crew` takes `salary_per_s` as a parameter with no default
  (economy.rs:813-852).
- Arithmetic: one sim-month = 30 × 86 400 s = 2 592 000 s → 0.02 × 2 592 000 =
  **₣51 840/month per crew**. `starting_funds_micros: 1_000_000_000_000` = ₣1 M →
  **5.184% for a single hire**, above the 5% ceiling PLAN:1196-1200 sets for a
  *fully-staffed* base ("a fully-staffed base with no active missions loses 2–5%
  of liquid funds per sim-month at default difficulty"). The drain is absolute,
  so as liquid funds shrink the fraction only grows. 20 crew ≈ ₣1 036 800/month ≈
  104% of starting funds (≈115% of the ₣900 k post-hire balance) — the report's
  "~104%" checks out. One scientist: ₣1 M / ₣51 840 ≈ 19.3 idle months to
  insolvency — the report's "~19" checks out.
- Harness: `staffed_base_run` *solves* the salary from `drain_fraction`
  (csp-bench/src/economy.rs:89-97:
  `total_salary = drain_fraction * funds_after_hire / MICROS_PER_FUND / period_s`)
  and `staffed_base_drains_in_band` (economy.rs:371-401) asserts the band on that
  solved rate (`drain_fraction: 0.022`), never on the shipped `HIRE_SALARY_PER_S`.
  The module doc says so explicitly ("The staff's `salary_per_s` total is *solved*
  for a target drain rate"). The §8.3 gate therefore cannot detect the shipped
  default being out of band — confirmed. There is also no difficulty knob anywhere
  that would make "default difficulty" mean something other than what the UI pays.

Verdict: real HIGH (balance + PRD-integrity).

### Finding 3: §3.5 insolvency consequences missing — no `StaffAttrition`, no `FacilityDormant`, no facility upkeep — CONFIRMED
**Original**: PLAN:1202 promises attrition/dormant events and facility upkeep; the
code implements only the clamp + `Insolvent` Clamping event, so bankruptcy is a
notification with no gameplay consequence.

**Verification**:
- PLAN.md:1201-1204, exact language: "funds **clamp at zero**; unpaid salaries
  trigger `StaffAttrition` and `FacilityDormant` events; an `Insolvent` state is a
  **`Clamping`** event that auto-demotes warp to ×1 and notifies. Negative funds
  never exist." (The 2–5% band sentence is PLAN:1196-1200, quoted under Finding 2.)
- Grep across `crates/` + `data/`: `StaffAttrition` / `FacilityDormant` have ZERO
  code hits — only PLAN.md:1202 and a docs/SIMULATION.md:504 deviation note ("the
  code has only `SalaryDue` + `Insolvent` … no attrition event exists"). The docs
  note acknowledges the gap; it does not implement it.
- Grep "upkeep": one hit, the csp-bench/src/economy.rs:9-10 comment deferring
  facilities/upkeep to "M8's colony/economy block" — which shipped (balance.ron
  carries the M8-B manufacture and M8-C life-support blocks) without any upkeep
  drain. No facility arena exists (`apply_hire_crew`'s own doc: "facilities land
  with M8's economy" — they didn't).
- What insolvency does today: `charge_funds` (economy.rs:699-721) clamps, sets
  `insolvent`, schedules `Insolvent` (Clamping class confirmed at events.rs:499);
  the `Insolvent` event's firing is an explicit no-op state-wise
  ("mutates nothing; surfacing rides `fired_events`", lib.rs:8452-8455);
  `fire_salary_due` (economy.rs:773-805) clamps, re-arms, and nothing else ever
  happens. `CrewStatus::Retired` has no setter anywhere (grep: only the enum def
  at economy.rs:253 and its doc) — crew stay `Active` forever at zero funds.
- §6.4's cut list (reordered: render tier, life support, Duva, orbital docking,
  web tier) does not sanction dropping attrition/dormant/upkeep.

Verdict: real HIGH (PRD DEGRADED/MISSING; bankruptcy loop hollow).

### Finding 4: survey contracts can expire (penalty) with the objective already met — CONFIRMED
**Original**: lab conversion raises the survey predicate continuously, but neither
stop nor exhaustion fires `DataBanked`, so an already-satisfied contract sits
`active` until its deadline penalizes it.

**Verification**:
- `survey_returned_units` (contracts.rs:1031-1060) subtracts the LIVE escrow
  projection `data_at(now)` per project (contracts.rs:1042-1057), so its value
  rises continuously as the lab converts, and after exhaustion/stop (project gone,
  pool empty) it counts the full `collected`.
- All nine `contract_notify` call sites enumerated: lib.rs:4863 (SurfaceLatched),
  lib.rs:5872 + 6340 (OrbitAchieved), lib.rs:8567, economy.rs:1005/1009
  (VesselRecovered + DataBanked from recover-vessel), research.rs:846 (transmit),
  research.rs:946 (recover-data), transfers.rs:1082 (ColonyDelivery).
  `apply_stop_research` (research.rs:1084-1130) and `fire_research_exhausted`
  (research.rs:1135-1167) contain NO `contract_notify` — confirmed.
- `fire_contract_deadline` (contracts.rs:1169-1211) charges the authored penalty
  unconditionally — there is no `objective_met` check before the penalty, the
  failed count, or the `ContractFailed` notification.
- Authored data: contract.survey.data.ron — `min_units: 5.0`,
  `deadline_days: Some(120.0)`, `failure_penalty_funds: 2_000.0`. Scenario timing
  checks out: shipped lab T = 0.5 data/s, R ≥ 2 science/s (skill floored to 1 ×
  multiplier 2.0, data/parts/csp.lab.mk1.ron), η = 1 → a 5-unit escrow exhausts in
  ~5.5 s. (Caveat retained from the report: completion IS evaluated on any other
  trigger — the penalty scenario needs a quiet program, e.g. warping a lone lab
  vessel; that is the realistic case.)

Verdict: real MEDIUM — predicate/state and event triggers disagree; the exact
§3.6 violation class.

### Finding 5: offer board has no decline/expiry — jam with no player recourse — CONFIRMED
**Original**: offers leave the board only via `AcceptContract`; unsatisfiable draws
accumulate and freeze the 6 slots; the instant-complete "faucet self-starves" bound
is inverted in practice.

**Verification**:
- `publish_contracts` returns early while `offers.len() >= board_size`
  (contracts.rs:735-738); the ONLY removal is `offers.remove(&offer)` at
  contracts.rs:897 inside the accept path; grep for Decline/cancel/expire verbs
  across crates: zero hits (the single SimCommand contract verb is
  `AcceptContract`, lib.rs:951). No staleness sweep exists.
- Prereq filtering happens at generation (contracts.rs:680
  `contract_prereqs_met`), but prereqs are tech + completed-count facts — the
  BODY draw is unfiltered: rescue draws `["terrin","rask"]`, survey.data
  `["terrin","rask","duva"]`, colony_resupply `["terrin","duva","rask"]`. A
  mid-game player (tech.rocketry.3, 1 satellite done) becomes eligible for rask
  rescues they cannot fly; those offers jam slots indefinitely.
- Satellite band math verified: `periapsis_alt_m (80k, 250k)`,
  `apoapsis_alt_m (150k, 1.5M)` (contract.satellite.ron) → a parked 150 km
  circular orbit satisfies a draw iff drawn min-peri ≤ 150k: P = 70/170 ≈ 41.2%
  (apo cap ≥ 150k always). Satisfiable draws are consumed at
  ₣8k advance + ₣32k completion = ₣40k per accept. So the board's end state is
  dominated by unsatisfiable entries — the doc's "faucet self-starves" bound
  (contracts.rs:802-811) describes the same jam the report flags.
- Nuance (does not change the verdict): "frozen forever" is mildly overstated —
  the player can un-jam by eventually acquiring the capability to complete a
  jammed offer. With zero funds and no in-flight vessel (the report's deadlock
  shape) there is indeed no in-game recourse.

Verdict: real MEDIUM.

### Finding 6: `unlocks.facilities` / `unlocks.contract_types` are dead data — CONFIRMED
**Original**: §3.3's unlock semantics for facilities and contract types are
hand-duplicated in contract RON prereqs; the tree fields gate nothing.

**Verification**:
- data/techtree/home.ron authors all four nodes' `facilities` and
  `contract_types` (e.g. rocketry.2 → `contract_types: ["contract.satellite"]`).
- Consumers: csp-data validate.rs:3776-3779 and :4235-4238 resolve the ids to
  authored facilities/templates (resolution only — no agreement check against the
  contract RONs); the type/field definitions in csp-data ids.rs/techtree.rs; and
  csp-sim tech.rs's ONLY uses are inside `#[cfg(test)]` (mod tests at tech.rs:402;
  fixtures at tech.rs:423, 431-434). Zero sim/UI gating reads either field.
- Actual gating is the duplicated prerequisite
  `TechResearched("tech.rocketry.2")` inside contract.satellite.ron — two sources
  of truth that the validator does not cross-check (and they already differ in
  strength: the RON additionally requires a completed SurveyRun).
- `facility.lab` / `facility.test_stand` gate nothing; the lab part is gated only
  by its own `tech_requirement: "tech.science.1"` (csp.lab.mk1.ron).

Verdict: real MEDIUM (PRD §3.3 DEGRADED; silent-drift risk).

### Finding 7: every hire is skill 0; view floors it; claimed skill unvalidated — CONFIRMED
**Original**: `apply_hire_crew` hardcodes `skill: 0`; the view floors to ≥1 so
`R = Σ skill · mult` is a constant; `StartResearch` trusts the command's skill.

**Verification**:
- economy.rs:839: `skill: 0` hardcoded; `SimCommand::HireCrew` carries name/kind/
  salary only — no skill. Roster panel prints it raw: program.rs:1116-1119
  ("… · skill {} · …").
- csp-view/src/lib.rs:4987-4993:
  `let skill = sim.economy().crew.get(&c.id).map_or(1, |m| m.skill).max(1);` —
  a hired skill-0 member and an id-only `BoardCrew` member (absent from the
  roster → `map_or(1)`, draws no salary, costs no ₣5 000 hire) both research at
  effective skill 1 × multiplier 2.0. Crew-kind choice has zero progression
  meaning — confirmed.
- `apply_start_research` (research.rs:993-1006) validates only `skill ≤ 5`,
  uniqueness, and aboard-ness — it never cross-checks the claimed skill against
  the roster member's recorded skill. research.rs:86-89's module doc admits "the
  command value is the logged truth until then".

Verdict: real MEDIUM (§3.2 lab-model gap).

### Finding 8: DRY — four heap-rebuild copies and duplicated banking/fraction logic — CONFIRMED
**Original**: (a) four hand-rolled `mem::take(heap) → filter → collect` scrubs;
(b) banked-science block duplicated between transmit and recover; (c) closable /
`1 − transmit_ratio` logic duplicated with a shape divergence.

**Verification** — all cited sites reproduced:
- (a) research.rs:585-596 (`drop_vessel_projects`), research.rs:1117-1128
  (`apply_stop_research`), research.rs:1222-1233 (`finalize_projects_on_nodes`,
  inside a per-project loop), contracts.rs:1143-1148 (`complete_contract`) —
  four independent copies of the same rebuild pattern.
- (b) research.rs:823-846 (`apply_transmit_data`) vs research.rs:926-946
  (`apply_recover_data`): identical `banked += …; credit_science_payout; notify(
  ScienceBanked); contract_notify(DataBanked)` blocks.
- (c) research.rs:897-915 vs economy.rs:940-956: same other-vessel/live-project
  closable check and transmitted-fraction logic, with the stated divergence
  present — economy.rs:946-947 uses `.map_or(0.0, |d| (1.0 -
  d.transmit_ratio).max(0.0))` for a missing experiment def (banks 0 fraction)
  while research.rs:880-884 rejects the same missing def with a typed
  `Recoverable`.

Verdict: real MEDIUM (maintainability; divergence (c) already observable).

### Finding 9: `exhaust_seconds` mis-predicts for `T < ε` and over-banks at +1 tick — DISMISSED
**Original**: with `0 < T < ε` the negative log term makes the predicted
exhaustion precede the true crossing, `.max(1)` clamps to +1 tick, and
`fire_research_exhausted` over-banks the whole escrow instantly.

**Verification** — the math does not hold. Let `η`, `ε = data_epsilon (1e-6)`,
`R`, `T < ε`, `d0 > ε` (the start gate, research.rs:1025):
- Formula: `t_f = η(d0−T)/R + (ηT/R)·ln(T/ε)`. The true all-linear ε-crossing
  (linear because `ε > T` means D reaches ε before T) is
  `t_true = η(d0−ε)/R`. The difference is
  `t_f − t_true = (η/R)·[(ε−T) + T·ln(T/ε)] = (ηε/R)·[(1−x) + x·ln x]` with
  `x = T/ε ∈ (0,1)`, which is **strictly positive** — the prediction is LATER
  than the true crossing, not earlier. The report has the direction inverted.
- `t_f` is never ≤ 0 under the `d0 > ε` gate (the bracket is bounded below by
  `η·min(d0−ε, …)`-scale positives; numeric sweep over T ∈ {1e-12…9e-7},
  d0 ∈ {ε⁺, 30}: minimum 2.6e-9 s > 0), so the negative log term cannot drive
  the `.max(1)` clamp. The clamp is reached only when the WHOLE exhaust time is
  under one tick — which then implies `d0 − ε ≲ R/(64η)`, i.e. an escrow within
  ~ε of the gate: the "over-bank" is bounded by η·ε ≈ 1e-6 science points
  (₣1e-5). Negligible by construction, same as the designed everywhere-else
  approximation.
- "Converts its whole escrow instantly" is false: with `T → 0` and a real escrow
  (d0 = 30, R = 2, η = 1) the predicted exhaust is ~15.0 s ≈ 960 ticks — the
  drain rate in the abundant regime is `R/η` data/s regardless of T.
- "data_at's regime switch is similarly inconsistent at the boundary" is also
  false: `exhaust_seconds` is exact w.r.t. `data_at` whenever the exponential
  branch governs (data_at(t_f) = ε exactly), and for `T < ε` the residual at
  `t_f` is `T(1 + ln(ε/T)) ≤ ε` (max at T = ε) — consistent, not inconsistent.
- Numeric reproduction run (python mirroring both functions) confirmed all of
  the above; the validator claim ("only `> 0`", validate.rs:2970-2976) is true
  but moot given the above.

Verdict: DISMISSED — the mechanism as described does not exist; the only true
kernel ("T < ε is degenerate") has impact bounded by η·ε ≈ 1e-6 science.

### Finding 10: stale test comment inverts the accept path's offer handling — CONFIRMED
**Original**: the comment says `offers.remove` precedes the checks so a rejected
accept does not restore the offer; the code removes only after all checks pass.

**Verification**: in `apply_accept_contract` every rejection path returns before
the removal — prereqs (contracts.rs:823-825), active cap (contracts.rs:831-833),
deadline validity (contracts.rs:880-884) — and `offers.remove(&offer)` sits at
contracts.rs:897. In the test itself (`active_contract_cap_rejects`,
contracts.rs:1797-1842) the board is filled, the accept is rejected at the cap,
so offer 0 REMAINS on the board — the comment at contracts.rs:1839-1842 states
the opposite. Functionally harmless; the comment misleads. LOW, as filed.

---

## PRD coverage table (§3.2 / §3.3 / §3.5 / §3.6) — verified accurate
Spot-checks performed on every row carrying a claim:
- §3.2 closed form: PLAN:1100-1110 (`rate = R·min(1, D/T)`, linear
  `D0 − Rt/η`, exponential `D0·exp(−Rt/(ηT))`, exhaustion `t = (ηT/R)·ln(D0/ε)`)
  matches research.rs:304-367 verbatim (both regimes + ε exhaustion + per-second
  rates). Both research events Clamping (events.rs:497-499). WIRED — accurate.
- §3.2 diminishing repeats 63/86/95%: research.rs:741-753 implements the exact
  integral `C' = C + (cap−C)(1−exp(−k·u/cap))`, k = 1 — with u = cap per run this
  yields 63.2/86.5/94.9% exactly as PLAN:1092-1094 states. Accurate.
- §3.2 transmit/recover semantics verified in `apply_transmit_data` /
  `apply_recover_data` (ratio + EC cost; 100%/remainder + program-wide close with
  typed refusals while other holders exist). Accurate.
- §3.2 lab-model gaps DEGRADED via Findings 1/7 (9 dismissed) — accurate as
  amended by this pass.
- §3.3 parts/track-levels live (tech.rs:71-98 `is_part_unlocked`, `level_cap`,
  `frozen_upgrades` → `compose_upgrade_levels`); §3.3 facilities/contract-types
  MISSING (dead data) — Finding 6 confirmed. Accurate.
- §3.5 rows: funds sources (advance contracts.rs:913, completion :1150, science
  payout economy.rs:727-730, recovery :994-999); salary on the absolute grid
  (tests `salary_is_partition_independent`, `salary_shortfall_clamps_and_
  announces_once` passed in the reproduced run); clamp + `Insolvent` Clamping
  (economy.rs:699-721, events.rs:499); band DEGRADED (Finding 2); attrition/
  dormant/upkeep MISSING (Finding 3); recovery 1.0/0.9/0.25 + inventory credit
  (economy.rs:899-925 + balance.ron); crew model with skill always 0 (Finding 7);
  tourist itinerary/deaths-setting absent (PLAN:1243-1244 vs CrewKind::Tourist
  economy.rs:233 and colonies.rs:2205's "off|respawn branch is post-M8-A;
  permanent is the honest default"); `max_tracked_vessels` u64::MAX vs PLAN:1225's
  512 (balance.ron:15, deviation documented in-file). All accurate.
- §3.6 rows: templates/weights/prereqs/rewards/deadline/penalty wired (6 RONs +
  contracts.rs:812-935, 1169-1211); fact prereqs, no reputation currency
  (contracts.rs:407-421 area; zero reputation hits); **`standing` derived scalar
  MISSING** — PLAN:1299-1300 requires it "for display and offer-board weighting
  only", zero code hits (grep; only unrelated English usages); seeded generation
  `stream(save_seed, {CONTRACTS, 0, index})` not re-rollable by reload
  (contracts.rs:676-711); completion event-driven with the Finding 4 lab gap;
  v1 kinds PARTIAL 5 of 7 (satellite, rescue, survey, part test, resupply — crew
  transport and tourist flight from PLAN:1304-1305 absent; survey shipped twice).
  All accurate.

## Completability of the 6 shipped templates — verified accurate
- `contract.survey`: min_units 1.0, terrin, prereq tech.start (free root);
  canister run (10 units, mask 49) → transmit (needs EC) or latch → RecoverData/
  RecoverVessel — both are `DataBanked` triggers. Starter-completable. ✓
- `contract.survey.data`: min_units 5.0 draw, cap 30/key forces multiple
  biome/situation keys; situation mask 63 authored. Reachable. ✓
- `contract.satellite`: band math re-derived (any non-debris elliptic Terrin
  orbit with rp−R ≥ min, ra−R ≤ max); burns reach rails via
  `apply_to_rails_at` → `OrbitAchieved` trigger (lib.rs:6338-6340, read). ✓
- `contract.part_test`: CrewSeat-or-ThrustSite capability check
  (contracts.rs:968-987, `craft_has_module`) in Terrin orbit — any crewed rocket
  qualifies. ✓
- `contract.rescue`: transition-keyed `VesselRecovered(body)`
  (contracts.rs:988-990) — Terrin draws complete with a pad recovery; the RON's
  own comment admits no stranded-vessel spawner. ✓
- `contract.colony_resupply`: `colony_deliveries` fact recorded on landing into
  a LIVE colony's stores and `ColonyDelivery` fired
  (transfers.rs:1053-1086) — same `(colony.0, resource)` key shape the predicate
  reads (contracts.rs:1002-1016). Authored rask-mines colony exists
  (data/systems/home/colonies.ron); `FoundColony` command exists (lib.rs:964);
  `deadline_days: None` — the abandoned-colony strand risk noted in the report
  is real (folded into Finding 5). ✓
- Deadlock check: accepts pay advances (free action), science pays on transmit;
  the only soft-deadlock shape is Finding 5's jam at zero funds. Accurate.
