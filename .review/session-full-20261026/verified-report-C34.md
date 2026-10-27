# Verified Report C34 — Content: world, contracts, fixtures, demo (session-full-20261026)

Verifier scope: every finding re-traced against the current tree (no edits made).
Empirical checks run: `cargo test -p csp-data` → 121 passed; `cargo test -p csp-bench` → 65 passed
(= C34's "53+12"); Python re-derivations for the EC/synodic/delta-v numbers; full part/recipe/command
greps. Verdicts: **5 confirmed, 0 dismissed.**

---

### Finding 1: `contract.colony_resupply` 9/9 uncompletable + free-advance faucet — CONFIRMED

**Original summary**: No part can hold ore (only EC/O2/food/LF/ox tanks exist), so even rask×ore —
the one draw C09 called completable — cannot be delivered; the other 8 draws fail for colony-side
reasons. Combined with `deadline_days: None` + unconditional 10k advance + no decline verb, duds are
a farmable free-advance faucet (up to 32 × 10k = 320k) that permanently burns active slots.

**Verification**:
- **No ore carrier.** Enumerated every `Tank(...)` in all 16 `data/parts/*.ron`:
  `csp.battery.z1k.ron:46` (electric_charge 1000), `csp.tank.life.s.ron:53` (oxygen 864 / food 216),
  `csp.tank.s160.ron:51` and `csp.tank.s320.ron:50` (liquid_fuel/oxidizer). No ore/metal/composite
  tank on any part; no part carries a Converter/ISRU module either (grep over all 16 files).
- **Delivery gate.** `transfers.rs:1422-1427`: `to_colony` requires
  `vessel_resource(src, resource) >= amount`; `vessel_resource` (transfers.rs:312-318) sums
  `tank_contents` only. Ore can never enter `tank_contents`:
  - `spawn_contents` (transfers.rs:388-410, used by ManufactureCraft at manufacture.rs:357) fills
    only each part's declared tank resources;
  - `credit` (transfers.rs:532+) stores only into parts with a matching `part.tanks` spec;
  - the only tank-credit caller in the crate is the transfer completion (transfers.rs:1121), and
    both its feeding paths are gated: colony→vessel withdraw needs `vessel_capacity` (declared
    tanks, transfers.rs:1475-1482), vessel↔vessel needs a source vessel already holding the
    resource.
- **8/9 vs 9/9 resolution.** C09's verified-report (lines 156-175) established the *colony side*
  only: "only rask×ore completable" — it verified the Rask colony HAS an ore store (cap 100) and
  stopped there. C34's vessel-side trace is airtight: even rask×ore dies at
  `vessel_resource(v,"ore") >= amount`, which is unreachable with shipped parts. **9/9 is correct;
  C09's 8/9 was optimistic on the vessel side.** The other 8 draws, re-confirmed independently:
  - terrin×{ore,food,oxygen}: the only player-foundable colony is the csp-view starter def
    (`csp-view/src/lib.rs:1301-1317`: Solar 10 m² + Battery 500 + EC level 250 — **no Store
    modules**), so aggregate store capacity for any resupply resource is 0; both the initiation
    free-capacity gate (transfers.rs:1431-1441) and `credit_end`'s store lookup (transfers.rs:1136)
    fail. Authored colonies cover rask only.
  - duva×{ore,food,oxygen}: `data/systems/home/sites.ron` has exactly three sites — terrin-launch,
    terrin-downrange (terrin), rask-mines (rask). `apply_found_colony` rejects unknown sites
    (colonies.rs:1595-1596), so no colony can ever exist on Duva.
  - rask×{food,oxygen}: authored Rask colony stores are ore 100 / metal 60 / EC (colonies.ron:28-34)
    — no food or oxygen store → capacity 0, delivery rejected/lands 0, and the `colony_deliveries`
    fact (written only at transfers.rs:1076-1081, gated `landed > 0`) never moves.
- **Faucet.** `apply_accept_contract`: prerequisite check → active-cap check
  (`MAX_ACTIVE_CONTRACTS = 32`, contracts.rs:104, enforced ~:830) → unconditional
  `credit_funds(advance_funds)` at contracts.rs:913-917 → deadline armed only when authored
  (`deadline_days: None` → `deadline = None` → no `ContractDeadline` event, no failure path). The
  only exits from `active` are completion (impossible for these) and deadline failure (never armed);
  `SimCommand`'s contract surface is `AcceptContract` only — no decline/cancel anywhere in sim or
  view (grep clean), consistent with verified C08-F5 (board jam, no decline/expiry). The board
  refills via `publish_contracts` (contracts.rs:711+) after every accept. Prereq `tech.rocketry.3`
  costs 60 science (techtree line 54) — reachable. So: accept duds as they appear up to 32 × 10k =
  320k farmed. Worse than stated, in fact: once 32 duds are held, **no** contract (including good
  kinds) can be accepted — the whole §3.6 system bricks.

**Verdict**: CONFIRMED — critical, exactly as reported (9/9, not 8/9).

### Finding 2: §3.5 resource loop has no closing edge; "recovery refills it" comment false — CONFIRMED

**Original summary**: Recovery credits only tank contents to site inventory plus craft cost as
funds; metal/composite are never tank resources, so the pad stock (2000 metal / 400 composite) is
never refilled — the sites.ron comment is false. Only metal producer (Rask smelter) is stranded;
composite has no producer at all. ~12-launch ceiling.

**Verification**:
- `recovery_resources` (economy.rs:534-547) reads `tank_contents` only → EC/food/oxygen/LF/ox, never
  metal/composite. The site-inventory write at economy.rs:963-967 is the *only* site inventory
  credit besides initial install (economy.rs:661); craft cost lands in funds
  (`part_micros → credit`, economy.rs:994-998). So sites.ron:44-45's "recovery at the pad
  (efficiency 1.0) refills it" is false for the metal/composite stock the comment annotates.
- Starter craft ("Stock Hopper", `csp-view/src/demo.rs:53-59`) = pod.mk1 + science.canister +
  battery.z1k + tank.s160 + engine.sparc → build_resources metal 60+8+3+22+70 = **163** (verified
  against each part file). 2000/163 ≈ 12.3 → "~12 starter-craft equivalents" accurate. Bench
  consumption: `reliability_bench_resource: "metal"`, 1.0/unit (balance.ron:99-100). Exhaustion
  rejects with "manufacture: insufficient site inventory" (manufacture.rs:204).
- Recipes: only electrolysis (water→oxygen), isru_propellant (ore→LF/ox), smelt_ore (ore 1.0 → metal
  0.6). **No composite producer exists.** Colony metal (store cap 60) is stranded:
  `TransferEnd` is Vessel|Colony only (transfers.rs:200 — no site endpoint), and withdraw into a
  vessel needs a metal tank no part has (Finding 1's enumeration).

**Verdict**: CONFIRMED — comment false, loop open, ceiling real.

### Finding 3: Rask outpost authored against the wrong day/night model; sub-ulp terminator — CONFIRMED

**Original summary**: rask/body.ron's own header defines locked-to-Terrin (rotation = orbital period
138,984 s). Under that model the site's solar day is the synodic month → night 70,558 s → one night
at 3.5 EC/s needs ~247k EC; authored 5000 EC battery covers 2%. The colonies.ron "permanently
daylit" comment is false under that definition. As shipped it "works" only because the sim treats
the moon as sun-locked with the site exactly on the terminator (cos = +6.1e-17).

**Verification (re-derived)**:
- rask/body.ron:3-4,16: `rotation_period_s: None` = "rotation period is the orbital period"
  (parent Terrin, `period_s: 138984.37657447575`). Parent-locked ⇒ solar day = synodic month:
  1/(1/138984.3766 − 1/9203835.5104) = **141,115.3 s**, night **70,557.7 s**. Authored draw:
  drill `power_ec_per_s: 2.0` + converter `1.5` = 3.5 EC/s (colonies.ron:21-27) →
  3.5 × 70,557.7 = **246,952 EC** per night (report's 246,953 — a 1-EC rounding difference on the
  same closed form; the "~247k" and "battery 5000 covers 1,429 s = 2.0%" and "founding 3000 covers
  857 s" figures reproduce exactly). No permanently-daylit longitude exists on a parent-locked
  moon — the colonies.ron:4-7 comment is false under the body file's own definition.
- Sim-as-shipped: `body_light` (colonies.rs:408-437) resolves `rotation_period_s: None` as
  `ω = n` (the moon's own orbital mean motion) → `synodic_rate = 0` — a fixed subsolar point, i.e.
  the sun-locked model C09-F3 verified live. With all epoch angles 0 (body.ron:18-25), h0 = lon − π
  = −π/2; computed cos(h0) = **+6.123e-17 > 0 → Day**; lon − 1 ulp gives cos < 0 → **Night**. The
  sub-ulp margin claim reproduces exactly. Solar margin cross-check: 40 m² × 0.20 × 1361 × (2/π) =
  6,931.5 W → net +6,928.0 EC/s over the 3.5 draw — matches C09's live +6,928.02. The ≈2.4 rad
  recommendation gives cos margin 0.737 — sound.

**Verdict**: CONFIRMED — numbers reproduce to machine precision (one immaterial 1-EC rounding nit).

### Finding 4: eval-scenario set omits §8.5's contract-lifecycle and colony-supply proofs — CONFIRMED

**Original summary**: 5 authored scenarios cover ascent/lease/warp/replay only; no land/recover
scenario, no contract board→accept→complete scenario, no colony-supply scenario anywhere.

**Verification**: `data/eval-scenarios/` contains exactly ascent-mission, idle-warp, lease-launch,
log-replay, script-ascent; none mention contract/colony (grep). PLAN §8.5's Sim-e2e row (line 1806)
names "launch→orbit→land, contract lifecycle, colony supply loop, agent completing a mission"
verbatim. `flight_arc_launch_orbit_reentry_land` is a bench unit test (csp-bench/src/scenarios.rs:681),
not a committed scenario; the only contract lifecycle coverage is the crate-internal probe with
snapshot-injected facts (`csp-bench/src/contracts.rs:121-145`, `mark_facts` through the save
boundary). The runner is additive-by-file (eval.rs doc, "drop a scenario in
`data/eval-scenarios/` and the suite picks it up").

**Verdict**: CONFIRMED.

### Finding 5: 5 of 7 normative §3.6 contract kinds authored; deferral unsanctioned by §6.4 — CONFIRMED

**Original summary**: crew transport and tourist flight have no template; deferral documented in
code but absent from the §6.4 cut list; crew-transfer machinery already exists.

**Verification**: PLAN.md:1304-1305 lists 7 v1 kinds verbatim ("satellite placement … crew
transport, rescue, survey data run, part test at regime, base resupply, tourist flight"). Six
templates shipped covering 5 kinds (SatellitePlacement, SurveyRun ×2, PartTest, Rescue,
ColonyResupply). Deferral note at csp-data/src/contract.rs:33-36 ("Crew transport and tourist flight
are deferred…"); techtree `contract_types` markers list exactly the 6 shipped templates (techtree
lines 23-68), none for the missing kinds. §6.4 cut list (PLAN.md:1543-1564) has 5 items — render
tier, life support, Duva, docking, web tier — **no contracts entry**. Crew machinery exists
(`TransferCargo::Crew` + `land_crew` in transfers.rs:1101+, crew location facts), supporting the
"rationale weakest for crew transport" note.

**Verdict**: CONFIRMED.

---

## Verified-good / PRD rows — spot-checked, no changes

- **§3.0 constants**: pinned by `csp-data/tests/home_system.rs` (Rask 1.390e5 ±0.1% checks at
  lines 103-124); suite green (121 passed on current tree).
- **Suites**: csp-bench 65 passed (= "53+12"), csp-data 121 passed — both green as claimed.
- **Completability table**: rescue spot-checked end-to-end — predicate
  `Rescue{body}` matches `ContractTrigger::VesselRecovered(b)` (contracts.rs:988-990), fired from
  the recovery commit with the latched body (economy.rs:996-1001); trivially completable on terrin,
  and rask-mines recovery at 0.9 covers rask. Prereq chain (satellite first) closes: Stock Hopper
  alone is ~1,971 m/s vac (short), but a buildable two-stage (sparc+s320 / ember+s160, all
  tech-reachable parts) computes ~4,477 m/s vac vs ~2,287 + losses — satellite/part_test/rescue are
  genuinely completable. Satellite band swap rule confirmed at contracts.rs:309-319. SurveyRun
  starter path (canister, transmit ≥ min 1.0 u; survey.data reusable:false + recover-closes-key,
  deadline 120 d) reads as reported.
- **Demo tree**: `apps/demo/data/parts` byte-identical to `data/parts` (only a `.gitkeep` differs —
  not a part file); demo carries exactly the 3 terrin-only contract templates, byte-identical.
- **M5 Duva row**: `duva_reachable_in_sim_end_to_end` exists at csp-sim/src/lib.rs:18269 exactly as
  cited; Duva site-less is §6.4 item 3; Duva in resupply draws is Finding 1.

## Notes (immaterial)

- Finding 3: report says 246,953 EC/night; exact closed form gives 246,952 EC. No consequence.
- Finding 1: the impact is slightly understated — at 32 burned slots the player can no longer
  accept *any* contract, so the faucet also hard-bricks the entire contract system, not just the
  resupply kind.

**Final: 5 confirmed / 0 dismissed.**
