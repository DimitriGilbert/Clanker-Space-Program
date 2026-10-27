# Review Report C09 — Colony flow network (`crates/csp-sim/src/colonies.rs`)

Session session-full-20261026, cluster C09. Scope: §3.5.1 flow network, §2.6.5 colony event
mandate; cross-checked lib.rs (event classes/firing), transfers.rs (colony endpoints),
contracts.rs (resupply predicate), economy.rs (sites/abandon), program.rs + csp-view (player
surface), data (colonies.ron, sites.ron, rask/body.ron, recipes, balance.ron). Full file read
(code L1–2460, tests L2461–5172). Suite verified green (`cargo test -p csp-sim colonies::` —
43 passed). Two suspected bugs verified empirically with a temporary integration probe
(since deleted): a resource-minting converter cycle and immortal LS-less colony crew.

---

### [SEVERITY: HIGH] Finding 1: Stock-anchored net-positive converter cycle mints resources forever (conservation hole in the allocation pass)

**File**: crates/csp-sim/src/colonies.rs:775-803 (`grounded_modules`), :825-953 (`allocation_pass`)
**Problem**: The grounding least-fixpoint (added to kill self-feeding converters) tests only *input
availability* — "every requested resource is stocked or produced by ANOTHER grounded module" — never
*net conservation*. A cycle of two or more converters whose net rate is positive, anchored by any
initial stock, grounds all members and runs at fraction 1.0 forever.
**Evidence**: Verified empirically (temporary probe, since deleted): modules `[Store a 1000,
Converter a 1.0 → b 0.5, Converter b 0.5 → a 1.2]`, founding store `a = 10`. Result:
`fractions [1.0, 1.0, 1.0]`, `net_a = +0.2/s`, and after 600 s `a = 130.0` (10 → 130 from nothing).
`b` nets exactly 0. The existing test `self_feeding_converter_pins_at_zero` only covers the
single-module and net-zero cases.
**Impact**: The brief's literal "converter loops creating mass/energy from nothing". Not reachable
through the shipped UI (players can only found the fixed starter def; authored recipes have no
cycles) — but `sim.command FoundColony` is exposed verbatim through the MCP server (C27), and any
future custom-colony authoring trips it. A crewed economy built on this math can be inflated
without bound.
**Suggestion**: At `apply_found_colony`, build the module-level production/consumption graph over
resources and reject (or pin) any strongly-connected component whose aggregate net rate is positive
and which has no `Drill`/`Solar`/`Generator` member — grounding alone cannot see net rates.
Alternatively require every grounded cycle member's inputs to trace to a non-cyclic source.

### [SEVERITY: HIGH] Finding 2: The authored Rask outpost sits exactly ON the terminator; its permanent "Day" hangs on a +6e-17 float residue

**File**: crates/csp-sim/src/colonies.rs:458-464 (`daylight_at`); data/systems/home/sites.ron:66-77 (`rask-mines` longitude_rad = 1.5707963267948966); data/systems/home/colonies.ron:6-7 ("a permanently daylit site")
**Problem**: Rask's orbit elements are all zero (mean anomaly/arg periapsis/ascending node = 0), so
`h0 = λ − L0 − π = −π/2` exactly. `daylight_at` tests `cos(H)·cos φ > 0`; `cos(−π/2)` evaluates to
+6.123e-17 (verified), so the site reads `Day` — permanently, because the synodic rate is exactly 0
(finding 3). But λ = π/2 is precisely the terminator longitude: the subsolar point is λ = π, and the
day/night boundary is λ = ±π/2. The authored comment "a permanently daylit site" is wrong — it is a
permanently *terminator-grazing* site whose day/night status is decided by one ulp.
**Impact**: The flagship authored colony's entire EC budget (solar 6931 EC/s vs 3.5 EC/s demand)
rests on a sign with zero engineering margin. Any change to `h0`'s arithmetic, the longitude
convention, or libm's last ulp flips the outpost to permanent Night: the battery drains in
857 s (3000 EC / 3.5 EC/s), the colony lands terminal `Degraded{PowerLost}`, produces nothing
forever, silently, with no repair path. It also harvests the full daylit-average insolation
(866 W/m²) at a point where true grazing insolation is ≈ 0 — an infinite-energy well masquerading
as a marginal site.
**Suggestion**: Move the site into the open day hemisphere (e.g. `longitude_rad: 2.4` — comfortably
`cos h0 > 0`), and add a loader-time assertion that an authored colony on a zero-synodic body is at
least `~0.1 rad` off the terminator. Fix the colonies.ron comment.

### [SEVERITY: MEDIUM] Finding 3: Tidally-locked satellites are modeled as tidally locked to the *sun*, not to their parent — Rask gets no day/night cycle at all

**File**: crates/csp-sim/src/colonies.rs:347-351, :431-433 (`ColonyBodyEnv::rotation_period_s` → `omega = n` where `n` is the body's own orbit)
**Problem**: For `rotation_period_s: None` the code sets ω = the body's own mean motion around its
*parent* (Rask around Terrin), making `synodic_rate = ω − n = 0` — a fixed sun in the sky, "no
terminators ever". Physically (and per PLAN §3.0, where "tidally locked" means the rotation period
equals the 1.390e5 s orbital period around Terrin), a tidally-locked *moon* still sees the sun
rotate with the synodic month: its solar day ≈ its orbital period (~139k s), terminator crossings
every ~69.5k s (~3.2 sim-days). The drift that should cancel is the *parent's* mean motion around
the star (Terrin's n ≈ 2π/9.2e6 s), not the moon's own n.
**Impact**: §3.5.1 rule 1 ("TerminatorCrossing ... computable in closed form from body rotation and
orbital phase") is not honored for satellite bodies; batteries and the whole day/night machinery
are dead code on Rask, and the outpost's authored balance ("the battery rides the transient, not a
night" — colonies.ron:6-7) was computed against the wrong model. Under the correct model the
outpost needs ~243k EC per 3.2-day night (battery is 5000) — the authored colony would cycle
Degraded every night. Deterministic and honestly documented in-code, but a PRD deviation with no
§6.4 sanction.
**Suggestion**: For a satellite (body with a parent that is not the star), resolve the sun's
apparent drift from the parent's elements: `synodic = omega − n_parent_around_star`. Re-check the
Rask battery/EC budget under the real night length.

### [SEVERITY: MEDIUM] Finding 4: Crew posted to a colony with no LifeSupport module are immortal and consume nothing

**File**: crates/csp-sim/src/colonies.rs:1132-1145 (`ls_depleted` scans only `LifeSupport` modules), :1773-1810 (`apply_assign_crew` — no LS-module requirement)
**Problem**: The depletion predicate iterates `LifeSupport` modules only. A colony with crew > 0
and zero `LifeSupport` modules has no rows to scan → `ls_depleted` is always false; the crew draw
no oxygen/food/EC and can never die. Neither `AssignCrewToColony` nor `TransferColonyCrew`
(transfers.rs `land_crew`) requires an LS module.
**Evidence**: Verified empirically: colony = `[Battery 100]`, no LS module, life support enabled,
one crew posted → after 10 sim-days: state `Nominal`, crew still aboard, roster `Active`.
**Impact**: No shipped colony has a LifeSupport module (the authored Rask outpost and the UI starter
def both lack one — the UI offers "→colN" posting buttons for every live colony, program.rs:1132),
so colony life support is dead code in the shipped game and colony crew-posting is
consequence-free. PRD §3.5.1 rule 5's "LifeSupportDepleted with crew aboard → crew Lost" presumes
crew aboard are being supported at all.
**Suggestion**: Either reject posting/landing crew at a colony with no functional LS module (when
life support is enabled), or treat `crew > 0 && zero undamaged LS modules` as `ls_depleted`.

### [SEVERITY: MEDIUM] Finding 5: colony_resupply contracts — 8 of 9 authored body×resource draws are uncompletable in shipped content; accepted duds permanently occupy an active slot

**File**: crates/csp-sim/src/colonies.rs (loop is correct — verified by `resupply_contract_completes_on_delivery`); data/contracts/contract.colony_resupply.ron (bodies terrin/duva/rask × resources ore/food/oxygen); crates/csp-sim/src/transfers.rs:1434-1447 (capacity gate)
**Problem**: The delivery predicate (contracts.rs:997-1015) counts landings into stores of colonies
on the named body. But: the only player-foundable colony is the fixed starter def (Solar/Battery,
EC store only — csp-view lib.rs:1301), so *any* ore/food/oxygen delivery on Terrin rejects with
"insufficient colony store capacity" (no way to add stores); Duva has no sites at all; the Rask
outpost has ore/metal stores but no food/oxygen stores. Net: only `rask × ore` is completable.
**Impact**: A player who accepts a Terrin resupply (advance 10k, completion 45k) can never complete
it: contracts have `deadline_days: None` and there is no decline/cancel command, so each accepted
dud permanently consumes one of `MAX_ACTIVE_CONTRACTS = 32` slots. The player gets an opaque
transfer rejection with no hint that the colony needs a store the game never lets them build.
**Suggestion**: Gate resupply offer generation (or acceptance) on a live colony with capacity for
the drawn resource actually existing on the drawn body; or add store-carrying colony content /
custom-module founding. Cross-cluster: content fix is C34, predicate gating C08.

### [SEVERITY: MEDIUM] Finding 6: Impact damage is terminal, unnotified, and shares the 100 m transfer radius — one dropped stage bricks a colony's production forever

**File**: crates/csp-sim/src/colonies.rs:1850-1895 (`apply_damage_module` — `Degraded{ImpactDamage}` only from `Nominal`, no repair path), :1914-1972 (`apply_surface_impacts` — radius = `transfer_cfg.radius_m`), :87-93 (doc: "`Degraded{ImpactDamage}` is terminal in M8-A")
**Problem**: Any debris body touching the surface within the colony's *transfer* radius (100 m)
damages the lowest-index undamaged module and lands `Degraded{ImpactDamage}`. In that state all
drills/converters stop (`module_inert`) and only `PowerRestored` — which lifts only `PowerLost` —
can exit. There is no repair command, and no notification kind exists for colony damage (the
notification ring covers reliability only), so the player learns it only by reading the panel's
state string.
**Impact**: Discarding a spent stage 90 m from the outpost — well inside normal landing operations
— permanently ends that colony's production with zero player feedback. Harsh, invisible, and not
sanctioned by §3.5.1 (which names impact damage as a Degraded *reason*, not a terminal state) or
the §6.4 cut list.
**Suggestion**: Add a repair path (time + EC/metal cost), a distinct (smaller) impact radius vs the
transfer radius, and a `NotificationKind` for colony module damage.

### [SEVERITY: LOW] Finding 7: Module doc and command comments are stale about the impact seam

**File**: crates/csp-sim/src/colonies.rs:61-65 ("the bubble-impact detection hook remains outstanding (it is not part of M8-B's resource-flow scope)"), crates/csp-sim/src/lib.rs:995/997-998 ("the bubble-impact detection hook is M8-B's")
**Problem**: The M8-C seam landed and is wired (`apply_surface_impacts` called from
`step_bubble_range` at lib.rs:4857), but two load-bearing doc blocks still say the hook is
outstanding. In a file whose docs are otherwise meticulous to the point of being the review map,
this actively misleads.
**Suggestion**: Update both comments to name the wired seam.

### [SEVERITY: LOW] Finding 8: Terminal colony transitions leave `vessel.joined_colony` dangling

**File**: crates/csp-sim/src/colonies.rs:1707-1764 (`apply_abandon_colony`), :2183-2240 (`LifeSupportDepleted` arm) — neither clears joined vessels; gate at transfers.rs:1367-1389 then rejects everything
**Problem**: A vessel joined to a colony that goes Dormant/Abandoned keeps `joined_colony = Some(id)`.
The panel still shows it "joined", but every transfer rejects (`colony_live` gate) with no prompt to
leave. `LeaveColonyInventory` exists but nothing surfaces it.
**Suggestion**: Clear `joined_colony` on terminal transitions (the transfer record bookkeeping
already tolerates vanished endpoints), or at minimum show the stale state in the panel.

### [SEVERITY: LOW] Finding 9: `apply_found_colony` does not reject a site already hosting a colony

**File**: crates/csp-sim/src/colonies.rs:1588-1701
**Problem**: No occupancy check — two colonies can share one site (identical pose, full stores
each). The UI gates on `has_colony` (csp-view lib.rs:4843, which includes Dormant/Abandoned — so a
site can also never be re-founded after abandonment), but the MCP `sim.command` path does not.
**Suggestion**: Reject at the apply ("site already settled"); decide whether Abandoned releases the
site (UI currently says no).

### [SEVERITY: LOW] Finding 10 (DRY): Solve-input assembly, crew-unhook, and store-freeze logic duplicated

**File**: crates/csp-sim/src/colonies.rs:2011-2051 (`colony_net_solve`) vs :2251-2331 (`recompute_colony`); crew-unhook loop at :1738-1744, :1829-1835, :2212-2219; store-freeze loop at :1756-1761 and :2232-2237
**Problem**: `colony_net_solve` and `recompute_colony` hand-build the identical `SolveInput`
(env → light → levels → caps → concentrations → crew-gating) in two places (~35 lines); the
"crew → Unassigned + roster location" loop appears three times in this file (plus two more in
transfers.rs `land_crew`); the "re-anchor at now + zero net rates" freeze loop appears twice.
Drift between the copies is exactly how the firing-time re-check would silently diverge from the
scheduling recompute — the property the whole stale-event design depends on.
**Suggestion**: Extract `fn colony_solve_input(&self, id, tick) -> Option<...>` (one source of
truth for both), `fn unhook_crew_to_unassigned(&mut self, id, &[CrewId], shadow)`, and
`fn freeze_stores(colony: &mut Colony, now: u64)`.

---

## Observations (no action demanded)

- `publish_colonies` is O(live colonies × heap size) per advance interval — fine at v1 scale
  (verified by the 20-colony budget test), worth remembering if colony counts ever grow.
- Charging consumption at `fraction · request` (not the raw grant) can under-serve a later-priority
  consumer whose feedstock was over-granted to a multi-input module throttled on a different row —
  deterministic, no conservation issue, matches the documented "binding fraction" choice.
- Colony transitions (`PowerLost`/`LifeSupportDepleted`) are Clamping (warp demotes) but produce no
  notification/toast — the panel state string is the only surfacing. Cross-cluster with C24's warp
  refusal reasons.
- `next_colony_id` is hashed (lib.rs:8866) and `hash_colonies` walks every `Colony` field including
  the M8-B/C additions (`altitude_m`, fractions, solved/inspection ticks) — hash coverage complete.
- The restore gate's inspection pairing is correctly scoped to live colonies (lib.rs:10504-10521),
  so Abandoned colonies keeping a serialized `inspection_tick` with no event is coherent.

## PRD coverage table (§3.5.1 + §2.6.5)

| Requirement | Status | Evidence |
|---|---|---|
| Rule 1 two-state power + closed-form `TerminatorCrossing` (Silent) | **WIRED** (satellite synodic deviation — Finding 3; per-latitude average replaced by the tilt-0 closed form `(2/π)S·cosφ`, honestly documented, mathematically the same family) | colonies.rs:409-489, events.rs class_of |
| Rule 2 algebraic thermal `T_eq`, never integrated | **WIRED** | colonies.rs:1068-1095, test `thermal_steady_state_is_algebraic` |
| Rule 3 allocation pass (resource asc, `(priority, ColonyId, ModuleIndex)`, full-then-pro-rata) | **WIRED** — extended with grounding/sink gates beyond spec (sound; net-positive-cycle hole — Finding 1) | colonies.rs:825-1014 |
| Rule 4 next-event min (store bounds, terminator, inspection), `amount = rate·dt` literal | **WIRED** — epoch-anchored, partition-independence tested | colonies.rs:1159-1224, tests `colony_advance_is_partition_independent`, `store_bound_is_query_tick_independent` |
| Rule 5 states `Nominal/Degraded/Dormant/Abandoned`; `PowerLost`→Degraded; LS-depleted→crew Lost+Dormant; Dormant schedules nothing; `AbandonColony` @ 0.25; crash-damage reason | **WIRED** (deaths setting hard-coded `permanent` — per-save `off\|respawn\|permanent` absent game-wide; ImpactDamage terminal — Finding 6; LS-less colonies never deplete — Finding 4) | colonies.rs:2058-2412, balance.ron `abandon_recovery: 0.25` |
| §2.6.5 event classes (Terminator/StoreFull/Empty Silent; LifeSupplyDepleted Clamping; coarse events only) | **WIRED** (PowerLost/PowerRestored also Clamping — PRD-unnamed, sensible) | events.rs:449-514; `twenty_colonies_stay_inside_the_event_budget` |
| §2.6.5 pathological density (20 colonies × 6 converters × small buffers) | **WIRED** at colony scale (400-debris half is C29's scenario) | colonies.rs:3159-3244 |
| §3.5 founding cost for colony modules ("funds → facilities (…colony modules)") | **MISSING** — parallel-verified finding (not re-derived here); UI labels the button "prototype, no cost" | program.rs:1392, csp-view lib.rs:1299 |
| Player-visible surface (found → supply → produce → withdraw) | **WIRED** — full loop verified incl. join/deliver/withdraw buttons and panel rows; resupply reachability limited to rask×ore (Finding 5) | program.rs:1231-1370, colonies tests |

## Top 3 issues

1. **Finding 1 (HIGH)** — verified resource-minting converter cycle in the allocation pass; the
   grounding fixpoint cannot see net conservation.
2. **Finding 2 (HIGH)** — the authored Rask outpost sits exactly on the terminator; permanent day
   rests on a +6.1e-17 float residue (one ulp from silent, unrepairable colony death).
3. **Finding 3 (MEDIUM)** — tidally-locked moons modeled as locked to the sun (no day/night on
   Rask at all); balance authored against the wrong model (compounds Finding 2).
