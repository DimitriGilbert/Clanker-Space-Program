# Review report C08 — Progression: science / tech / economy / contracts

Session `session-full-20261026`, target = current working tree (HEAD 7dd4b86).
Files reviewed in full: `crates/csp-sim/src/{research,tech,economy,contracts}.rs`;
wiring cross-checked in `csp-sim/src/{lib,events,transfers}.rs`, `csp-view/src/lib.rs`,
`csp-ui/src/program.rs`, `data/{contracts,techtree,experiments,balance}.ron`,
`data/parts/{csp.lab.mk1,csp.science.canister}.ron`, `csp-bench/src/economy.rs`,
`docs/GAMEPLAY-GUIDE.md`. Module tests verified green (`cargo test -p csp-sim --lib --
research:: tech:: economy:: contracts::` → 38 passed).

---

### [HIGH] Finding 1: `RecoverVessel` silently destroys a running lab project — escrowed data AND accrued science vanish
**File**: crates/csp-sim/src/economy.rs:880-1010 (no project handling) + crates/csp-sim/src/lib.rs:5008-5016
**Problem**: `apply_recover_vessel` banks only *pooled* science (`v.science.data`), then calls
`remove_vessel`, which unconditionally calls `research::drop_vessel_projects` — "work-in-progress
is lost, no accrual". `StartResearch` has no residence restriction, so a project is routinely
live while the vessel is `SurfaceLatched` (that is exactly the recoverable state).
**Evidence**: economy.rs:934-958 iterates `v.science.data` only; lib.rs:5010-5016 drops projects
with zero accrual and zero remnant; contrast `apply_stop_research` (research.rs:1097-1112) which
banks accrual and returns the remainder, and the Program copy "stop → bank the accrued science
and release the rest" (program.rs:2131-2137) plus "RecoverVessel … pooled science banks"
(program.rs:590-591).
**Impact**: a player who recovers a lab vessel mid-conversion loses the whole escrowed key
(up to `cap` data units, e.g. 30 on Terrin = 30 science = ₣300 milestone) plus all accrued
points, with no notification and no way to know. Directly feeds complaint #6 ("research/science
UI confusing") — the recovery surface promises banking and instead eats the lab's work.
**Suggestion**: in `apply_recover_vessel`, before `remove_vessel`, finalize the vessel's projects
(`finalize_project` per project: bank accrual, return remnant into the pool under the lab node) so
both land in the recovery banking pass; keep `drop_vessel_projects` for destruction paths only.

### [HIGH] Finding 2: shipped default salary blows the §3.5 "2–5% per sim-month" band, and the knob lives outside balance.ron
**File**: crates/csp-view/src/lib.rs:1291-1294 (`HIRE_SALARY_PER_S = 0.02`) vs data/balance.ron (no salary field) and PLAN.md:1196-1200
**Problem**: every UI hire draws ₣0.02/s (csp-view/src/lib.rs:3589, 1084). One sim-month =
30·86 400 s → ₣51 840/month. On the authored starting balance of ₣1 000 000 that is **5.18%
for a single crew member** — above the 5% ceiling PLAN sets for a *fully-staffed* base; the
harness's own "fully staffed" probe (20 crew, csp-bench/src/economy.rs:374) would drain ~104%/month.
**Evidence**: `staffed_base_drains_in_band` (csp-bench/src/economy.rs:371-401) *solves*
`salary_per_s` from `drain_fraction` instead of asserting the shipped default, so the §8.3 gate
cannot catch this; `HIRE_SALARY_PER_S` is a hardcoded constant while balance.ron's own header
says "ALL tuning lives here" (§8.3). The sim-side `HireCrew` has no default salary at all
(economy.rs:813-852) — the number exists only in the view crate.
**Impact**: a fresh program that hires even one scientist goes insolvent in ~19 idle months;
"fully-staffed" play is a guaranteed bankruptcy spiral. Insolvency itself is reachable and
correctly clamped (verified), but the PRD's measurably-tuned economy is not what ships.
**Suggestion**: author a `crew_salary_per_s`-class field in balance.ron (~0.003–0.008 total for
a staffed base to land mid-band), thread it through `HireCrew`'s callers, and pin the shipped
value in `balance_ron_mirrors_economy_defaults` so the band assertion runs on real data.

### [HIGH] Finding 3: §3.5 insolvency consequences are missing — no `StaffAttrition`, no `FacilityDormant`, no facility upkeep
**File**: crates/csp-sim/src/economy.rs:65-76 (module doc) + PLAN.md:1196-1204
**Problem**: PLAN:1202 — "funds clamp at zero; unpaid salaries trigger `StaffAttrition` and
`FacilityDormant` events; an `Insolvent` state is a `Clamping` event". The clamp and the
`Insolvent` Clamping event are implemented (economy.rs:699-721, verified Clamping in
events.rs:499), but `StaffAttrition` and `FacilityDormant` do not exist anywhere in the tree
(grep: zero hits), and there is no facility-upkeep drain at all (grep "upkeep": only a comment
in csp-bench/src/economy.rs:10 deferring it to "M8's colony/economy block" — which shipped).
**Evidence**: `fire_salary_due` (economy.rs:773-805) clamps, sets `insolvent`, re-arms — and
nothing else ever happens. Crew stay `Active` forever at zero funds; §6.4's cut list does not
sanction dropping them.
**Impact**: bankruptcy is a one-shot toast, not a state with teeth — a broke program keeps its
entire roster at full capability indefinitely, so "warping to a transfer window can drain you"
has no gameplay consequence beyond the notification. PRD-coverage: DEGRADED (mechanic hollowed).
**Suggestion**: minimal v1: on a `SalaryDue` firing that shortfalls while already `insolvent`,
mark N roster members `Retired` (StaffAttrition) with a notification; facility upkeep can ride
the same event once a facilities arena exists. Even one attrition step per insolvent month
makes the §3.5 loop real.

### [MEDIUM] Finding 4: survey contracts can expire (penalty) with the objective already met — lab conversion has no completion trigger
**File**: crates/csp-sim/src/research.rs:1084-1130 (`apply_stop_research`), research.rs:1135-1167 (`fire_research_exhausted`) vs crates/csp-sim/src/contracts.rs:1031-1060 (`survey_returned_units`)
**Problem**: `survey_returned_units` counts lab-converted units as *returned* — it subtracts the
live escrow projection `data_at(now)`, so its value rises continuously as the lab works and jumps
to `collected − pooled` at exhaustion/stop. But neither `apply_stop_research` nor
`fire_research_exhausted` calls `contract_notify(DataBanked)`; the only survey triggers are
transmit/recover (research.rs:846, 946; economy.rs:1009).
**Evidence**: a `contract.survey.data` ask satisfied purely by lab conversion (e.g. 5 units
escrowed, converted in seconds — lab T=0.5 data/s, R≥2 science/s, data/parts/csp.lab.mk1.ron:58)
leaves the contract `active` until an unrelated trigger; its authored 120-day deadline
(data/contracts/contract.survey.data.ron) then fires `ContractDeadline` and charges the ₣2 000
penalty while `objective_met` is true.
**Impact**: contract completes-or-penalizes on the wrong event — the exact §3.6 violation class
("predicate over sim state checked on relevant events"); player sees a failed contract they
had already finished. Also stalls completion until an unrelated transmit/recovery happens.
**Suggestion**: fire `ContractTrigger::DataBanked` wherever research banking increases returned
units (stop + exhaustion), or add a `ResearchProjectComplete` trigger evaluated in
`contract_notify`.

### [MEDIUM] Finding 5: the offer board has no decline/expiry — unreachable offers jam slots permanently, and the instant-complete faucet is bounded by that jam, not by design
**File**: crates/csp-sim/src/contracts.rs:730-767 (publish), contracts.rs:897 (only removal), lib.rs SimCommand set (no decline verb)
**Problem**: offers leave the board only via `AcceptContract`. There is no decline, no staleness
sweep, no re-roll (by design, §3.6). Once the 6 slots hold offers the player cannot satisfy
(rask/duva rescues and surveys before interplanetary capability), the board is frozen forever.
**Evidence**: grep over the SimCommand surface: no Decline/Cancel/Expire offer verb; `publish_contracts`
returns early while `offers.len() >= board_size` (contracts.rs:736). The `apply_accept_contract`
doc's bound claim (contracts.rs:802-811, "the faucet self-starves") is inverted in practice:
*satisfiable* satellite draws (~41% of draws match a parked 150 km circular orbit:
min-periapsis U(80k, 250k) ≤ 150k, max-apo always ≥ 150k) are consumed-and-paid (₣40k per click)
while *unsatisfiable* ones accumulate — the end state is a permanently jammed board, which is the
same state as "no faucet".
**Impact**: mid-game contract income can die permanently with no player recourse; combined with
zero funds and no in-flight vessel this is a soft progression deadlock (only F9-revert escapes).
**Suggestion**: expire offers at publish time (drop entries older than N·salary_period and
redraw), or add a `DeclineContract` command; either also gives the instant-complete faucet an
honest bound.

### [MEDIUM] Finding 6: tech-tree `unlocks.facilities` / `unlocks.contract_types` are dead data — §3.3's unlock semantics are hand-duplicated in contract prereqs
**File**: data/techtree/home.ron (all four nodes) vs crates — zero consumers
**Problem**: PLAN §3.3: "A `TechNode` … unlocks new `PartId`s, `UpgradeTrack` levels, facilities,
and contract types." PartIds (via `PartDef::tech_requirement`) and track levels (`level_cap`)
are live; `unlocks.facilities` and `unlocks.contract_types` are validated for id-resolution
(csp-data/src/validate.rs:3779, 4238) but read by nothing in sim or UI (grep: no hits outside
csp-data and tech.rs tests). Actual contract gating is the duplicated
`TechResearched("tech.rocketry.2")` prerequisite inside each contract RON.
**Evidence**: data/techtree/home.ron:36 `contract_types: ["contract.satellite"]` ↔
data/contracts/contract.satellite.ron `prerequisites: [TechResearched("tech.rocketry.2"), …]` —
two sources of truth; the validator checks each resolves but not that they agree, so they can
silently drift. Facilities (`facility.lab`, `facility.test_stand`) gate nothing — the lab is
indirectly gated only by its part's `tech_requirement: "tech.science.1"`.
**Impact**: PRD-coverage DEGRADED; the unlock model the tree authors is decorative, and a
future edit to one side of the duplication will not fail CI.
**Suggestion**: either derive contract-template eligibility from `unlocks.contract_types`
(board filter joins the researched set's unlocked types) and delete the duplicated prereq, or
delete the dead fields from the schema; add a validator rule that the two must agree.

### [MEDIUM] Finding 7: every hire is skill 0 — the sim hardcodes it and the view papers over it, so `R = Σ_researcher(skill)` never reflects crew quality
**File**: crates/csp-sim/src/economy.rs:839 (`skill: 0`), csp-view/src/lib.rs:4987-4993 (view-side floor)
**Problem**: `SimCommand::HireCrew` carries no skill; `apply_hire_crew` writes `skill: 0` for
every member. The roster panel shows "skill 0" for all hires (program.rs:1118-1119). The lab
path then floors it view-side: `sim.economy().crew.get(&c.id).map_or(1, |m| m.skill).max(1)`
(csp-view/src/lib.rs:4991) — so §3.2's `R = Σ_researcher(skill)·lab_multiplier` is in practice
`Σ 1 · 2.0` for everyone, and an id-only `BoardCrew` member (not on the roster, no salary)
researches exactly as well as a hired ₣5 000 Scientist.
**Evidence**: research.rs:87-89 documents "the real crew arena with per-member skills is a
later M7 sub-phase; the command value is the logged truth" — but `StartResearch` also never
cross-checks the claimed skill against the roster member it names (research.rs:995-1006), so
any client can claim skill 5 for a skill-0 member.
**Impact**: crew-kind choice (Scientist vs Tourist) has zero progression meaning; the research
rate the panel copy implies ("crew aboard staff it") is a constant; sim/client skill mismatch is
unvalidated.
**Suggestion**: add `skill` to `HireCrew` (authored per kind, or drawn from a balance table),
have `StartResearch` read the roster member's skill instead of trusting the command, and drop
the view-side floor.

### [MEDIUM] Finding 8: DRY — four copies of the "scrub pending events" heap rebuild and three copies of the transmit/recover banking block
**File**: crates/csp-sim/src/research.rs:585-596, 1117-1128, 1222-1233; contracts.rs:1143-1148; research.rs:823-846 vs 923-946; research.rs:897-915 vs economy.rs:940-955
**Problem**: (a) `drop_vessel_projects`, `apply_stop_research`, `finalize_projects_on_nodes`, and
`complete_contract` each hand-roll the same `mem::take(heap) → filter → collect` pattern for
dropping a project's/deadline's pending events — four sites to keep in sync (a missed arm breaks
the restore pairing gate silently until a snapshot fatals). (b) the banked-science block
(`banked += …; credit_science_payout; notify(ScienceBanked)`) is duplicated between
`apply_transmit_data` and `apply_recover_data`. (c) the "key closable ⇔ no other holder" and
`1 − transmit_ratio` fraction logic is duplicated between `apply_recover_data` and
`apply_recover_vessel` with slightly different shapes (missing-def ⇒ 0.0 in economy.rs:947,
vs typed rejection in research.rs:880-884).
**Suggestion**: one `World::scrub_events(pred)` helper; one `bank_science(key, units, close)`;
one `recover_fraction(&self, key) -> f64`. Cuts ~80 lines and removes the divergence risk
already visible in (c).

### [LOW] Finding 9: `exhaust_seconds` assumes throughput ≥ data_epsilon — a sub-ε lab exhausts at +1 tick and over-banks
**File**: crates/csp-sim/src/research.rs:337-347
**Problem**: for `d0 > T` the formula is `η(d0−T)/R + (ηT/R)·ln(T/ε)`; with `0 < T < ε`
(crafted part data — the validator only requires `> 0`, validate.rs:2970-2980) the log term is
negative, the predicted exhaustion precedes the true (all-linear, since D hits ε before T)
crossing, and `exhaust_tick`'s `.max(1)` clamps to +1 tick where `fire_research_exhausted` banks
the full `η·D0`. `data_at`'s regime switch is similarly inconsistent at the boundary.
**Impact**: a degenerate-but-valid lab spec converts its whole escrow instantly; unreachable
with the shipped lab (T = 0.5) but one RON edit away with no gate.
**Suggestion**: reject `throughput_data_per_s < data_epsilon` in `apply_start_research`
(or bound it in the validator alongside the `> 0` check).

### [LOW] Finding 10: stale test comment inverts the accept-path's offer handling
**File**: crates/csp-sim/src/contracts.rs:1839-1842
**Problem**: the comment says "`offers.remove` precedes the checks; a rejected accept does not
restore it" — in the code every check (prereqs :823, cap :831, deadline validity :880) returns
before `offers.remove` (:897), so a rejected accept leaves the offer on the board.
**Impact**: none functionally, but the comment documents the opposite of the (correct) behavior
and will mislead the next editor of the cap path.
**Suggestion**: fix the comment (or assert offer survival in that test to make it honest).

---

## PRD coverage table (§3.2 / §3.3 / §3.5 / §3.6)

| Requirement | Status | Evidence |
|---|---|---|
| §3.2 data inventory, typed keys, 6 situations | WIRED | research.rs:122-177; `Situation` |
| §3.2 diminishing repeats (63/86/95%) | WIRED | research.rs:741-753, exact integral; test :1317 |
| §3.2 transmit (ratio+EC) / recover (100%, closes, remainder) | WIRED | research.rs:774-847 / 861-948; blanket-per-key reading documented |
| §3.2 lab closed form (2 regimes, ε exhaustion, per-second) | WIRED | research.rs:304-367 — matches PLAN math verbatim; tests :1274-1311 |
| §3.2 `ResearchProjectComplete`/`ResearchDataExhausted` both Clamping | WIRED | events.rs:497-499 |
| §3.2 hoarding strategy | WIRED | pool persists; escrow on project |
| §3.2 lab model gaps | DEGRADED | Findings 1 (recovery drops escrow), 7 (skill), 9 (T<ε) |
| §3.3 nodes unlock parts / track levels | WIRED | tech.rs:71-98, 271-291; frozen-at-build verified by test :662 |
| §3.3 nodes unlock facilities / contract types | MISSING (dead data) | Finding 6 |
| §3.3 multiplicative deltas, never new PartDefs; no retroactive upgrades incl. reload | WIRED | `compose_upgrade_levels` + `upgrade_levels_at_build`; snapshot round-trip test :727 |
| §3.3 tree validator (cycles/reachable/monotonic) in CI | WIRED (C32 owns detail) | validate.rs; `cargo test --workspace` in CI |
| §3.5 funds sources (advance/completion, milestones, recovery) | WIRED | contracts.rs:913/1150, economy.rs:727-730, 922-999 |
| §3.5 salary drain on absolute grid, partition-independent | WIRED | economy.rs:740-805; tests :1190, :1408 |
| §3.5 funds clamp at zero; `Insolvent` Clamping + notify | WIRED | economy.rs:486-521, 699-721; test :1226 |
| §3.5 2–5%/sim-month band asserted by harness | DEGRADED | Finding 2 — harness solves its own rate; shipped default blows the band |
| §3.5 `StaffAttrition` / `FacilityDormant` / facility upkeep | MISSING | Finding 3 |
| §3.5 recovery 1.0 launch / 0.9 / 0.25 abandon, inventory credit | WIRED | economy.rs:899-925; tests :1269-1329 |
| §3.5 crew model (kinds, skill 0..=5, location, status, salary) | WIRED (skill always 0 — Finding 7) | economy.rs:296-311 |
| §3.5 hire → `Facility` | DEGRADED (documented) | lands `Unassigned`; no facility arena |
| §3.5 tourist itinerary / deaths setting (`off|respawn|permanent`) | MISSING (kind only / permanent-only) | economy.rs:233, colonies.rs:2205 |
| §3.5 `max_tracked_vessels = 512` | DEGRADED (data deviates, documented) | balance.ron sets u64::MAX |
| §3.6 templates: params/weights/prereqs/rewards/deadline/penalty | WIRED | 6 RONs; contracts.rs:293-402 |
| §3.6 fact prereqs, no reputation currency | WIRED | contracts.rs:407-421 |
| §3.6 `standing` derived scalar (display + offer weighting) | MISSING | no hits; weighting uses authored weights only |
| §3.6 seeded generation, not re-rollable by reload | WIRED | contracts.rs:676-711; test :1339 |
| §3.6 completion on relevant events, never polled | WIRED (lab gap — Finding 4) | triggers at lib.rs:4863/5872/6340/8568, transfers.rs:1083 |
| §3.6 v1 kinds (satellite, crew transport, rescue, survey, part test, resupply, tourist) | PARTIAL — 5 of 7 kinds (crew transport + tourist flight absent; survey shipped twice) | data/contracts/ |

## Completability of the 6 shipped templates — all completable via reachable verbs
- `contract.survey` (prereq tech.start, free root): run canister → transmit (needs EC aboard)
  or land+latch → `RecoverData`/`RecoverVessel`. Starter-completable, unlocks the chain.
- `contract.survey.data` (tech.science.1 + 1 SurveyRun): same verbs, ≥5 returned units; cap 30/key
  forces multiple biome/situation keys — reachable (situation mask 63).
- `contract.satellite` (tech.rocketry.2 + 1 SurveyRun): any non-debris vessel in the drawn
  Pe 80–250 km / Ap ≤1500 km band; burns trigger via `finish_burns → apply_to_rails_at →
  OrbitAchieved` (verified). Generator's swap rule keeps every window non-empty.
- `contract.part_test` (tech.rocketry.3): CrewSeat-or-ThrustSite presence in Terrin orbit —
  any crewed rocket qualifies; completable.
- `contract.rescue` (tech.rocketry.3 + 1 Satellite): transition-keyed `VesselRecovered(body)`;
  terrin draws trivial (land + recover at pad); honest "no stranded spawner" comment in data.
- `contract.colony_resupply` (tech.rocketry.3): `TransferComplete` into a live colony's stores
  records the fact and fires `ColonyDelivery` (transfers.rs:1064-1086, verified same key shape
  as the predicate); Terrin colonies foundable, authored Rask outpost exists. No deadline —
  note an abandoned-colony edge can strand an active contract in the 32-slot arena forever
  (folded into Finding 5's no-decline problem).
- Progression-loop deadlock check: none in the normal loop — contracts pay advances on accept
  (free action), science pays on transmit; the only deadlock shape is Finding 5's jam at zero
  funds with no in-flight vessel.

## Top 3 issues
1. **Finding 1** — `RecoverVessel` silently destroys live lab projects (escrow + accrued
   science) on a successful recovery; contradicts the recovery surface's own copy.
2. **Finding 2** — the shipped hire salary (0.02 ₣/s, hardcoded outside balance.ron) puts a
   single crew member above the §3.5 fully-staffed 2–5%/month ceiling, and the §8.3 harness
   cannot detect it because it solves its own drain rate.
3. **Finding 3** — `StaffAttrition` / `FacilityDormant` / facility upkeep (PLAN:1202) are
   absent: insolvency is a one-shot toast with no consequences, hollowing §3.5's bankruptcy loop.
