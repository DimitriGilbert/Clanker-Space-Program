# Verified Report C09 — Colony flow network (`crates/csp-sim/src/colonies.rs`)

Verification agent pass over `review-report-C09.md` (session-full-20261026). Every finding was
re-derived from source at the cited lines; the two HIGHs and the two empirical MEDIUMs were
re-run as live probes against the real crate (external probe crate at `/tmp/c09probe`, public
`World` API only, real `data/systems/home` loaded through `csp_data::load_system` — no repo file
was modified). `cargo test -p csp-sim colonies::` re-run by this agent: **43 passed, 0 failed**
(matches the report's claim).

Verdict summary: **10 of 10 findings CONFIRMED** (Finding 8 confirmed with one sub-claim
corrected; Finding 1 confirmed with reachability stated more precisely than the original).
Nothing dismissed.

---

### Finding 1: Stock-anchored net-positive converter cycle mints resources forever — CONFIRMED

**Original**: grounding tests only input availability, never net conservation; a stock-anchored
multi-converter cycle with positive net rate grounds and runs at fraction 1.0 forever; probe
showed 10 → 130 units of `a` in 600 s.

**Verification**: Re-derived the allocation math from
`crates/csp-sim/src/colonies.rs:775-803` (`grounded_modules`) and `:825-953` (`allocation_pass`),
then reproduced it live through the real `FoundColony` → `apply_found_colony` → `recompute_colony`
path (public API only), with recipes `a 1.0 → b 0.5` and `b 0.5 → a 1.2`, modules
`[Store a 1000, Conv ab, Conv ba]`, founding store `a = 10`:

```
[F1] fractions at founding: [1.0, 1.0, 1.0]
[F1] net a=+0.2000/s
[F1] after 600 s: a=130.0031 state=Nominal
[F1] after +1080 s more: a=346.0031 anchor_tick=0 anchor_level=10.0 net=+0.2 cap=1000
```

The anchor never moved and no event fired — the mint is unbounded, halted only by store capacity
(`level_at` clamps at `cap`). Mechanism confirmed in code: `grounded_modules` (colonies.rs:789-792)
accepts "stocked (`level > eps`) OR produced by another grounded module"; `b` is produced by the
grounded `ab` converter, so `ba` grounds; `grant_map` (colonies.rs:968-971) treats any nonzero
stock as infinite availability, so both grants fill and the greatest-fixpoint sweep settles at
fraction 1.0. Net `a` = +1.2 − 1.0 = +0.2/s from nothing. The existing tests
`self_feeding_converter_pins_at_zero` (colonies.rs:3408: single self-feeder; :3460: source-less
cycle) and `grounded_cycle_still_runs` (:3503: generator-anchored cycle — a sanctioned source) do
not cover the stock-anchored net-positive case; the report's coverage claim is accurate.

**Reachability, stated precisely** (sharper than the original): NOT reachable anywhere in shipped
content — the UI founds only the fixed starter def (`csp-view/src/lib.rs:1301`, Solar+Battery);
the MCP `sim.command` tool is indeed a raw `SimCommand` passthrough (`csp-server/src/mcp.rs:532-556`)
that accepts a custom `FoundColony` def, but `ColonyModuleDef::Converter` carries only a
`RecipeId` resolved against the loaded table (`colonies.rs:1477`), and the three authored recipes
(`data/recipes/`: water→oxygen, ore→LF+ox, ore→metal) are acyclic — no recipe output is any
recipe's input, so no converter can feed another converter at all. The hole is therefore a latent
sim-correctness defect tripped by any future cyclic recipe pair, modded data, or test content —
not by today's player or MCP agent. Severity HIGH as a conservation defect in the load-bearing
allocation pass is still justified (the grounding design's own doc claims to kill exactly this
class), and the suggested fix (reject/pin SCCs with positive aggregate net rate and no
Drill/Solar/Generator member) is sound.

---

### Finding 2: Authored Rask outpost sits exactly ON the terminator; permanent "Day" hangs on a +6.1e-17 residue — CONFIRMED

**Original**: longitude π/2 + all-zero orbit elements ⇒ h0 = −π/2 exactly; `cos(−π/2)` = +6.1e-17
decides Day; zero margin; wrong authored comment; full daylit-average insolation at a truly
grazing point.

**Verification**: all three data facts confirmed: `data/systems/home/sites.ron:70`
(`longitude_rad: 1.5707963267948966` = the f64 nearest π/2), `data/systems/home/rask/body.ron`
(mean anomaly/arg periapsis/ascending node all `0.0` ⇒ L0 = 0), and the wrong comment at
`data/systems/home/colonies.ron:5-7` ("a permanently daylit site"). Live probe against the real
loaded system through `body_light`/`daylight_at` (colonies.rs:409-464):

```
[F2] cos(-pi/2) = 6.123233995736766e-17
[F2] h0 = -1.57079632679489656e0  (== -pi/2 exactly: true)   synodic_rate = 0e0   cos_lat = 1
[F2] daylight t=0: Day    t=+10y: Day    next_terminator_tick: None
[F2] day_insolation = 866.440 W/m2 -> solar 40m2 x 0.2 = 6931.5 EC/s
[F2] authored rask colony: Nominal, Day, fractions all 1.0, net EC +6928.02/s
[F2] after 100 sim-days: Nominal, Day, EC=5000 (capped), ore=100, metal=60
```

**Is it a live time bomb or purely theoretical?** Deterministic Day as shipped — it cannot flip
at runtime: `synodic_rate = omega − n` is `x − x = exactly 0.0` (both derive from the same
`period_s` when `rotation_period_s: None`, colonies.rs:431-436), `h0` is recomputed from constant
construction config at every solve, the model has no orbital precession (fixed Kepler elements),
and `cos(−π/2_f64)` is not even a libm quirk — the stored longitude sits below true π/2 by
6.12e-17, so the exact cosine is positive. BUT the margin is sub-representable: one longitude ulp
at π/2 is 2.22e-16, i.e. 3.6× the deciding margin; any edit that moves the longitude one step up,
any nonzero L0, or any change to the `λ − L0 − π` convention flips the outpost to permanent Night
(battery 3000 EC / 3.5 EC/s = 857 s to `PowerLost`, which with zero solar is absorbing —
"produces nothing forever" is accurate). The report's engineering substance is fully confirmed;
the recommendation (move to ~2.4 rad, loader-time terminator-distance assertion for zero-synodic
bodies, fix the comment) stands. The "infinite-energy well" phrasing is hyperbole — 866 W/m² is
the rule-1 average any Day site receives — but the underlying point (a terminator-grazing point
harvests the full day-average under the two-state step) is correct.

---

### Finding 3: Tidally-locked satellites modeled as locked to the sun (synodic = 0 via the moon's own n) — CONFIRMED

**Original**: `rotation_period_s: None` ⇒ ω = the body's own mean motion ⇒ synodic 0; physically a
moon locked to its planet still sees a solar day ≈ synodic month; Rask's EC balance authored
against the wrong model.

**Verification**: formula confirmed at colonies.rs:431-436 (`omega = n` from the body's OWN orbit
elements) and the doc at :348-350 states the "fixed sun: no terminators ever" reading outright.
Probe against real data: synodic_rate = 0.0 exactly, no terminator ever, colony Day for 100
sim-days — the day/night machinery is dead code on Rask, as claimed. The correct two-body
geometry (moon spin ω = 2π/138984 s locked to Terrin; sun drifts at Terrin's n = 2π/9203835 s —
exactly the report's suggested `synodic = omega − n_parent_around_star`):

```
[F3] correct synodic = 4.4525e-5 rad/s
[F3] solar day = 141115 s (6.53 sim-days), night = 70558 s (3.27 sim-days)
[F3] EC per night at 3.5 EC/s = 246952 EC (battery 5000; founding 3000 drains in 857 s)
```

The report's "~139k s solar day / ~69.5k s night / ~243k EC" figures are ~1.5% low (they use the
moon's sidereal orbital period; the strictly correct solar day is the synodic month, 141.1k s) —
immaterial to the conclusion: the authored battery (5000) covers ~2% of one night, and under the
correct model the outpost cycles `Degraded{PowerLost}` for ~98% of every 70.6k s night
(drill+smelter halt; the day surplus of 6928 EC/s restores it within ~1 s of dawn). The
`data/systems/home/rask/body.ron` header itself pins "tidally locked = the orbital period (§3.0)"
— i.e. locked to Terrin — so the code's sun-locked reading contradicts the authored data's own
definition. Confirmed as a real model deviation with balance impact, not just cosmetic.

---

### Finding 4: Crew at a colony with no LifeSupport module are immortal and consume nothing — CONFIRMED

**Original**: `ls_depleted` scans only `LifeSupport` modules; no LS module ⇒ no rows ⇒ never
depleted; no consumption; `AssignCrewToColony` requires no LS module.

**Verification**: colonies.rs:1132-1145 — the depletion loop body only runs for
`ColonyModuleDef::LifeSupport` defs; with zero such modules `ls_depleted` stays `false` regardless
of crew. Consumption rows come only from `module_requests` (colonies.rs:613-625), which registers
`LifeSupport` rows only when such a module exists with crew > 0 — crew at an LS-less colony draw
nothing. `apply_assign_crew` (colonies.rs:1773-1810) checks colony liveness and member status
only — no LS requirement (nor does `TransferColonyCrew`'s `land_crew`, transfers.rs:1193-1204).
Live probe (life support enabled, colony `[Battery 100]` + EC 50, one crew posted):

```
[F4] after 10 sim-days: state=Nominal crew={CrewId(0)} member_status=Active location=Colony(ColonyId(0))
[F4] EC level=50.000 (founding 50, nothing consumed it)
```

Shipped-content claim also verified: the authored Rask colony (Solar/Battery/Drill/Converter/2×
Store) and the UI starter def (Solar/Battery) both lack `LifeSupport`, and the UI offers `→colN`
posting buttons for every live colony (`csp-ui/src/program.rs:1131-1143`). Colony life support is
consequence-free in the shipped game. Confirmed as reported.

---

### Finding 5: colony_resupply — 8 of 9 authored body×resource draws uncompletable; duds permanently occupy a slot — CONFIRMED

**Original**: completion counts landings into stores of colonies on the drawn body; the only
player-foundable colony carries an EC store only; Duva has no sites; rask has ore/metal stores;
only rask×ore completable; deadline None and no cancel verb ⇒ permanent slot burn.

**Verification**: each link checked in source/data.
- Template (`data/contracts/contract.colony_resupply.ron`): bodies `[terrin, duva, rask]` ×
  resources `[ore, food, oxygen]` = 9 draws; `deadline_days: None`; advance 10k / completion 45k.
- Generator (contracts.rs:377-391) draws body and resource with no gating on colony existence.
- Predicate (contracts.rs:997-1015) sums `facts.colony_deliveries` over colonies on the body;
  the fact is written only by colony-transfer completions (transfers.rs:1068-1086).
- Delivery requires colony store capacity for that resource (transfers.rs:1434-1447): no store
  entry ⇒ capacity 0 ⇒ `free < amount` ⇒ "insufficient colony store capacity" — the opaque
  rejection, confirmed verbatim.
- Colonies possible in a shipped game: the authored Rask outpost (ore 100 + metal 60 + EC stores)
  founded at world build (`csp-view/src/lib.rs:1891` → `found_authored_colonies`), plus player
  starter colonies (`starter_colony_def`, csp-view lib.rs:1301 — Solar/Battery/EC only) at any
  site without a colony. `data/systems/home/sites.ron` contains exactly three sites, none on
  Duva. There is no module/store-addition command in the `SimCommand` enum.
- Therefore: terrin×{ore,food,oxygen} and duva×{ore,food,oxygen} — 6 draws — can never complete;
  rask×{food,oxygen} — 2 more — likewise (no food/oxygen store on any possible Rask colony);
  rask×ore is the only completable draw. 8/9 uncompletable, exactly as reported.
- Slot burn: `MAX_ACTIVE_CONTRACTS = 32` (contracts.rs:104), `deadline_days: None` schedules no
  `ContractDeadline`, and no decline/cancel/abandon-contract command exists in the enum — an
  accepted dud is permanent. (One mitigating nuance: no failure penalty either — a dud wastes the
  slot and the player's time, not funds.)

---

### Finding 6: Impact damage is terminal, unnotified, and shares the 100 m transfer radius — CONFIRMED

**Original**: debris inside `transfer_cfg.radius_m` damages the lowest-index undamaged module;
`Degraded{ImpactDamage}` recorded only from `Nominal`; no repair path; no notification kind.

**Verification**: colonies.rs:1928 (`let range = self.transfer_cfg.radius_m;`) — the impact radius
IS the transfer radius; `data/balance.ron:40` `transfer_radius_m: 100.0`. :1963
(`position(|m| !m.damaged)` — lowest-index undamaged). :1889-1893 — `ImpactDamage` recorded only
from `Nominal`. `PowerRestored` (colonies.rs:2157-2181) lifts only `Degraded{PowerLost}`; grep for
any repair command across `SimCommand`: none. `NotificationKind` (reliability.rs:387-…) variants
are PartFailed/IgnitionMisfire/OnRails/ContractAccepted/ContractCompleted/ContractFailed/
ScienceBanked/StreamKey/PartReliability — no colony-damage kind. The seam is live in play:
`apply_surface_impacts` is called from `step_bubble_range` at lib.rs:4857 on every surface-touching
debris body, so a discarded stage landing within 100 m of a colony bricks its production with no
player-visible signal beyond the panel's state string. PLAN §3.5.1 rule 5 names impact damage as
"a `Degraded` reason" (PLAN.md:1288) and the §6.4 cut list (PLAN.md:1543-1564) says nothing that
sanctions terminal-ness without repair. The in-code doc (colonies.rs:87-93) admits "terminal in
M8-A ... a repair command lands post-M8-A with the impact-detection hook" — but the hook HAS
landed (M8-C) and no repair followed, which is exactly the gap the finding names. Confirmed.

---

### Finding 7: Stale doc comments about the impact seam — CONFIRMED

**Original**: colonies.rs:61-65 and lib.rs:995/997-998 still say the bubble-impact hook is
outstanding/M8-B's, but the seam landed and is wired.

**Verification**: colonies.rs:63-65 ("the bubble-impact detection hook remains outstanding (it is
not part of M8-B's resource-flow scope)") and lib.rs:993-998 (doc: "the bubble-impact detection
hook is M8-B's"; marker: "ui-unreachable: the crash-damage stand-in for tests") both predate the
M8-C seam, which is wired at lib.rs:4854-4857 (`apply_surface_impacts` inside `step_bubble_range`)
and reachable in ordinary play via debris. Both blocks are stale as claimed; the `ui-unreachable`
marker on `DamageColonyModule` is doubly wrong now (the command may still be test-only, but the
damage state it creates is player-reachable through the real path). Trivial fix, correctly LOW.

---

### Finding 8: Terminal colony transitions leave `vessel.joined_colony` dangling — CONFIRMED (one sub-claim corrected)

**Original**: abandon/LS-depleted don't clear joined vessels; the gate at transfers.rs:1367-1389
then rejects every transfer; "`LeaveColonyInventory` exists but nothing surfaces it."

**Verification**: `apply_abandon_colony` (colonies.rs:1707-1764) and the `LifeSupportDepleted`
arm (colonies.rs:2183-2240) touch colony records and the crew roster only — neither touches
`vessels[*].joined_colony`. `joined_colony_gate` (transfers.rs:1380-1387) rejects all
vessel↔colony transfers once the colony is Dormant/Abandoned. The dangling state is real.

**Correction**: the "nothing surfaces it" sub-claim is wrong. The program panel renders a row for
every colony including Dormant/Abandoned (`csp-view/src/lib.rs:4768-4788`), computes `joined`
from the focused vessel's `joined_colony` regardless of livery (lib.rs:4826-4829), and shows a
"leave" button whenever `c.joined` (`csp-ui/src/program.rs:1314-1322`) — which issues
`LeaveColonyInventory`, and `apply_leave_colony` (transfers.rs:1346-1359) has no colony-live
gate, so the player CAN clear the stale opt-in. The actionable residue of the finding is the
state hygiene issue (auto-clear on terminal transitions, or mark the row stale), which stands as
LOW. Confirmed with that correction.

---

### Finding 9: `apply_found_colony` does not reject a site already hosting a colony — CONFIRMED

**Original**: no occupancy check at the apply; UI gates on `has_colony` (which includes
Dormant/Abandoned, so abandoned sites can never be re-founded either); MCP path does not gate.

**Verification**: read the whole apply (colonies.rs:1588-1701) — it validates site existence,
env, finiteness, recipes; nothing references other colonies. Live probe, two identical
`FoundColony` defs at the same site through the real command path:

```
[F9] colonies at the one site: [(ColonyId(0), "pad", Nominal), (ColonyId(1), "pad", Nominal)] (both accepted: true)
```

UI side confirmed: `has_colony: sim.colonies().values().any(|c| c.site == s.id)`
(csp-view lib.rs:4843) matches every state including Dormant/Abandoned, and the found button
gates on `!s.has_colony` (program.rs:1388) — so the shipped UI neither double-founds nor
re-founds an abandoned site, while the raw command path (MCP) double-founds freely. Confirmed.

---

### Finding 10 (DRY): Solve-input assembly, crew-unhook, store-freeze duplicated — CONFIRMED

**Original**: `colony_net_solve` vs `recompute_colony` hand-build the identical `SolveInput`;
crew-unhook loop ×3 in colonies.rs (+ more in `land_crew`); freeze loop ×2.

**Verification**: colonies.rs:2011-2051 (`colony_net_solve`) and :2268-2331 (`recompute_colony`)
construct env→`body_light`→levels→caps→concentrations→crew-gating→`solve_network` in two
hand-written copies — line-for-line parallel (the firing-time re-check's correctness depends on
these staying identical, which is the report's point). Crew-unhook loops ("if location ==
Colony(id) → Unassigned") at :1738-1744 (abandon), :1829-1835 (evacuate), :2212-2219
(LS-depleted); transfers.rs `land_crew` carries the same roster-location bookkeeping in its
vessel→vessel and vessel→colony arms (transfers.rs:1174-1188, 1193+). Store-freeze loops
(re-anchor at now, zero net rates) at :1756-1761 and :2232-2237, identical shape. Confirmed as a
maintainability LOW with the suggested extractions being the right shape.

---

## PRD coverage table re-verification

| Row | Verdict |
|---|---|
| Rule 1 two-state power + closed-form `TerminatorCrossing` (Silent) | Verified WIRED with the two named deviations: satellite synodic (Finding 3, confirmed empirically — no crossings on Rask) and the authored per-latitude average replaced by the tilt-0 closed form `(2/π)S·cosφ`, admitted in the module doc (colonies.rs:12-15). |
| Rule 2 algebraic `T_eq` | Verified: colonies.rs:1068-1095 solves `T_eq = (Q/(εσA))^¼` once per recompute; `thermal_steady_state_is_algebraic` exists (colonies.rs:2871). |
| Rule 3 allocation pass | Verified: colonies.rs:825-1014 implements resource-ascending, `(priority, ModuleIndex)` full-then-pro-rata with the (sound) grounding/sink extensions; net-positive-cycle hole per Finding 1. |
| Rule 4 next-event min, `amount = rate·dt` | Verified: `next_boundary`/`earliest_store_bound` (colonies.rs:1159-1224), epoch-anchored stores; named tests exist (`store_bound_is_query_tick_independent` :3327, `colony_advance_is_partition_independent` :3360). |
| Rule 5 states/transitions | Verified: arms at colonies.rs:2058-2412; `abandon_recovery: 0.25` (balance.ron:60); deaths hard-coded `permanent` (comment :2205-2207); the three named caveats (ImpactDamage terminal — F6; LS-less immortality — F4; hard-coded deaths) all confirmed. |
| §2.6.5 classes | Verified in events.rs:464-486: TerminatorCrossing/ColonyStoreFull/Empty/ColonyInspection Silent; PowerLost/PowerRestored/LifeSupportDepleted Clamping (PowerLost/Restored unnamed by the PRD — the report's "sensible" note is fair). |
| Pathological density | `twenty_colonies_stay_inside_the_event_budget` exists (colonies.rs:3159) and passes. |
| §3.5 founding cost MISSING | Verified: PLAN.md §3.5 funds diagram names "facilities (labs, test stands, pads, colony modules)" as a funds sink; sim charges nothing; UI hover says "prototype, no cost" (program.rs:1392; csp-view lib.rs:1299). |
| Player surface | Verified: found button (program.rs:1388-1397), join/leave/deliver/withdraw (program.rs:1303-1340), crew posting (1131-1143); resupply reachability limited to rask×ore per Finding 5. |

## Observations

The five no-action observations were spot-checked where cheap: `publish_colonies` is indeed
O(live colonies × heap) per interval (colonies.rs:2423-2457) and `fraction · request` charging is
as documented (:1049-1051). No corrections needed.
