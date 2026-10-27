# Verified Report C33 — verification of review-report-C33.md

Verifier: independent re-check against the working tree at
`/home/didi/workspace/Clanker-Space-Program` (HEAD past 6cc2cb4). Every file, line, and
numeric claim re-opened and re-derived; sim code paths traced to their writes; data tests
re-run. No files edited other than this report.

Tests re-run: `cargo test -p csp-data --test parts --test techtree --test balance` —
all green (6 + 2 + 5 = 13 tests, not 10 as the report header said; trivial undercount,
claim direction correct).

---

### Finding 1: Mid-game economy dead-end — CONFIRMED

**Original**: The only paths that ever credit a launch-site inventory are `RecoverVessel`
(tank contents only) and `AbandonColony` salvage at 25%; no tank holds ore/metal/composite;
no recipe produces composite; `ManufactureCraft` hard-rejects on short stock; pad starts
2000 metal / 400 composite vs a 163-metal starter craft → ~12-launch fuse; colony_resupply
offers drawing ore are uncompletable.

**Verification** — every leg re-derived independently:

(i) **No cargo tank.** Exhaustive `Tank(` scan of all 16 parts in `data/parts/`: exactly 4
Tank modules — `csp.tank.s160.ron:51` (liquid_fuel/oxidizer), `csp.tank.s320.ron:50`
(liquid_fuel/oxidizer), `csp.battery.z1k.ron:46` (electric_charge),
`csp.tank.life.s.ron:53` (oxygen/food). Nothing in the catalog can hold ore, metal,
composite, or water. Confirmed.

(ii) **No composite recipe.** All 3 recipes in `data/recipes/`:
`recipe.smelt_ore.ron` (ore→metal 0.6), `recipe.isru_propellant.ron` (ore→fuel+oxidizer),
`recipe.electrolysis.ron` (water→oxygen). Composite has zero producers anywhere. Confirmed.
(Water also has no producer, but it is not a build resource.)

(iii) **Site-inventory credit paths — exhaustive.** `rg` over csp-sim for writers of
`economy.sites[*].inventory` finds exactly two: `crates/csp-sim/src/economy.rs:963-966`
(recovery) and `crates/csp-sim/src/colonies.rs:1733-1736` (AbandonColony).
- Recovery credits `recovery_resources(v, efficiency)` (`economy.rs:534-547`) which iterates
  `vessel.tank_contents` only — parts' embodied `build_resources` are never refunded, and no
  tank holds metal/composite anyway. (Report's cite `economy.rs:919-928` lands inside
  `apply_recover_vessel`, the caller; the helper is at 534 — same code path, immaterial.)
- `AbandonColony` salvages 25% of `colony.stores` into **the colony's own site**
  (`colonies.rs:1732-1736`). `ManufactureCraft` requires `def.launch == true`
  (`manufacture.rs:134-137`); the only `launch: true` site in
  `data/systems/home/sites.ron` is `terrin-launch`. So rask-mines metal (the authored
  smelter colony) can never fund a build, and abandoning rask-mines credits rask-mines.
- `TransferEnd` is `Vessel | Colony` only (`crates/csp-sim/src/transfers.rs:193-209`); every
  colony-touching transfer command (`TransferResource`, `TransferColonyResource`,
  `crates/csp-sim/src/lib.rs:797-825, 1058-1076`) requires a **vessel** endpoint. Colony
  metal cannot board a vessel (no tank capacity → the reservation rejects), and there is no
  colony→site end at all. Confirmed.
- Contracts: `ContractRewards` (`crates/csp-data/src/contract.rs:363-372`) is
  advance/completion/failure **funds only**; `apply_contract_reward` (`economy.rs:859-867`)
  moves funds only. No funds→resources market exists (grep for buy/purchase/restock: only
  test hits).

(iv) **Launches-to-brick — my own arithmetic.** Per-part `build_resources` summed for the
Stock Hopper (`csp-view/src/demo.rs:53-59`: pod+canister+battery+s160+sparc):
metal = 60+8+3+22+70 = **163**, composite = 0+4+2+0+10 = **16** — matches the report
exactly. Metal: 2000/163 = 12.27 → **12 full builds** (remnant 44 metal; a 13th
minimum-propulsion craft — ember 30 + probe 10 + battery 3 = 43 — can be squeezed in, so
the fuse is 12-13, "~12" is right). Composite: 400/16 = 25 builds → **metal binds first**.
Bench tests drain 1 metal/unit on top (`balance.ron: reliability_bench_resource = "metal",
1.0/unit`). A two-stage orbital stack (~220-250 metal: probe+2×s320+ember+decoupler+legs+
sparc+s160) gives 2000/250 ≈ 8 — matches the report. After exhaustion,
`manufacture.rs:199-205` rejects "insufficient site inventory" permanently. Confirmed.

(v) **Any other un-brick path?** Checked contracts (funds only, above), colony export
(no transfer end, above), UI, and MCP. The UI "found outpost" button
(`csp-ui/src/program.rs:1390-1397`) appears at any colony-less site including terrin-launch,
but `starter_colony_def` (`csp-view/src/lib.rs:1296-1318`) builds **solar + battery + 250 EC
only** — no drill, no smelter, no metal store; abandoning it credits only ~62.5 EC. Colony
modules are fixed at founding (no add-module SimCommand exists). The one technical hole:
the MCP surface's `sim.command` raw passthrough (`csp-server/src/mcp.rs:815-822`) can inject
an arbitrary `FoundColony` def with founding metal/composite store levels at terrin-launch,
then `AbandonColony` credits 25% — repeatable and free (founding charges nothing). That is
an undocumented agent escape hatch that doubles as an infinite-resource faucet, not a
designed or sanctioned refill mechanic; it does not un-brick the designed loop, it confirms
the loop has no content-side wiring.

**Secondary claim (ore resupply uncompletable)**: `contract.colony_resupply` draws
resources `["ore","food","oxygen"]` (the draw is at
`data/contracts/contract.colony_resupply.ron:17`, not :15 — trivial line-number slip).
The delivery fact `colony_deliveries` is written **only** in the `TransferComplete` handler
(`transfers.rs:1061-1086`) when cargo lands in a live colony's stores; colony drill
accumulation never fires it. Since every colony transfer needs a vessel endpoint and no
vessel tank holds ore, ore can never be delivered — even via MCP raw commands (a transfer
of ore credits 0 into a tankless vessel). 1/3 of draws are permanently uncompletable.
Confirmed.

**Verdict: CONFIRMED.** The §3.5 "resources ◄── colonies/ISRU" edge is unwirable with this
data; the pad's 2000/400 is a ~12-launch fuse with no designed recovery; ore resupply
offers are dead on arrival.

### Finding 2: Planes/rovers absent with no §6.4 sanction — CONFIRMED

**Original**: Pillar 1 promises "rockets, planes, rovers, stations"; the catalog has no
wing/wheel/air-breather/intake; §6.4's cut list does not sanction the cut; no §6.5 decision
recorded.

**Verification**:
- PLAN.md:12 (verbatim): "**Build anything** — rockets, planes, rovers, stations, from a
  data-driven part graph."
- §6.4 "Cut list — reordered" (PLAN.md:1543-1562) contains exactly 5 items: (1) R3 advanced
  render tier → post-v1, (2) life support → post-v1, (3) Duva → post-v1, (4) orbital
  docking M6 → post-v1 **with proximity transfer retained**, (5) web tier polish. No item
  mentions planes, rovers, wings, wheels, or lift. No other section sanctions their
  absence (grep over PLAN.md + docs/ for wing/wheel/rover/plane finds nothing beyond
  pillar 1 and incidental uses, e.g. PLAN.md:507 assumes "a rover sits" in a warp rule —
  the plan still presupposes rovers).
- §6.5's ritual (PLAN.md:1565-1571) requires cuts be "Record the decision in
  `docs/quarterly/`" — **`docs/quarterly/` does not exist** (docs/ contains only
  ARCHITECTURE/BUILD-AND-DEV/DATA-AND-SCRIPTING/GAMEPLAY-GUIDE/SIMULATION/UI-AND-RENDERING,
  perf/). No recorded decision anywhere.
- Data: no wing/lifting-surface/wheel/drive/intake part among the 16. Stronger than the
  report states: the **schema itself cannot express them** — `ModuleDef`
  (`crates/csp-data/src/part.rs:506+`) has variants Engine, Tank, Control, DockPort,
  CrewSeat, Experiment, Drill, Converter, Lab, Parachute (plus deployable/legs handling);
  no Wing/LiftingSurface/Wheel/GroundDrive exists, so "just author the data" is not even
  possible without a schema change.
- `cl_curve` claim re-checked across all 16 files: nonzero (0.05) only on
  `csp.pod.mk1.ron:26` and `csp.lab.mk1.ron:38`; every other part authors 0.0. Confirmed.
- Stations half: dockport exists (tech.rocketry.3), lab exists (tech.science.1); §6.4
  item 4 sanctions the docking-UI deferral while explicitly retaining proximity transfer,
  so "PARTIAL — sanctioned" for stations is fair. Planes/rovers: MISSING, unsanctioned.

**Verdict: CONFIRMED.** Half of pillar 1 is unbuildable, and it is a schema-level gap, not
merely missing data files.

### Finding 3: `unlocks.facilities` / `unlocks.contract_types` are dead data — CONFIRMED

**Original**: No production code reads either list; only `level_cap` over `track_levels`
consumes `unlocks`; `RunBenchTest` gates on site/funds/metal but not tech, so the
test stand is usable from tick 0 despite `facility.test_stand` at rocketry.3.

**Verification**:
- rg across csp-sim, csp-ui, csp-view, csp-app for `.facilities` / `.contract_types`:
  the only readers are in `crates/csp-data/src/validate.rs` (3776-3782 well-formedness,
  4238-4247 one-directional "unlock names an authored template" check) and a test
  constructor in `crates/csp-sim/src/tech.rs:433-434` (inside a `#[test]` tree builder).
  No gameplay consumer. The only production consumer of `TechUnlocks` is
  `TechTree::level_cap` over `track_levels` (`crates/csp-data/src/techtree.rs:236-247`).
  Confirmed. `FacilityId` in the sim is only `CrewLocation::Facility`
  (`economy.rs:271-273` region), a location tag — not a gate.
- `apply_bench_test` (`crates/csp-sim/src/reliability.rs:1166-1230`) gates on: config
  validity, units > 0, known site, `def.launch`, site state row, funds, and site metal
  inventory. **No tech/research check.** The UI's bench buttons gate on the same economics
  only (`csp-ui/src/program.rs:1473, 1590`). So the stand is usable from tick 0 while
  `data/techtree/home.ron` authors `facility.test_stand` at tech.rocketry.3 (line 67).
  Confirmed.
- Contract offering gates on each template's own `prerequisites`
  (`contracts.rs:660-664, 789, 824`), with `TechResearched` reading the researched set
  (`contracts.rs:414`) — a second, parallel source of truth; the validator checks only
  that unlock ids resolve to authored templates ("the other direction is free",
  validate.rs:4124-4125), so silent divergence is possible. Confirmed.

**Mitigating context (does not dismiss)**: `TechUnlocks`' doc comments
(`techtree.rs:127-132`) declare the two fields "opaque until the facility economy
milestone / the contracts milestone; the validator checks well-formedness only" — i.e.
this is deliberate forward-declared scaffolding, not an accidental wire-forget. But the
contracts milestone has since landed (M8-C, contracts.rs) and still does not read
`contract_types`, and the bench stand is concretely free-tier contrary to the authored
gate. The finding stands at MEDIUM.

**Verdict: CONFIRMED** (with the "intentional scaffolding" nuance noted).

### Finding 4: `max_tracked_vessels: u64::MAX` voids §3.5's object-cap guarantee — CONFIRMED

**Original**: PLAN §3.5 mandates `max_tracked_vessels = 512` for bounded growth; the data
ships 18446744073709551615 with an honest comment.

**Verification**:
- PLAN.md:1222-1226 (§3.5 "Economy loop", verbatim): "Object caps and debris policy (the
  sim and save must not grow without bound over 100 sim-years of launches). In
  `balance.ron`: `max_tracked_vessels = 512`, `max_debris = 64`." Line 1231: "The economy
  harness asserts save size after 100 sim-years ≤ 32 MB."
- `data/balance.ron:15`: `max_tracked_vessels: 18446744073709551615` (= u64::MAX) with
  comment "the vessel arena is effectively unbounded". `max_debris: 64` stays.
- The cap is real and load-bearing when set: spawn/undock/manufacture all reject at
  `caps.max_vessels` (`csp-sim/src/lib.rs:5814, 6013, 7291, 11415`), and the data value
  flows in via `csp-view/src/lib.rs:1783` (`max_vessels: balance.max_tracked_vessels`).
  Additionally the sim's own default is also u64::MAX
  (`lib.rs:1619 DEFAULT_MAX_VESSELS`), so nothing anywhere restores 512.
- Extra drift found while verifying: `balance.ron:13` and `crates/csp-data/src/balance.rs:74`
  both cite "PLAN §2.9" for the object caps; the object-caps text lives in **§3.5**
  (§2.9 is the physics backend). Trivial, but the report's §3.5 cite is the correct one.

**Verdict: CONFIRMED.** The normative 512 and the shipped u64::MAX disagree in the file
whose header says "ALL tuning lives here", with no recorded deviation.

### Finding 5: Stale header comments contradict their own attach nodes — CONFIRMED

**Original**: battery header says "0.625 m node class" while its nodes are 1.25; probe
header says "0.625-class stack nodes" while its nodes are 1.25.

**Verification**: `data/parts/csp.battery.z1k.ron:7` — "0.625 m node class — mounts under
pods and small structures." vs `:21-22` `diameter_m: 1.25` (whose inline comment
"1.25-class stack interfaces — the canister precedent" is correct).
`data/parts/csp.probe.s0625.ron:8-9` — "0.625-class stack nodes match the small-part size
class" vs `:23-24` `diameter_m: 1.25` (inline comment again correct). The data is right;
only the prose headers mislead. Confirmed as a doc-drift nit.

**Verdict: CONFIRMED.**

### Finding 6: `sites.ron` comment claims pad recovery "refills" the stockpile — CONFIRMED

**Original**: The comment above `starting_stock` says recovery at the pad refills it, but
recovery credits funds + tank contents only; embodied build_resources never return.

**Verification**: `data/systems/home/sites.ron:44-45` — "~12 starter-craft equivalents of
metal plus composite for the electronics-heavy parts; recovery at the pad (efficiency 1.0)
refills it." As established under Finding 1: `recovery_resources` (`economy.rs:534-547`)
credits tank contents × efficiency only; no tank holds metal/composite; embodied
`build_resources` are never refunded. The comment documents a mechanic that does not exist
— and it is exactly the mechanic that would close Finding 1's loop.

**Verdict: CONFIRMED.**

---

## PRD-coverage table spot-checks (requested)

- **Starter craft numbers**: dry mass 800+100+40+220+900 = 2060 kg; propellant 360 units ×
  5 kg = 1800 kg; wet 3860 kg. Δv = 9.81 × 320 × ln(3860/2060) ≈ **1.97 km/s** (report
  exact). Sea-level liftoff TWR = (200 kN × 280/320) / (3860 × 9.81) ≈ **4.6** (report's
  4.5 fair; the demo comment's "≈ 5" uses vac thrust — 5.28). All WIRED-row numbers check.
- **Funds pacing**: `starting_funds_micros: 1_000_000_000_000` = ₣1,000,000 ✓; survey =
  ₣1,000 advance + ₣6,000 completion = ₣7,000 ✓ (`contract.survey.ron`).
- **Science pacing**: caps terrin 30 / rask 60 / duva 90 / solen 50, masks 63/63/63/48 ✓
  (`exp.materials_study.ron`). World science ≈ 640 reproduces exactly as
  terrin 6×30 + rask 6×60 + solen 2×50 = 640 (Duva excluded, post-v1 per §6.4) vs tree
  cost 15+30+60 nodes (105) + 209 track-level science = **314** ✓. Headroom claim holds.
- **Demo parity (§6.3)**: `diff` of `data/` vs `apps/demo/data/`: parts/, balance.ron,
  resources.ron, recipes/ **byte-identical** (only a `.gitkeep` marker extra in
  `data/parts/` — not a part file). techtree differs only in commented §6.3 reductions
  (survey.data / rescue / colony_resupply contract_types removed with rationale comments);
  the `demo_files_match_disk` guard exists in `csp-view/src/demo.rs`. PASS claim verified.
- **Map correction**: `exploration-map.md:48,62,282` says "9-node techtree"; the actual
  `data/techtree/home.ron` has **4 nodes + 5 tracks**. Report's correction is right.
- **Loc/ATTRIBUTION rows**: not re-audited line-by-line (tests green, loc key tests pass);
  no contradicting evidence seen.

## Score

6 confirmed / 0 dismissed: (1) economy dead-end HIGH, (2) planes/rovers unsanctioned HIGH,
(3) dead unlock lists + free-tier bench MEDIUM, (4) u64::MAX vessel cap MEDIUM,
(5) stale part headers NIT, (6) false sites.ron comment NIT.
