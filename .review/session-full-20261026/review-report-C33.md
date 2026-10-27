# Review Report C33 — Content: parts catalog + tech + recipes + rules

Session `session-full-20261026`, working tree at HEAD past `6cc2cb4`. Every assigned file read
in full: 16 × `data/parts/*.ron`, `data/techtree/home.ron`, `data/experiments/exp.materials_study.ron`,
3 × `data/recipes/*.ron`, `data/resources.ron`, `data/balance.ron`, `data/loc/en.ron`,
`data/ATTRIBUTION.md`; demo tree diffed against `apps/demo/data/`; economy math traced through
`csp-sim/src/{economy,manufacture,research,reliability,transfers}.rs` and
`csp-view/src/{demo.rs,lib.rs}` (starter craft), `csp-ui/src/program.rs` (hire salary).
`cargo test -p csp-data --test parts --test techtree --test balance` passes (10 tests).

Map correction: `data/techtree/home.ron` is **4 nodes + 5 tracks**, not "9 nodes" (exploration-map
§1 line "9-node techtree" over-counts; the 9 is nodes+tracks).

---

### [HIGH] Finding 1: The site-resource economy is a dead-end budget — after ~12 launches the pad can never build again
**File**: `data/parts/*.ron` (catalog-wide), `data/recipes/recipe.{smelt_ore,isru_propellant,electrolysis}.ron`, with `data/systems/home/sites.ron:46-49` (starting_stock — C21/C34 seam)

**Problem / Evidence**: The only paths that ever *credit* a launch-site inventory are
`RecoverVessel` (tank contents only — `csp-sim/src/economy.rs:919-928`, `recovery_resources`
iterates `tank_contents`, never parts' embodied `build_resources`) and `AbandonColony` salvage
at 25% (`colonies.rs:1733-1737`). `TransferEnd` is `Vessel | Colony` only (`transfers.rs:203-211`)
— there is no colony→site transfer, no funds→resources market, and `ManufactureCraft` hard-rejects
on short stock (`manufacture.rs:193-203`). Meanwhile the 16-part catalog contains **no Tank with
capacity for `ore`, `metal`, `composite`, or `water`** (only `liquid_fuel`/`oxidizer` on
`s160`/`s320`, `oxygen`/`food` on `tank.life.s`, `electric_charge` on `battery.z1k`), and the 3
recipes produce only `metal`, `oxygen`, `liquid_fuel`+`oxidizer` — **no recipe produces
`composite`**, ever. §3.5 promises "advanced parts need Composite, which is **import-only at
first** → creates the pull into the colony game" — the "at first" never ends: colony metal and
composite physically cannot reach the pad (no carrying part, no transfer end), so the pull ends in
a wall.

**Impact**: On paper — pad starts 2000 metal / 400 composite. The seeded Stock Hopper
(pod + canister + battery + s160 + sparc) costs 163 metal + 16 composite → **12 launches**; a
mid-game orbital craft (s320+ember+decoupler+legs) ≈ 200-250 metal → **8 launches**; bench tests
drain 1 metal/unit on top. Composite: lab craft ≈ 33-46 each → ~10-15 total. After exhaustion
`ManufactureCraft` rejects "insufficient site inventory" forever; funds (start ₣1,000,000, survey
contract ₣7,000 each) are irrelevant because nothing converts funds back into stock. The game
bricks for building with no recovery mechanic — §3.5's "resources ◄── colonies/ISRU" edge is
unwirable with this data. Secondary consequence: `contract.colony_resupply` draws
`resources: ["ore","food","oxygen"]` (`data/contracts/contract.colony_resupply.ron:15`) — **no
part can carry ore**, so any offer that draws `ore` (1/3 of draws) is uncompletable
(no `TransferComplete` can ever land ore in a colony).

**Suggestion**: Ship one small cargo part (`Tank(capacity: [("ore", N), ("metal", N),
("composite", N)])` — §3.5: "A Cargo bay is just a Tank") and either a composite recipe or a
composite-rich site yield (Rask), then wire a colony/vessel→site credit edge (or make recovery at
the launch site refund `build_resources` × efficiency — which is also what
`sites.ron:45`'s comment *claims* already happens). Alternatively resize `starting_stock` to the
intended full-campaign BOM and document the finite budget. (Transfer-end seam itself is C09/C21;
the data gap is C33.)

### [HIGH] Finding 2: Pillar-1 content — planes and rovers absent with no §6.4 sanction
**File**: `data/parts/` (catalog-wide absence)

**Problem / Evidence**: Pillar 1 (PLAN.md:12): "**Build anything** — rockets, planes, rovers,
stations, from a data-driven part graph." The catalog has legs, probe core, dockport, lab
(station-ish half) — but **no wing/lifting-surface part, no wheel/ground-drive module, no
air-breathing engine, no intake**; `cl_curve` is authored nonzero only on pod/lab (0.05) and
nothing generates lift or ground drive. §6.4's cut list (render tier, life support, Duva, orbital
docking M6, web polish) does **not** sanction cutting plane/rover parts, and the cut trigger
(§6.5) has no recorded decision for them.

**Impact**: Rockets: WIRED. Stations: PARTIAL (dockport exists; docking UI post-v1 per §6.4
item 4 — sanctioned). **Planes: MISSING. Rovers: MISSING.** Half of pillar 1 is unbuildable in
data, unsanctioned.

**Suggestion**: Either author the minimum rover/wing content (one wheel family + one wing family
is enough to make "rovers/planes" true in a data-driven sense), or record the deferral in
`docs/quarterly/` per §6.5's ritual so the pillar-1 gap is a decision, not an omission.

### [MEDIUM] Finding 3: Tech tree `unlocks.facilities` and `unlocks.contract_types` are dead data — the authored gates gate nothing
**File**: `data/techtree/home.ron:22,34,49,67` (facilities), `:23,35,50,68` (contract_types)

**Problem / Evidence**: No production code reads either list — the only consumer of `unlocks` is
`TechTree::level_cap` over `track_levels` (`csp-data/src/techtree.rs:240`). `facility.launchpad`,
`facility.vab`, `facility.lab`, `facility.test_stand` appear nowhere outside this file (one test
helper in `validate.rs:5604` aside); `FacilityId` in the sim is only a crew *location* tag
(`economy.rs:271-273`). `RunBenchTest` (`reliability.rs:1168-1201`) gates on site/funds/metal but
**not** on `tech.rocketry.3` — so the test stand the tree says unlocks at Advanced Rocketry is
usable from tick 0. Contract offering is gated by each template's own `prerequisites`
(`contracts.rs`), duplicating `contract_types` as a second source of truth (currently in sync).

**Impact**: Data authored to gate, code never reads it — the inverse of the "coded but not wired"
rule. A modder/author raising `facility.test_stand` to a deeper node changes nothing in-game;
the two contract-unlock lists can silently diverge. The bench stand being free-tier also lets a
day-one player burn starting metal on qualification before any tech.

**Suggestion**: Either wire the gates (bench apply checks the researched set; offer generator
reads `contract_types`) or strip both fields from the schema and let part `tech_requirement` +
contract `prerequisites` be the single sources of truth. If kept, add a validator rule that
cross-checks `contract_types` against template prerequisites so drift fails CI.

### [MEDIUM] Finding 4: `max_tracked_vessels: u64::MAX` voids §3.5's object-cap guarantee
**File**: `data/balance.ron:15`

**Problem / Evidence**: PLAN §3.5: "Object caps … In `balance.ron`: `max_tracked_vessels = 512`,
`max_debris = 64`" so that "the sim and save must not grow without bound over 100 sim-years of
launches" (the same section mandates the ≤32 MB save-size harness assertion). The data sets
`max_tracked_vessels: 18446744073709551615` with an honest comment ("the vessel arena is
effectively unbounded").

**Impact**: The PRD's bounded-growth protection for *vessels* is turned off by tuning while the
PRD text still promises it; only debris (64) stays capped. Every `ManufactureCraft` order can add
a live vessel forever — the 100-sim-year save-size property now rests on player behavior, not on
a cap.

**Suggestion**: Restore 512 (or author a number the save-size harness actually asserts), or amend
the deviation through a recorded decision; as-is the normative §3.5 number and the shipped data
disagree in the file whose header says "ALL tuning lives here".

### [NIT] Finding 5: Stale header comments contradict their own attach nodes (battery, probe)
**File**: `data/parts/csp.battery.z1k.ron:7`, `data/parts/csp.probe.s0625.ron:8-9`

**Problem / Evidence**: battery header: "0.625 m node class — mounts under pods…" while its nodes
are `diameter_m: 1.25` (`:21-22`, whose own inline comment is correct); probe header: "0.625-class
stack nodes match the small-part size class" while nodes are `diameter_m: 1.25` (`:23-24`).

**Impact**: The *data* is right (the 1.25 inline canister precedent is applied); the headers
would mislead the next author copying a "0.625" mount convention. Data-doc drift only.

**Suggestion**: Fix the two header lines to say 1.25-class stack nodes (canister precedent).

### [NIT] Finding 6: `sites.ron` comment claims pad recovery "refills" the stockpile — it does not
**File**: `data/systems/home/sites.ron:45` (comment above `starting_stock`; file owned by C34, flagged for the false balance claim)

**Problem / Evidence**: "recovery at the pad (efficiency 1.0) refills it" — recovery credits
funds (part cost × efficiency) plus **tank contents** only (`economy.rs:919-928`); embodied
`build_resources` metal/composite are never refunded, and no stock tank carries them.

**Impact**: The comment documents exactly the mechanic (Finding 1) that would prevent the economy
dead-end — a reader will believe the loop is closed when it is not.

**Suggestion**: Correct the comment when Finding 1 is resolved (either direction: implement the
refund or state the budget is finite).

---

## PRD coverage table (pillar 1 / §3 data-driven content)

| PRD item | Status | Evidence |
|---|---|---|
| Rockets (build/fly stack, staging, scaling) | **WIRED** | 16 parts; s160 Continuous + adapter Discrete scaling; Δv/TWR coherence tests green |
| Starter craft buildable at tech.start | **WIRED** | Stock Hopper = pod+canister+battery+s160+sparc, all `tech.start` (demo.rs:53-59); Δv ≈ 1.97 km/s (suborbital), liftoff TWR ≈ 4.5 |
| Stations (dockport, lab) | **PARTIAL — sanctioned** | dockport tech.rocketry.3, lab tech.science.1; docking UI post-v1 per §6.4 item 4 (parallel-known, C10) |
| Planes (wings/lift/jets) | **MISSING — unsanctioned** | Finding 2 |
| Rovers (wheels/drive) | **MISSING — unsanctioned** | Finding 2 |
| §3.3 tech tree (nodes, tracks, at-manufacture freeze) | **WIRED** | 4 nodes + 5 tracks, monotonic costs, all 16 parts reachable, all track levels capped-reachable; validator green |
| §3.2 science economy | **WIRED** | exp caps terrin 30 / rask 60 / duva 90 / solen 50; canister mask 49 consistent; world science ≈ 640 pts vs ≈ 314 total tree cost — comfortable headroom |
| §3.5 manufacture loop | **DEGRADED** | Funds+time+stock debit all work, but stock is unrenewable (Finding 1) |
| Composite "import-only **at first**" | **DEGRADED** | No composite recipe/yield exists (Finding 1) |
| §3.5 resources v1 set (ten) | **WIRED** | resources.ron matches the PRD ten verbatim; monoprop unused by any part but PRD-mandated |
| §3.5 cargo bay ("just a Tank") | **MISSING** | No ore/metal/composite/water-capacity part (Finding 1) |
| §3.6 v1 contract types | **PARTIAL** | 6 of 7 PRD types (crew transport, tourist flight absent; colony_resupply = base resupply); no §6.4 sanction either way — note only (templates are C34) |
| Localization (data keys) | **WIRED** | en.ron covers all 16 parts, 5 tracks, 4 nodes, 6 contracts, 3 recipes, sites, experiment; loc tests green |
| ATTRIBUTION §3.7 | **WIRED** | Format + allowlist honored; cc0 ambientCG rows, procedural parts authored |
| Demo-tree parity (§6.3) | **WIRED** | parts/balance/resources/recipes byte-identical; techtree/experiment/loc reductions intentional, commented, and guarded by `demo_files_match_disk` + loc gate tests |

## Top 3 issues

1. **Finding 1 (HIGH)** — resource logistics is unwirable in data: no cargo part, no composite
   recipe, no site-refill edge → the pad's 2000 metal / 400 composite is a ~12-launch fuse, and
   `colony_resupply` offers drawing `ore` are uncompletable.
2. **Finding 2 (HIGH)** — pillar 1's planes/rovers have zero data content with no §6.4/§6.5
   sanction recorded.
3. **Finding 3 (MEDIUM)** — the tech tree's facility/contract-type unlocks are authored but never
   read; the test stand is usable from tick 0 despite being "unlocked" at rocketry.3.
