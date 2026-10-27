# Review C13 — Physics: part model + readouts (resolve.rs, part.rs, graph.rs, readout.rs, profile.rs)

Scope: `crates/csp-phys/src/{resolve 3691, part 466, graph 355, readout 706, profile 402}.rs`,
cross-traced against the sim (`csp-sim/src/lib.rs` propellant_plans/debit, `transfers.rs`),
the flight HUD walk (`csp-view/src/flight.rs`), the VAB (`csp-ui/src/vab.rs`), the launch path
(`csp-view/src/lib.rs:launch_craft`), and part data (`data/parts/`). `cargo test -p csp-phys --lib`
passes (157/157). part.rs and graph.rs: clean — no findings (unit-quaternion gates, fixed vertex
order, stable discriminants, deterministic BTreeMap arenas all check out).

---

### [HIGH] Finding 1: VAB stage readout models per-stage engine ignition that flight cannot perform — stage TWR/burn-time rows are wrong for every multi-engine staged craft

**File**: crates/csp-phys/src/readout.rs:217-227 (engine set), readout.rs:12-24 (module-doc "Burn" model), readout.rs:65-69 ("Engines this stage ignites")

**Problem**: `analyze()` computes each stage row from `row.engine_ids` = engines among
`session.parts_in_stage(stage)`. The sim has no engine activation at all: `SimCommand` surface has
only vessel-wide `SetThrottle` (csp-sim/src/lib.rs:607), `propellant_plans` draws fuel plans for
**every** engine-spec node on the craft (csp-sim/src/lib.rs:9045-9050), and thrust is applied at
every `Thrust` site at the vessel throttle (csp-phys/src/simple.rs:1443-1454). The flight HUD's own
Δv walk agrees with the sim, not with the readout: `burn_step` sums over *all* attached engines
(csp-view/src/flight.rs:2114-2174).

**Evidence** (readout's own `two_stage_readout_separates_mass_and_propellant` fixture: booster
20 kN/280 s + sustainer 10 kN/300 s, 2000 kg propellant, 2380 kg wet):
- Readout stage-1 row: F = 20 kN, ṁ = 7.28 kg/s → burn 274.7 s, TWR 0.86.
- Flight + HUD at full throttle: F = 30 kN, ṁ = 10.68 kg/s → burn 187.3 s, TWR = 1.28.
Burn time −32%, liftoff TWR +50%, and the per-stage Δv split differs (readout credits the whole
mass ratio to booster Isp; flight burns it at thrust-weighted Isp).

**Impact**: Any player build with engines on both sides of a decoupler (sparc + ember are both
stock) plans by numbers no flight pattern can reproduce — the stage-1 row is unreachable in flight.
This is exactly §3.1's "one resolver, one place … if these ever disagree, players find it in a
day", applied to the readout. The M3 exit row "live Δv/TWR/burn-time" is wired but its stage model
is fictional relative to the shipped flight loop; the mission check (`min_launch_twr` 1.2) judges
a TWR the craft will not launch with.

**Suggestion**: Pick one model and implement it once. Minimal honest fix inside readout.rs: a
stage row's engine set = *all* engines in the attached set at that stage's firing (drop the
`parts_in_stage` engine filter), matching `burn_step`/`propellant_plans` exactly; relabel rows as
"between firings". Better fix: add per-stage engine activation to `StageActions`/the sim (KSP
semantics) and keep the readout as-is. Either way, the readout and the HUD walk must share the
engine-set rule (see Finding 3).

### [MEDIUM] Finding 2: Vessel-wide propellant pooling + ascending-id drain makes serial staging strictly counterproductive (upper stage always starved)

**File**: crates/csp-phys/src/readout.rs:282-291, 318-348 (R14-F6 note); csp-sim/src/lib.rs:9032-9037 (`avail_of` sums the whole vessel), csp-sim/src/transfers.rs:501-523 (`debit` drains ascending `PartNodeId`)

**Problem**: Fuel ignores connectivity: every attached tank pools, and draws drain lowest-id
first — in the VAB's top-down build order that is the *upper* stage's tank. For the canonical
serial stack the booster drinks the sustainer dry through the decoupler, so the sustainer's row is
Δv = 0 + an `EngineWithoutPropellant` warning (readout's own test asserts exactly this,
readout.rs:655-664), and flight does the same. Total craft Δv of a 2-stage equals the single-stage
with the same tanks *minus* the decoupler's dead mass — staging buys nothing in v1.

**Impact**: Playability of the M3 "staging" exit row: the decoupler part, stage plan, per-stage
readout and staging UI all exist, yet the optimal play is to never stage. Readout and sim agree
(the numbers don't lie), so this is a design/data-integrity gap, not a display bug — but it is the
root cause Finding 1 papers over with a fictional engine model.

**Suggestion**: Cut fuel crossfeed at decoupler edges (region = the validate.rs:2138-2199 supply
region, which already computes component connectivity), or add engine activation per stage. The
supply-region flood in `CraftSession::missing_propellants` is the natural shared implementation
for readout + sim draw regions.

### [MEDIUM] Finding 3: Two Δv/burn engines in two crates (DRY) — already diverged once, guaranteed to diverge again

**File**: crates/csp-phys/src/readout.rs:143-384 vs csp-view/src/flight.rs:2089-2378 (`burn_step`, `consume_drawn`, `build_walk`, `delta_v_walk_from`)

**Problem**: `csp-phys::readout::analyze` (session/BuiltPart-driven, per-stage engine sets,
`libm::log`) and the HUD walk (live `tank_contents`-driven, all-attached engines, coverage-pool
model mirroring `propellant_plans`, `f64::ln`) implement the same physics with different engine
sets and different depletion loops. Finding 1 is the divergence they already exhibit; any future
fuel-flow change (crossfeed, activation, massless-propellant policy) must land twice or the VAB
and the HUD disagree again. The HUD version is the one that matches the sim.

**Impact**: Permanent parity hazard on the exact numbers the game is planned by; the §3.1
"one resolver, one place" principle is violated for the readout layer.

**Suggestion**: Move the contents-driven walk (`burn_step` + depletion loop + drop walk) into
`csp-phys` beside `readout.rs`, parameterized over (nodes with contents/mass/engine specs,
drop sets). `analyze` becomes a thin adapter that seeds it from a freshly resolved session; the
HUD seeds it from `VesselPropulsion`. One engine-set rule falls out for free.

### [MEDIUM] Finding 4: Parachute inflation ramp is bypassed whenever the deploy command is issued outside the catch envelope — the standard staged-chute path opens the canopy instantly

**File**: crates/csp-phys/src/simple.rs:2651-2664 (`decel_area_m2`); spec semantics authored in crates/csp-phys/src/part.rs:374-385 and resolved at crates/csp-phys/src/resolve.rs:1983-2011

**Problem**: `ramp = ((at_s − t0)/deploy_time_s).clamp(0,1)` measures from the deploy *command*
time `t0`. The canopy gate (`alt ≤ 6000 m && speed ≤ 260 m/s`) is evaluated per tick; if the
player stages or clicks deploy while high/fast — the normal recovery flow, since
`fire_stage` auto-issues `DeployPart` at stage fire (csp-view/src/flight.rs:611-615) — then at the
first gate-passing tick `at_s − t0` already exceeds `deploy_time_s` and the area jumps packed
(0.4 m²) → deployed (120 m²) in one tick. The authored 1.5 s ramp (csp.chute.s125.ron:56) never
runs. At gate-pass near the 260 m/s ceiling the instant 120 m² Cd·A produces ~MN-scale load
(0.5·ρ·120·260²), vs the chute's authored `max_axial_n` 150 kN — the external-load attribution
(simple.rs:1428-1437) hands exactly that force to the chute node's structural check.

**Impact**: The Phase-1 recovery loop (re-entry → staged chute → recover) that this cycle
celebrates loses the one mechanism that keeps the deploy survivable, precisely in its most common
invocation. Worst case: chute shreds/detaches at first catch, pod impacts at ~260 m/s.

**Suggestion**: Start the ramp at gate-pass, not at command time: record (or solve for) the first
tick the gates pass — e.g. store `caught_at_s` lazily in `DecelSite` semantics (recompute from
`(craft, deployed, tick)` is awkward since the gate needs live alt/speed; a cheap fix is clamping
`ramp = min(1, (at_s − t0)/T)` **but** tracking gate-pass by storing the deploy record's gate
entry when `refresh_decel_sites` first sees gates satisfied). At minimum, document the current
"ramp from command, full-open on gate pass" semantics in `ParachuteSpec::deploy_time_s` so data
authors know the ramp only protects same-envelope deploys.

### [MEDIUM] Finding 5: `BuiltPart.data_set_hash` is never stamped on the player path and has no consumer anywhere — §3.1/§7.7 provenance is dead plumbing

**File**: crates/csp-phys/src/resolve.rs:261-267 (field + "the manufacture path fills every field"), resolve.rs:115-119 (doc: "no consumer at M3 … tracked future item"); crates/csp-ui/src/vab.rs:1898-1927 (`stock_build` uses `BuiltPart::stock` → `[0;32]`); csp-sim/src/tech.rs:100-130 (`build_part` takes the hash but has no production caller)

**Problem**: The VAB's manufacture path (`stock_build`) freezes id/scale/variant/upgrades but
stamps a zeroed `data_set_hash`; `TechState::build_part` — the helper that *does* accept a hash —
is called only by tests. No code in the tree compares `data_set_hash` against
`csp_data::data_set_hash()` (§7.7 "loading with changed data warns" is unimplemented; only a
save-level hash is round-tripped in csp-sim/src/save.rs:416). The resolver doc's claim that the
manufacture path fills every field is false for the path players actually use.

**Impact**: If/when the §7.7 drift warning lands, every existing player craft records zeros and
the warning can never fire per-part; until then the field is serialized dead weight that makes the
record look complete when it isn't.

**Suggestion**: Stamp `data_set_hash` in `VabSession::stock_build` (the catalog install site has
the hash), or delete the field until the consumer exists. Collapse the two manufacture helpers
(`VabSession::stock_build` vs `TechState::build_part`) into one — they already disagree on the
hash and on stretch handling.

### [NIT] Finding 6: The §3.1 wear composition (`apply_wear`/`wear_multiplier`/`hazard_per_s`) is a dead display projection

**File**: crates/csp-phys/src/resolve.rs:186-200 (superseded note), 1525-1560

The M9b model in `csp-sim/src/reliability.rs:511` computes λ from `ReliabilitySpec` directly
(`base·stress·qual·infant`); nothing outside resolve.rs reads `wear_multiplier`/`hazard_per_s`
(grepped: zero consumers). The doc honestly says removal is a follow-up. Fine to keep for the
readout switch-over, but `EffectivePart = resolve ∘ wear` now exists only in the module doc —
flagging so the cleanup actually happens when the readouts move to the spec table.

### [NIT] Finding 7: MissionProfile is only half "editable in the editor"; confidence always evaluates FRESH qualification

**File**: crates/csp-phys/src/profile.rs:63-84; crates/csp-ui/src/vab.rs:3156-3181 (target Δv / min TWR editable, `burn_segments` shown as a count only), vab.rs:3133-3148 (confidence with `FamilyQualifications::default()`)

§3.4 says the profile is "editable in the editor"; segments — the input the confidence number
integrates over — are fixed at the `terrin_ascent` defaults (120 s atmo + 180 s vac). A lander or
long-ascent mission gets a confidence number computed over the wrong plan. Same for quals: the
save's family table is not plumbed into the editor, so a flight-proven family still displays
"(unqualified family)". Both are labeled in-UI, so honest, but the number is weaker than the PRD
promises. Also note `ExcessRestarts` is approximated by *segment index* (profile.rs:279), which
trips ember's 3× rule on the second segment even for a single ignition — documented, small
(~1% confidence effect at authored MTBFs).

### Follow-ups (other clusters)
- §7.7 load-time data-set drift warning: absent from the save/load path (C11) — see Finding 5.
- Chute safe-deploy envelope (6 km / 260 m/s) is displayed nowhere — the part blurb shows only
  deployed Cd·A (csp-ui/vab.rs:2601-2605) and the deploy button gives no "will catch below…" hint (C22).
- HUD walk uses `f64::ln` where the readout pins `libm::log` — presentation-only, not hashed; fine
  today, but fold into one implementation per Finding 3.

---

## PRD coverage table

| PRD item (§) | Status | Evidence |
|---|---|---|
| §2.4/§3.1 `EffectivePart = resolve(BuiltPart) ∘ wear(PartUsage)` | **WIRED** (resolver frozen half; wear half dead projection — F6) | resolve.rs:2282-2456; reliability.rs:511 owns live λ |
| §3.1 frozen-at-manufacture upgrades, reload-safe | **WIRED** | `compose_upgrade_levels` resolve.rs:877-934; frozen `TrackLevels` self-contained |
| §3.1 `data_set_hash` provenance + §7.7 drift warning | **UNWIRED** (F5) | never stamped (vab.rs:1913), never compared |
| §3.1 one resolver shared by editor/sim/launch | **WIRED** | launch_craft re-resolves via VAB analysis (csp-view/lib.rs:2776-2811); FRESH quals on both sides |
| M3 "live Δv/TWR/burn-time" readouts | **WIRED — DEGRADED** stage model vs flight (F1) | readout.rs + vab.rs:3220-3299 rows, craft_section totals |
| M3 `MissionProfile` stub | **WIRED — PARTIAL** (segments not editable, F7) | profile.rs:103-153; vab.rs:3156+ |
| M9b confidence `exp(−Λ_expected)` UI | **WIRED** (FRESH quals in editor — F7) | profile.rs:204-307; vab.rs:3133-3148 |
| §3.4 stress multipliers displayed | **WIRED** | `stress_breakdown`/`RegimeMetrics` probe — resolve.rs:1703-1763, vab.rs:3509+ |
| §3.4 qualification live per-family | **WIRED** | `qual_factor` live curve re-eval; bench/flight accrual in reliability.rs/tech.rs:139-158 |
| Parachute spec resolution → `DeployPart` flow | **WIRED** (ramp bug F4; envelope not surfaced) | resolve.rs:1983-2011 → sim lib.rs:8911-8944 → simple.rs:2651 |
| Determinism (BTree, libm, bit-identical, permutation-safe) | **WIRED** | resolve.rs:150-156 + tests; readout.rs:38-39 |
| VAB Δv/TWR == flight delivery (2-stage trace) | **DEGRADED** (F1/F2) | trace: readout.analyze vs propellant_plans/burn_step |

## Top 3 issues
1. **F1 [HIGH]** — VAB per-stage readout assumes engine ignition that flight cannot do; stage TWR/burn-time off by ~30-50% for every multi-engine staged craft (readout.rs:217 vs sim lib.rs:9045).
2. **F2 [MEDIUM]** — vessel-wide fuel pooling + ascending-id drain starves the upper stage through the decoupler: serial staging is strictly worse than not staging (readout's own test asserts the 0-Δv upper stage).
3. **F4 [MEDIUM]** — chute inflation ramp measured from deploy-command time, so the standard staged-chute recovery opens 120 m² instantly at gate-pass (~MN snap vs 150 kN rated attachment).
