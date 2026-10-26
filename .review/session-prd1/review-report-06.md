# PRD-06 + PRD-07 — Progression (science/tech/economy/contracts) + RTS layer (colonies/manufacture/life-support) review

**Cluster files:** `crates/csp-sim/src/research.rs` (1300), `tech.rs` (877), `economy.rs` (1407), `contracts.rs` (2133), `colonies.rs` (5168), `manufacture.rs` (596), `life_support.rs` (756). Data: `data/techtree/home.ron`, `data/contracts/*.ron` (6), `data/systems/home/{colonies,sites}.ron`, `data/experiments/exp.materials_study.ron`, `data/parts/*.ron` (11), `data/balance.ron`. Supporting: `crates/csp-view/src/lib.rs` (launch path, `stage_program`), `crates/csp-ui/src/program.rs`, `crates/csp-view/src/demo.rs`, `crates/csp-app/src/lib.rs`, `crates/csp-sim/src/transfers.rs` (crew board/transfer).

**PRD baseline (PLAN §3.2–§3.6):** science = data × time × researchers, data as inventory with soft-cap + transmit/recover + labs · tech tree + upgrade tracks frozen at build · funds with salaries/recovery/insolvency · colonies as event-bounded flow networks · manufacture via site-inventory build orders · seeded offer board with fact prereqs + event predicates.

**Verdict:** the sim machinery is essentially complete and honest — the §3.2 ledger, closed-form lab, micro-fund ledger, seeded board, and the 5.1k-line colony flow network are all real, well-documented implementations. **But the player-facing game exposes only a fraction of it.** The science loop IS playable today — via a degenerate, undiscoverable path (pad-sit → Program window → run → recover). Everything downstream — labs, researchers, crew, colonies, manufacture BOM/time, vessel recovery — is either UI-absent or content-absent. Two of the six contract templates offer contracts a player can *never* complete.

**Corrections to the exploration map (§2, §3):**
- *"launches cost nothing"* — **wrong.** `apply_spawn_physical` debits `craft_cost_micros` at the pad (`lib.rs:5495–5500`, charge at `:5582`); `launch_preflight` refuses unaffordable crafts (`csp-view/src/lib.rs:2399–2407`). What launch bypasses is `ManufactureCraft`'s BOM+time pipeline — the funds debit is real.
- *"recovery requires SurfaceLatched and there is no parachute → effectively transmit-only"* — **overstated.** The settle latch fires for any gentle touchdown (powered landing is feasible at the starter's TWR≈5), and the launchpad itself is a valid `Landed` context, so `RecoverData` is trivially reachable on Terrin with zero flight. The true hole is that **`RecoverVessel` has no player path and `SimCommand::TerminateVessel` doesn't exist at all** (PLAN:1228 mandates it).
- *"`SetLifeSupportEnabled` issued at startup by app"* — only when the user's `difficulty.life_support_enabled` setting is on; default is **off** (`csp-ui/src/settings.rs:152`). It's an opt-in setting, not an unconditional startup flag.

---

## The science loop, traced end-to-end

**What a player can actually do today:**

1. VAB palette includes `csp.science.canister` (tech.start, `data/parts/csp.science.canister.ron:43-48`) — one-shot `Experiment` module, situations `LANDED|SPACE_LOW|SPACE_HIGH` (mask 49).
2. Launch → craft spawns InBubble resting on the pad → settle latch (~2 s, `simple.rs` `settle_ticks=128`) → `SurfaceLatched`.
3. Top-bar "Program" button (drawn every mode, `csp-view/src/lib.rs:443,615-617`) → Program window shows the vessel's context (`Landed`, staged at `:2569-2602`), the experiment row ("run" enabled), and pooled-data rows.
4. **Run** → `apply_run_experiment` pools ~8.5 units at `(exp.materials_study, terrin, <pad-biome>, Landed)` (soft-cap `30·(1−exp(−10/30))`, `research.rs:734-738`).
5. **Recover** (`input.latched` gate, `program.rs:432-446`) → `apply_recover_data` banks 100% → **~8.5 science + ~85 funds** (`science_funds_per_point=10`) and closes the key. **Transmit** banks 40% but requires ≥8.5 `ElectricCharge` aboard — the stock starter has **no battery** (see H6).
6. Survey contract (`contract.survey`, tech.start) completes on the `DataBanked` notify inside `apply_recover_data`/`apply_transmit_data` → +7000 funds.

**So science generation works** — but only because the launchpad is itself a `Landed` context. The user's "how do I do science?" complaint is *discoverability + content*, not a missing sim path:

- The seeded starter craft is `["csp.pod.mk1","csp.tank.s160","csp.engine.sparc"]` (`demo.rs:38`) — **no canister, no battery**: the out-of-box craft can neither produce data nor transmit it.
- The tutorial's last step is "Close the orbit" (`csp-ui/src/tutorial.rs:68-104`) — it never mentions the Program window, science, or contracts.
- `RunExperiment`/`TransmitData`/`RecoverData` exist only in the Program window; nothing anywhere else points at it.

**Loop-breaking gaps the user hit:** "no recovery" = no `RecoverVessel`/`TerminateVessel` verb (H1); "no researchers / can't assign researchers" = no Lab part + no crew flow (H2); the rest of the progression layer is hidden (H3–H5).

---

## Findings

### HIGH

**H1 — No player verb recovers or removes a vessel; `TerminateVessel` doesn't exist.**

`apply_recover_vessel` is fully implemented — resource credit to a named site, part-cost payout at efficiency, crew return, science banking, `VesselRecovered` contract trigger (`economy.rs:867–986`) — and **nothing in csp-ui/csp-view/csp-app issues it** (the only "recover" in view is `recover_control` = `ToBubble`, `csp-view/src/lib.rs:1917`). `SimCommand::TerminateVessel` is absent from the enum entirely (PLAN §3.5:1228: "`SimCommand::TerminateVessel` exists").

Consequences, all player-visible:
- A `SurfaceLatched` hull can never leave the world short of crash-destruction. The pad is a single berth (`spawn_site_occupied_by`, `lib.rs:5361–5409`; preflight refusal at `csp-view/src/lib.rs:2412-2418`) — a craft left latched on the pad refuses the next launch with "recover or move the parked vessel first", and **neither verb exists**: the only workaround is flying/crashing the old hull off the pad.
- `contract.rescue` is uncompletable — its predicate is keyed to `ContractTrigger::VesselRecovered` (`contracts.rs:979-981`), which only `apply_recover_vessel` emits (with a 120-day deadline and 8 000-fund penalty; see H5).
- The "flight recovery ──► funds" edge of §3.5's economy loop contributes nothing.

*Suggestion:* a "Recover vessel" action wherever a latched vessel is shown (Program window science section + a map/flight affordance), plus a `TerminateVessel` command for hulks. This is the single highest-leverage UI addition — the sim side is done.

**H2 — Labs, researchers, and crew are dead at three stacked layers — not merely hidden.**

- *No Lab part exists.* `ModuleDef::Lab` is in the schema (`csp-data/src/part.rs:615`) but no `data/parts/*.ron` uses it → `apply_start_research`'s "node carries no lab" gate (`research.rs:943-955`) can never pass — not even via MCP/script for an agent.
- *No crew can board.* `v.crew` is populated only by `BoardCrew` (`transfers.rs:982`) / `TransferCrew` — no UI for either — and `StartResearch` requires each `ResearcherAssign.crew` to be aboard (`research.rs:973`). `ResearcherAssign` is explicitly a stub ("the real crew arena ... is a later sub-phase", `research.rs:79-89`).
- *The roster is disconnected from seats.* `HireCrew` (`economy.rs:800-839`) has no UI and creates roster members `Unassigned`; `BoardCrew` allocates a **fresh id-only `CrewId` with no roster record** (`transfers.rs:999-1011`) — it is not the §3.5 "hire → board" flow, so even the MCP command surface cannot seat a hired member.

`tech.science.1` unlocks `facility.lab` and `tech.rocketry.3` unlocks `facility.test_stand` (`data/techtree/home.ron:34,67`) — facilities with no implementation or UI — so even the science spend advertises features that don't exist. The §3.2 "data × time × researchers" formula collapses to "data × nothing".

*Suggestion:* author a `Lab` part (researcher_slots>0, throughput, multiplier) behind `tech.science.1`, add Hire/Board UI (roster in Program window, seat assignment on the focused vessel), and validate `BoardCrew` against the roster rather than minting orphan crew.

**H3 — Colonies are completely invisible and unfoundable.**

A 5 168-line §3.5.1 implementation + one authored Rask colony (`data/systems/home/colonies.ron`, founded at boot via `found_authored_colonies`, `csp-view/src/lib.rs:1218`) — and **zero presentation**: no colony panel, no map marker, no HUD line, nothing in `csp-ui` (grep-verified). `FoundColony`, `AbandonColony`, `DamageColonyModule`, `JoinColonyInventory`, `LeaveColonyInventory`, `ScheduleColonyInspection` all have applies and no UI. The Rask colony runs its flow network correctly (tidally locked → permanent day → solar/drill/smelter until ore+metal stores saturate, then producers pin at 0 via the rule-3 sink check, `colonies.rs:867-910`) — invisibly, producing resources nobody can see or use.

Consequences:
- `contract.colony_resupply` is uncompletable: its predicate needs `TransferComplete` into colony stores (`contracts.rs:988-1007`, deliveries stamped at `transfers.rs:1047-1064`) — which needs `JoinColonyInventory`/`TransferResource` (no UI) *and* a vessel latched within 100 m of a colony the player can't see (and for drawn bodies `terrin`/`duva`, can't found at all).
- The §3.5 "resources ◄── colonies/ISRU" pull — the reason `Composite` is import-only and `ManufactureCraft` consumes site inventory — never engages.
- Colony `LifeSupport` module is authored-capable but uncrewed by construction (crew posting has no path either).

*Suggestion:* minimal viable exposure = a colony list/inspection panel (Program window tab or map overlay) + `FoundColony` verb at a site; resupply needs `JoinColonyInventory` on a latched vessel in radius. Without any of this the whole M8 milestone is dead weight to the player.

**H4 — Launch bypasses `ManufactureCraft`; the site-inventory/resource economy is inert.**

`ManufactureCraft` is a real reservation pipeline — BOM check+spend from `economy.sites` inventory, funds, `mass/rate` build time, `ConstructionComplete` delivery (`manufacture.rs:111-248`, delivery `:255-340`). The shipped launch path enqueues `SpawnPhysicalVessel` directly (`csp-view/src/lib.rs:2210-2220`): funds ARE charged (`lib.rs:5495-5499`), but no `build_resources` BOM is computed or checked and there is no build delay. Meanwhile `set_sites` creates **empty** site inventories (`economy.rs:648-658`; `sites.ron` authors no starting stock), and the only writers of site inventory are `RecoverVessel` (H1) and `AbandonColony` salvage (H3) — both unreachable (`TransferEnd` is `Vessel|Colony` only, `transfers.rs:200-206` — transfers can never deposit into a site).

So the authored `build_resources` on every part def (e.g., canister needs `composite` — `csp.science.canister.ron:17`, the "import-only" pull) is dead data: nothing consumes it anywhere a player can reach. The economy reduces to one currency (funds) with one income stream (contracts + a 10-funds/point science trickle).

*Suggestion:* either wire `ManufactureCraft` into launch (order → warp-friendly wait → latched delivery → `Unlatch`/throttle liftoff) and seed `terrin-launch` with a starting stockpile, or document the pad-debit as the intended v1 substitute — but then remove `build_resources` from part defs or surface it as display-only, since it currently promises a mechanic.

**H5 — Two of six contract templates offer contracts the player cannot complete; one is a penalty trap.**

- `contract.rescue` (`data/contracts/contract.rescue.ron`): bodies `[terrin, rask]`, predicate = `VesselRecovered(body)` — impossible via UI (H1). 120-day deadline → anyone who accepts it **guaranteed-fails for −8 000 funds**. It appears on the board after `tech.rocketry.3`.
- `contract.colony_resupply` (`data/contracts/contract.colony_resupply.ron`): needs colony deliveries (H3); no deadline, so it parks in `active` permanently (cap 32 — benign but dead).
- `contract.survey.data` draws bodies `[terrin, rask, duva]` — transmit counts as a return, so it's *technically* completable anywhere reachable, but rask/duva are beyond any realistic player flight today.

The board's prereq gating (`contracts.rs:676-683`) checks facts, not *player reachability* — templates whose completion verbs are UI-absent are offered as live content.

*Suggestion:* gate generation by verb reachability (drop rescue/resupply templates until H1/H3 land), or ship the verbs.

**H6 — The shipped starter craft and tutorial make the working science loop undiscoverable.**

`STARTER_CRAFT` is pod+tank+engine (`demo.rs:38`): no canister → "no experiment modules aboard"; no battery → `TransmitData` rejects "insufficient ElectricCharge" (`research.rs:797-803`) on any craft without `csp.battery.z1k`. The tutorial ends at orbit. The one tutorial-shaped path a player is likely to try — fly the stock craft and look for a science button — finds nothing; the loop requires the un-obvious combination *add canister in VAB → Program window → run → recover/transmit*.

*Suggestion:* ship the starter craft with canister + battery; add a tutorial step ("run the experiment, then transmit or land and recover"); give the transmit row a disabled state + reason when EC is short.

### MEDIUM

**M1 — The offer board shows only `#index template-id`; active contracts show no deadline or progress.**

`stage_program_overview` renders `format!("#{} {}", o.index, o.template.as_str())` (`csp-view/src/lib.rs:2939-2949`). The drawn params (orbit band, body, min_units, amount), rewards, deadline, and days-remaining are all invisible; `active` is the same string. A player accepts blind and is penalized blind — deadline expiry (`fire_contract_deadline`, `contracts.rs:1151-1184`) charges the penalty with no countdown ever shown.

*Suggestion:* render kind-specific summaries from `params` (they're in the mirror already — just unformatted), plus reward/deadline fields; `ContractParams` → text is a ~60-line match.

**M2 — The tech tree gates only 2 real parts and spends most of its content on dead unlocks.**

4 nodes, 5 tracks (`data/techtree/home.ron`). Parts actually gated: `csp.tank.s320` + `csp.engine.ember` (tech.rocketry.2); everything else is tech.start. `tech.science.1` → `facility.lab` (no lab exists) + `contract.survey.data` offers + one track cap; `tech.rocketry.3` → `facility.test_stand` (`RunBenchTest` unreachable) + part_test (completable) + **rescue + colony_resupply (uncompletable, H5)** + track caps. The §3.3 mechanics (frozen `upgrade_levels_at_build` at placement — `csp-ui/vab.rs:1637-1665`, `tech.rs:88-130`; spawn gate `check_part_gate`, `tech.rs:271-291`; VAB palette gating `csp-ui/vab.rs:745-751,965-972`) are all correct — the *content* is the thin part.

Also note the starter craft (`pod+s160+sparc`, ~4 t wet, Isp 320) has ≈2 075 m/s Δv — it **cannot reach orbit** (Terrin LEO ≈ 2 287 m/s + losses); orbit becomes comfortable only with the s320/ember parts tech.rocketry.2 unlocks — the same node that unlocks `contract.satellite` (pe 80–250 km, apo ≤1.5 Mm). Conveniently consistent gating, but the "3-part rocket" answers: it satisfies *no* contract — even `contract.survey` needs a 4th part (the canister).

**M3 — The economy has no reachable recurring drain; §3.5's measured target is vacuous.**

`SalaryDue` publishes only while an `Active` roster member exists (`economy.rs:727-736`) — hiring is unreachable (H2) → salaries never fire → the "2–5% of funds per sim-month" drain band has no way to be exercised. `Insolvent` is reachable only through contract penalty clamps (`charge_funds`, `economy.rs:686-708`). Facility upkeep doesn't exist at all (no facilities). Net: the economy is single-currency, single-income (contracts), launches as the only sink.

**M4 — Pad science is degenerate and the starter economy is exploitable.**

The launchpad is a valid `Landed` context: a craft that never moves can run the canister, and `RecoverData` banks 100% (~8.5 science first run). N canisters on one craft multiply this (soft cap is per-key, spent flag is per-(node,exp)). At boot all 6 board slots draw the only eligible template — six identical `contract.survey` offers (`publish_contracts`, `contracts.rs:730-767`; only `TechResearched("tech.start")` is prereq-met at boot). Accepting several and doing **one** pad recovery completes all of them — ~42 000 funds for never leaving the pad. This is consistent with the spec's letter (KSP-style biome farming) but means the first "contract" tier requires zero flight.

Separately, `apply_recover_data` closes the key **program-wide** (`research.rs:871-911`) — after one pad recovery, `Landed@<pad biome>` is permanently spent; further pad runs reject "key closed". Per-biome keys exist (biomes share the authored cap but each gets its own `collected` counter — `research.rs:734-738`) yet reaching another Terrin biome needs a survivable off-pad landing.

**M5 — Authored Flying* situation caps are dead content.**

`exp.materials_study` caps include all six situations (`data/experiments/...:14`, mask 63), but the only instrument's mask is 49 (`LANDED|SPACE_LOW|SPACE_HIGH`) — `FLYING_LOW`/`FLYING_HIGH`/`SPLASHED` keys can never produce data. Harmless dead rows, but they overstate content coverage.

### LOW

- **L1 — Transmit button has no EC feasibility gate** (`program.rs:427`): always clickable; the drain reject lands as a generic "COMMAND REFUSED — TRANSMIT: INSUFFICIENT ELECTRICCHARGE" cue (`csp-app/src/lib.rs:944-956`). Honest after the fact; the promised "+X⚛" yield label invites the dead click.
- **L2 — `ContractReward` is a bare stub** (`economy.rs:846-857`, doc "the M7-D stub interface") — no production caller; MCP-only debug verb.
- **L3 — `BoardCrew` mints orphan crew** (`transfers.rs:982-1011`): allocates a fresh `CrewId` with no roster record, so the roster↔seat invariant treats it as id-only (skipped by `reseat_roster`, `economy.rs:566-579`). Even scripted agents cannot perform §3.5's hire→board.
- **L4 — Colony inspections never fire in the shipped game:** `inspection_tick` is `None` at founding (`colonies.rs:1697`) and only `apply_schedule_inspection` (`colonies.rs:1979+`, `ScheduleColonyInspection` — no UI, and no authored cadence) sets it. The rule-4 inspection deadline machinery is exercised only by tests.
- **L5 — Duplicate offer spam at boot** (see M4): 6 identical survey offers; weighting has no variety until tech unlocks.
- **L6 — `RunExperiment` has no debris/aliveness gate** (`research.rs:668-761`): a debris fragment retaining an unspent canister node can legally run experiments; also no `VesselState` aliveness check beyond the residence class.

## Verified good (no action)

- **§3.2 ledger is exact** — the soft cap applies the integral `C′ = C + (cap−C)(1−exp(−k·u/cap))`, reproducing the spec's 63/86/95% sequence (`research.rs:14-24, 734-738`); transmitted-key remainder handled per-key; recovery refuses while other vessels/projects hold the key (the restore-invariant-safe close, `:871-892`).
- **Lab math is the spec's two-regime closed form** with the `data_epsilon` exhaustion fix and the same-tick Exhausted→Complete event pair (`research.rs:304-367, 1039-1048`); interruption rules for jettison/breakup/merge documented and implemented (`:440-597`).
- **Economy uses integer micro-funds** with a bounded ledger, action-vs-drain rejection split, single-fire `Insolvent` (`economy.rs:97-130, 468-519, 686-708`); `RecoverVessel` is compute-then-commit atomic.
- **Contract generation is seed-deterministic** (`{CONTRACTS,0,offer_index}` KDF, `contracts.rs:676-711`), prereqs evaluate real facts, completion is event-triggered never polled, deadline events pair 1:1 with records, accept-time baseline re-base closes the cumulative-fact faucet (`contracts.rs:779-800, 840-871`).
- **Program window is reachable from every mode** via the always-drawn top bar and surfaces run/transmit/recover/tech/tracks/contracts/reliability/notices (`csp-view/src/lib.rs:443,551-572,2485-2739`; `csp-ui/src/program.rs`).
- **Rejects are player-visible**: any `SourceClass::Ui` command rejection lands a `COMMAND REFUSED` cue (`csp-app/src/lib.rs:944-956`).
- Colony flow network honors all five §3.5.1 rules including pro-rata allocation, sink pinning, `Dormant`/`Abandoned` costing nothing, and the `PowerRestored`-only exit from `Degraded` (`colonies.rs:1-122`).

## Suggested fix order (minimum playable loop)

1. **H1** — add a `RecoverVessel` verb on latched vessels (Program window + flight HUD) and a `TerminateVessel` command; unblocks the pad-berth trap, the rescue contract, and the recovery-funds leg in one move.
2. **H6** — starter craft gains canister + battery; tutorial gains a science step; EC-gate the transmit button (L1).
3. **H5/M1** — gate uncompletable contract templates from generation; render offer params/rewards/deadlines.
4. **H2** — Lab part + roster-aware board UI — the "researchers" complaint.
5. **H3** — colony visibility panel; `JoinColonyInventory`/`TransferResource` verbs; then `FoundColony`.
6. **H4** — either wire `ManufactureCraft` into launch or explicitly scope it out (and strip `build_resources` as dead data).
