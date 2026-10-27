# Review Report C34 — Content: world, contracts, fixtures, demo (session-full-20261026)

Scope: `data/systems/home/**`, `data/contracts/*.ron` (6), `data/eval-scenarios/*.ron` (5),
`data/test-fixtures/{flight-launch,trivial}/`, `apps/demo/{index.html,data/**}`.
Every assigned file read in full; all numbers below re-derived (Python closed-form) or run
(`cargo test -p csp-bench` 53+12 green, `cargo test -p csp-data` 121 green on the current tree —
goldens reblessed at hash schema 21, confirming C29's "RESOLVED"). Cross-referenced verified
findings C09 (colony flow) without re-deriving the sim side.

---

### [CRITICAL] Finding 1: `contract.colony_resupply` is 9/9 uncompletable in shipped content — and the duds are a free-advance faucet

**File**: `data/contracts/contract.colony_resupply.ron:15-19` (space `bodies × resources`) vs `data/parts/*.ron` (tank inventory).

**Problem / Evidence**: The delivery path (`crates/csp-sim/src/transfers.rs:1422-1447`) requires BOTH
`vessel_resource(v, resource) >= amount` AND colony store free capacity. `vessel_resource`
(transfers.rs:312-318) sums `tank_contents`, and across all 16 parts the only `Tank` modules are
`electric_charge` (battery.z1k), `oxygen`/`food` (tank.life.s), `liquid_fuel`/`oxidizer` (s160/s320):
**no part can hold ore**. Therefore the `rask×ore` draw — the one C09-F5 verified as "completable" —
is also uncompletable: a player can never place ore aboard any vessel (launch stock is metal/composite;
vessel↔vessel transfer needs a source; withdraw-from-colony requires vessel capacity, transfers.rs:1475-1482;
`JoinColonyInventory` is an opt-in flag only, transfers.rs:1304-1340). The other 8 draws fail for the
colony-side reason C09 established: no colony anywhere has a `food`/`oxygen`/terrin-side `ore` store
(authored Rask colony: ore 100/metal 60 only; player starter def `csp-view/src/lib.rs:1301`: Solar+Battery,
no stores; no module-addition command exists), and Duva has zero sites so no colony can ever exist there.

**Impact**: A §3.6/M8-C contract kind with `weight: 2.0` that can never complete. Worse, combined with
`deadline_days: None` and the unconditional advance (contracts.rs:913-917 credits 10,000 funds on accept)
plus no decline/cancel verb, accepting duds is **free money** (up to `MAX_ACTIVE_CONTRACTS = 32` × 10k =
320k funds) that also permanently burns active slots. The board refills (contracts.rs:742), so the faucet
keeps flowing.

**Suggestion**: (a) author a cargo part with `Tank(capacity: [("ore",…),("metal",…),("composite",…)])` —
one part fixes this finding *and* Finding 2; (b) add `Store` modules for the drawn resources to the
starter colony def (or gate the generator's body/resource draws on colonies-with-matching-stores that
actually exist); (c) drop `duva` from the body list until Duva has sites, or author Duva sites;
(d) give the template a deadline so uncompletable offers can at least expire instead of being farmed.

### [HIGH] Finding 2: The §3.5 resource loop has no closing edge — metal/composite are strictly finite and the authored comment claims a refill that does not exist

**File**: `data/systems/home/sites.ron:44-49` (`starting_stock` + comment "recovery at the pad (efficiency 1.0) refills it").

**Problem / Evidence**: Recovery credits only *tank contents* to the site inventory plus craft cost as
funds (`crates/csp-sim/src/economy.rs:917-925`, `recovery_resources` :534-547). Metal and composite are
never tank resources, so the pad stock is **never** refilled by recovery — the comment is false. Sources:
starting_stock 2000 metal / 400 composite, one-time. Sinks: `ManufactureCraft` (starter craft ≈ 163 metal
per copy: pod 60 + canister 8 + battery 3 + tank 22 + sparc 70 → the "~12 starter-craft equivalents" half
of the comment is accurate) and `RunBenchTest` (1 metal/unit, `data/balance.ron:99-100`). The game's only
metal producer — the Rask smelter (`recipe.smelt_ore`, ore 1.0 → metal 0.6) — writes into a colony store
no vessel can withdraw from (no metal tank) and no site can receive. Composite has no producer at all.

**Impact**: A hard mid-game wall: after ~12 launches (fewer with bench testing) every launch is rejected
with `manufacture: insufficient site inventory` forever, while contracts keep paying funds the player
cannot spend on builds. §3.5's "resources ◄── colonies/ISRU" loop and the "Composite import-only →
creates the pull into the colony game" arc have no closing edge in the shipped content set.

**Suggestion**: Ship a cargo tank part (see Finding 1); make recovery at a launch site additionally credit
a fraction of `build_resources`; or add a colony→site export path. Fix the comment either way.

### [HIGH] Finding 3: Authored Rask outpost is balanced against the wrong day/night model — false comment, ~50× undersized battery, terminator-grazing longitude (data side of C09-F2/F3)

**File**: `data/systems/home/colonies.ron:4-7` (comment), `:16` (`Battery capacity: 5000.0`), `:31-32` (founding EC 3000); `data/systems/home/sites.ron:70` (`longitude_rad: 1.5707963267948966`); `data/systems/home/rask/body.ron:3-5,16`.

**Problem / Evidence**: `rask/body.ron`'s own header pins the PRD definition — tidally locked means
rotation period = orbital period (locked to **Terrin**, 138,984 s). Under that model the site's solar day
is the synodic month 141,115 s → night 70,558 s; at the authored draw (drill 2.0 + smelter 1.5 = 3.5 EC/s)
one night needs **246,953 EC** — the authored 5,000 EC battery covers 1,429 s (2%), the founding 3,000 EC
covers 857 s. The `colonies.ron` comment ("Rask is tidally locked — no terminator crossings, a permanently
daylit site; the battery rides the transient, not a night") is false under the body file's own definition:
under a correct model there is *no* permanently-daylit longitude on a parent-locked moon. As shipped the
colony "works" only because the sim models the moon as sun-locked (C09-F3, verified live: net +6,928 EC/s,
Day for 100 sim-days) with the site sitting **exactly on the terminator** (h0 = −π/2, `cos = +6.1e-17`,
one longitude ulp from permanent absorbing Night).

**Impact**: The outpost's viability rests on a sub-ulp floating-point margin plus a rotation model the
authored data itself contradicts. Any of: a one-ulp longitude edit, fixing the sim to the parent-locked
model, or nonzero epoch orbital elements flips the flagship M8 colony into permanent Night (battery dead
in 857 s, zero production forever) — or, under the correct model, a 98%-of-night `Degraded{PowerLost}` cycle.

**Suggestion**: Author the site at a longitude with real margin (≈2.4 rad, C09's rec) *and* size the
battery for a real night (≥ ~247k EC to ride at full throughput, or explicitly author the Degraded-at-night
cycle and rewrite the comment to say so); add a loader-time terminator-distance assertion for zero-synodic
bodies so no future edit silently lands on the boundary.

### [MEDIUM] Finding 4: eval-scenario set does not cover §8.5's named proofs for the newly wired loop

**File**: `data/eval-scenarios/*.ron` (5 files: ascent-mission, idle-warp, lease-launch, log-replay, script-ascent).

**Problem / Evidence**: PLAN §8.5 (sim-e2e row) names the tier's proofs: "launch→orbit→land, **contract
lifecycle, colony supply loop**, agent completing a mission". The five authored scenarios cover launch→ascent
(agent + script), lease gating, warp plumbing, and log replay. No scenario lands or recovers a vessel (the
internal bench recipe `flight_arc_launch_orbit_reentry_land` covers that, but it is not a committed
scenario); no scenario drives a contract board→accept→complete through the scenario format (only the
crate-internal satellite probe with snapshot-injected facts, `csp-bench/src/contracts.rs:121-145`); and
**no colony supply scenario exists anywhere** — in bench or eval — despite the M8-C loop being the newest
player-wired surface.

**Impact**: The two systems this cycle just wired (colonies, contracts) have zero committed e2e; Finding 1
is exactly the class a colony-resupply scenario would have caught. §6.4 sanctions none of this.

**Suggestion**: Add `colony-resupply.ron` (fixture world with the Rask colony: land, join, deliver ore,
goal = contract completed) and a contract-lifecycle scenario; the runner picks up any committed file
(`csp-bench/src/eval.rs:26`, `committed_eval_suite_all_pass` gates it).

### [MEDIUM] Finding 5: §3.6 v1 contract kinds — 5 of 7 authored; crew transport and tourist flight absent without cut-list sanction

**File**: `data/contracts/` (6 templates: satellite, survey ×2, part_test, rescue, colony_resupply) vs `PLAN.md:1304-1305` ("v1 types: satellite placement, crew transport, rescue, survey data run, part test at regime, base resupply, tourist flight").

**Problem / Evidence**: Two of the seven normative v1 kinds have no data template. The deferral is
documented in code (`csp-data/src/contract.rs:34-37`: "predicates bind facilities and crew itineraries in
ways v1 has no honest sim-state reading for") and the techtree's `unlocks.contract_types` markers sit
empty for them — but PLAN §6.4 (the sanctioned cut list, lines 1543-1564) contains no contracts entry.
The rationale is weakest for crew transport: `TransferColonyCrew`/`TransferCrew` completions and crew
location facts already exist to evaluate "move N crew to X" predicates.

**Impact**: Unsanctioned scope reduction of the §3.6 deliverable (variety gap, not a playability break —
the five authored kinds carry the loop).

**Suggestion**: Author a crew-transport template keyed on crew-transfer completions (machinery exists),
or amend PLAN §3.6/§6.4 to sanction the deferral of both kinds.

---

## Verified-good (no flag)

- **§3.0 body data**: every authored constant bit-matches the sheet and closed-form physics — Terrin
  period 9,203,835.510438265 s, Rask 138,984.37657447575 s, Duva 17,282,875.60361972 s (all `2π·sqrt(a³/μ)`
  to machine precision), escape 3,431 m/s, LEO 2,287 m/s, surface g 9.810/1.628/2.943, SOIs 2.430e6/4.790e7
  (consistent with `(m/M)^0.4·a`), Duva insolation 587 (= 1361·(1.36/2.07)²). Pinned by
  `csp-data/tests/home_system.rs`.
- **§3.0.1 biomes**: maps are real 64×32 indexed PNGs, distinct per body (md5 differs), every legend index
  present in each map; Rask's maria (index 0) really are concentrated on one hemisphere (lon ~0), matching
  the legend's "toward Terrin" story; loc keys resolve.
- **Fixtures**: flight-launch/trivial goldens green on the current tree (53+12 bench tests pass,
  `hash_schema_version: 21`); fixture data is a deliberately frozen §8.3 copy, sites pinned at schema 2
  with documented empty defaults.
- **Demo tree (§6.3)**: deliberate, documented subset — 16 parts byte-identical, balance identical,
  solen/terrin bodies identical, the 3 terrin-only contracts byte-identical, techtree/loc trimmed
  coherently (with a lockstep gate test); `index.html` handles no-WebGPU, late GPU-init failure, and
  wasm panics with visible fallbacks.
- **Satellite band overlap** (`periapsis.0=80k < apoapsis.1` crossing): handled — the generator swaps an
  inverted draw (contracts.rs:309-319), the validator rejects fully-inverted spaces.

## PRD coverage table

| Row (PLAN §) | Verdict | Evidence |
|---|---|---|
| §3.0 home system bodies/orbits/atmospheres | **WIRED** | constants verified above; derived checks pinned by tests |
| §3.0.1 authored biome maps + legends | **WIRED** | distinct PNGs, full index coverage, loc keys resolve |
| §3.5 sites + `starting_stock` (W2.6) | **WIRED, comment false** | Finding 2 — no refill edge exists |
| §3.5 resource loop closed by colonies/ISRU | **BROKEN in content** | Finding 2 — metal/composite finite, colony output stranded |
| §3.5.1 authored Rask outpost | **DEGRADED** | Finding 3 — works only via wrong model + sub-ulp terminator margin |
| §3.6 contract templates | **PARTIAL** | Finding 5 (5/7 kinds); Finding 1 (resupply UNREACHABLE) |
| §3.6 survey/satellite/part_test/rescue/survey.data | **WIRED** | all four completable — traced end-to-end (see table below) |
| M5 Duva content | **WIRED (body), site-less per §6.4 item 3** | `duva_reachable_in_sim_end_to_end` exists (csp-sim lib.rs:18269); Duva in resupply draws is Finding 1 |
| §7.6/§8.3 golden fixtures | **WIRED** | reblessed schema 21; suite green on current tree |
| §8.5 eval scenarios | **PARTIAL** | Finding 4 — no colony-supply/contract-lifecycle scenario |
| §6.3 demo data tree | **WIRED** | documented subset, byte-parity where present, gated by tests |

## Contract completability table (traced with reachable verbs + authored content)

| Template | Verdict | Path |
|---|---|---|
| `contract.survey` | **COMPLETABLE** | starter craft (canister 10 u, situations 49 incl. LANDED) → run on pad → transmit 40% → 4 u ≥ min 1.0 |
| `contract.satellite` | **COMPLETABLE** | band draw always satisfiable (swap rule); Terrin orbit 80–1500 km above 70 km atmo top |
| `contract.part_test` | **COMPLETABLE** | any-of module draw (CrewSeat=pod / ThrustSite=engine) in Terrin orbit ≥ drawn 80–200 km periapsis |
| `contract.rescue` | **COMPLETABLE** | land at drawn body + `RecoverVessel` (rask via rask-mines recovery at 0.9, or site=None); predicate is the recovery trigger (contracts.rs:988-990) |
| `contract.survey.data` | **COMPLETABLE (M5-tier for duva draws)** | 10 u/run, `reusable: false` → run + transmit + recover closes the key (returned 10 ≥ 5); duva draws need the full interplanetary loop, deadline 120 d |
| `contract.colony_resupply` | **UNCOMPLETABLE — 9/9 draws** | Finding 1 (one worse than C09's verified 8/9: no ore-carrying part exists) |

## Top 3 issues

1. **[CRITICAL]** colony_resupply 9/9 uncompletable + free-advance farming (Finding 1) — a whole §3.6 kind dead on arrival in content, and exploitable.
2. **[HIGH]** No closing edge in the §3.5 resource economy: ~12-launch metal ceiling, false "recovery refills it" comment, Rask metal stranded (Finding 2).
3. **[HIGH]** Rask outpost authored against the wrong tidal model: false comment, 50× undersized battery, terminator-grazing longitude with sub-ulp safety margin (Finding 3, data side of C09-F2/F3).
