# Verification Report — Cluster C09 (colony flow network / manufacture / life support)

Source verified: `crates/csp-sim/src/{colonies,manufacture,life_support}.rs`, cross-checked against `lib.rs`, `transfers.rs`, `events.rs`, `economy.rs`, and `csp-data/src/{validate,recipe}.rs`. Every finding was traced through the actual call paths; verdicts below are mechanism-level (code facts), with reachability assessed honestly — including one finding whose premise turned out to be **unreachable**.

**Verdicts: 8 CONFIRMED, 1 DISMISSED.**

Answers to the two headline questions:

- **"Does the power-pinned LS module really pin fraction 0 → immortal crew?" — YES.** Verified end-to-end (Finding 1). An ungrounded `LifeSupport` module registers zero consumer rows and zero consumption; the depletion scan still requires `stock <= EPS`, so stocked consumables freeze the crew in an immortal zombie state.
- **"Do stale committed transition events really fire after a resupply clears the predicate?" — YES.** Verified end-to-end on both scopes (Finding 2). The firings re-check only meta-gates (`live`, `life_support_enabled`, state membership, `v.crew.is_empty()`), never the scheduling predicate; a same-tick resupply cannot save the crew.

---

## [MEDIUM] Finding 1 — CONFIRMED: power-pinned `LifeSupport` module = immortal crew on stocked stores

Files verified: `colonies.rs:713-741` (`grounded_modules`), `:556-568` (`module_requests` LS arm), `:804` (`active = viable && grounded`), `:865-885` (`consumer_map` filters `!active`), `:925-933` (`consumed` skips `!active`), `:997-1017` (depletion scan), `:976-991` (`latent_ec_request` / `power_starved`).

Mechanism trace, all confirmed:

1. `module_requests` gives a crewed `LifeSupport` module the request set `consumes_per_crew ∪ {electric_charge: power_ec_per_s}` (`:556-568`).
2. `grounded_modules` applies to it like every other module: if ANY requested resource is neither stocked (`levels[r] > EPS`) nor produced by another grounded module, the module stays ungrounded (`:727-730`).
3. `active[i] = viable[i] && grounded[i]` (`:804`) → the ungrounded LS registers no `Consumer` rows (`consumer_map` skips `!active`, `:871-872`) and the `consumed` accumulator also skips it (`:927`) → it draws **nothing**, including fully stocked consumables.
4. The depletion scan (`:997-1017`) covers every LS module (inert/damaged/ungrounded included — the "damage kills" fix is real), but the kill condition is `stock <= EPS && grant < req − EPS`. With O2 stocked, `stock > EPS` short-circuits → `ls_depleted` false forever, and since nothing consumes the O2, the stock never drains.
5. `power_starved` uses the *latent* EC request summed over undamaged modules (`:976-983`), which includes the pinned LS's `power_ec_per_s` → `PowerLost` fires → `Degraded{PowerLost}`. `module_inert` (`:481-490`) gates only `Drill|Converter` in `Degraded`, so the LS stays *viable* but remains *ungrounded* (EC unstocked, unproduced) → permanently pinned at fraction 0.

Concrete reproduction verified: crewed `LifeSupport { consumes_per_crew: [(oxygen, q)], power_ec_per_s: p > 0 }` + stocked O2 `Store` + `Battery` at 0 + no EC producer → `Degraded{PowerLost}` forever, crew `Active`, O2 level never decreasing.

Test-coverage claim verified: **all six** test-side `LifeSupport` instantiations use `power_ec_per_s: 0.0` (`colonies.rs:2779, 3398, 3474, 3594, 4161, 4232`), so the O2 request alone grounds the module off its own stock and no test exercises the pin. The damaged twin is also verified: `module_inert` makes a damaged LS non-viable → same pin; `damaged_life_support_kills` (`:3568-3628`) passes `stores: vec![]` (`:3597`) — no O2 store — so the stocked-pantry variant is untested.

Note (minor caveat, not affecting the verdict): the scenario requires `life_support_enabled` (default OFF per `life_support.rs:37-45`) and a real `Store`/`Battery` module carrying the stock — founding `def.stores` levels clamp to module-derived capacity (`colonies.rs:1500-1511`), so "stocked reserves" requires authored capacity. Both are ordinary construction.

Severity assessment: MEDIUM is fair — the failure is silent, permanent, and violates the documented "switches to reserves; crew dies when a required store empties" contract (module doc `:72-80`) in exactly the states it was written for. The suggested fixes (ground LS on viability only, or treat unsatisfied LS requests as depletion evidence) are consistent with the code.

---

## [MEDIUM] Finding 2 — CONFIRMED: committed transition events fire without re-checking their predicate

Files verified: `colonies.rs:2110-2140` (transition scheduling), `:2089` (`drop_colony_events(id, true)`), `:1101-1109` (`is_colony_boundary`), `:1909-1983` (firing arms); `life_support.rs:327-334` (depletion scheduling), `:363-394` (`fire_vessel_ls_depleted`); `transfers.rs:1065-1100` (`credit_end` → `recompute_colony`); `lib.rs:6407-6526` (`fire_due_events` ordering).

Mechanism trace, all confirmed:

- **Colony LS kill after resupply.** `ColonyStoreEmpty{oxygen}` at tick T fires → `recompute_colony(id, now=T)` → `ls_depleted` true → `LifeSupportDepleted` scheduled at T+1 (`:2135-2139`). A `TransferComplete{→colony, oxygen}` at T with higher seq fires later in the same `(tick, seq)` batch → `credit_end` lands O2 (`transfers.rs:1087-1095`) → `recompute_colony` → `ls_depleted` now false → nothing new scheduled — but `drop_colony_events(id, true)` (`colonies.rs:2089`) drops only `is_colony_boundary` kinds (terminator/store-bound/inspection, `:1101-1109`), so the pending notification survives. At T+1 the firing checks only `live && life_support_enabled` (`:1945`) → all crew `Lost`, colony `Dormant`, **with O2 stocked**.
- **Ordering realism**: seq is assigned at insertion. The `ColonyStoreEmpty` was published at an earlier recompute; the emergency resupply transfer is created *after* that (the whole point of the scenario) → `TransferComplete` carries the higher seq → the fatal order (boundary first, resupply second) is the **natural** play order, not an exotic one.
- **Vessel twin.** `fire_life_support_check` at T debits what exists, flags `short`, schedules `VesselLifeSupportDepleted` at `clock+1` (`life_support.rs:327-334`); a same-tick `TransferComplete` with higher seq fills the tanks; `fire_vessel_ls_depleted` (`:368-376`) checks only the flag, vessel existence, and `!v.crew.is_empty()` → crew `Lost` with full tanks. No debit-free re-check exists.
- **Power blip.** EC store empties → `PowerLost` queued for T+1 (`:2117-2121`); EC credit lands at T → `power_starved` false → the pending event survives; at T+1 firing checks only `matches!(state, Nominal)` (`:1910`) → `Degraded{PowerLost}` with a charged battery → the firing's own recompute then schedules `PowerRestored` for T+2 (`:2123-2133`) → a spurious one-tick Degraded (with drills/converters inert for that interval per `module_inert`) plus a spurious notification pair.
- **No cancellation path exists**: the per-kind dedup (`:2119, 2131, 2137`) prevents duplicates but not removal; the only scrub of notification kinds is the `SetLifeSupportEnabled` toggle's whole-heap filter (`life_support.rs:243-254`), which is a different trigger entirely.

Severity assessment: MEDIUM is fair — deterministic wrong-world crew loss (`CrewStatus::Lost` + `Dormant`) or a spurious power blip, reachable through ordinary commands (`TransferColonyResource` after `JoinColonyInventory`). The firing-side predicate re-check the report suggests is consistent with the code's own belt-and-suspenders pattern (it already re-checks the flag it just scrubbed for).

---

## [LOW] Finding 3 — CONFIRMED: `next_terminator_tick` skips a real crossing inside the EPS window

File verified: `colonies.rs:445-458`, verbatim:

```rust
let dt = ((PI / 2.0 - h_now) / light.synodic_rate).rem_euclid(spacing);
let dt = if dt > EPS { dt } else { spacing };
Some(now.saturating_add((dt * TICK_HZ as f64).ceil() as u64))
```

`EPS = 1e-9` (s, `:136`). `dt ∈ (0, 1e-9]` takes the `spacing` branch, publishing the crossing a full half-solar-day out, when `ceil(dt·64)` would be 1 tick (`dt·64 ≤ 6.4e-8 → ceil = 1`). Meanwhile `daylight_at(now)` just stamped the pre-crossing side, and nothing re-stamps until the wrongly-late `TerminatorCrossing` fires → solar production, net rates, and store bounds all computed on the wrong daylight state for up to `spacing` seconds (~half a solar day). Recomputes happen at arbitrary integer ticks (every command, store bound, inspection) and crossings land at arbitrary phase, so `dt ∈ (0, 1e-9]` is a nonzero-probability, unbounded-in-count edge over a long save. Deterministic and self-healing at the next crossing — LOW is fair. The suggested fix (`dt > 0.0`, or a tick-scaled epsilon) matches the intent; `dt == 0` exactly-on-the-terminator is the only case the `spacing` branch is for.

---

## [LOW] Finding 4 — CONFIRMED: `apply_surface_impacts` and `vessel_colony_linked` compute the colony's surface point differently

Files verified: `colonies.rs:1745-1804` vs `transfers.rs:1198-1224`.

- `apply_surface_impacts` builds `SurfacePose { altitude_m: 0.0, … }` and calls `latched_pos(_, radius_m)` where `radius_m = env.radius_m` is the **physics** environment's radius for the dominant body (`colonies.rs:1756-1791`). The dominant-body gate (`:1775`) makes the physics env the matching table, but the altitude is hardcoded 0.
- `vessel_colony_linked` builds the same pose with `altitude_m: site_defs[co.site].altitude_m` (`transfers.rs:1211-1221`) and calls `latched_pos(_, colony_env[co.body].radius_m)` (`:1208`) — the **colony** env's radius.
- `latched_pos` computes `r = body_radius_m + pose.altitude_m` (`transfers.rs:692-697`), so the two call sites disagree on both inputs.
- The `Colony` record indeed stores only body/lat/lon — `apply_found_colony` resolves the site but drops `altitude_m` (`colonies.rs:1448-1449, 1524-1529`).

Consequence verified: for a colony founded on an elevated site, the impact check measures distance to a point `altitude_m` below the true pose; an impact landing at the colony reads ~`altitude_m` away and exceeds `transfer_cfg.radius_m` (default 100 m, `transfers.rs:257`) → elevated colonies are effectively impact-immune. All shipped tests use `altitude_m: 0.0` sites (`colonies.rs:2254`, `manufacture.rs:389`, `transfers.rs` fixtures), so the seam is unexercised. The radius-source divergence (`env.radius_m` vs `colony_env[body].radius_m`) is a second, quieter disagreement if the two tables ever differ for the dominant body. LOW is fair — authored-data-dependent distance error, deterministic.

---

## [LOW] Finding 5 — DISMISSED: the off-grid `LifeSupportCheck` re-arm is unreachable

The finding claims a §2.6.5 deferred-batch flush lets an event "fire with `event.tick < clock`", making `event_tick + period <= clock` so `.max(clock+1)` wins and permanently phase-shifts the grid (`life_support.rs:341-352`).

**The premise is false.** Verification of the flush mechanics:

- `fire_due_events` pops `e.tick <= t_next` (`lib.rs:6416`), and `t_next = min(effective_target, event_tick=heap_min, now+budget)` (`lib.rs:6795-6801`) — so `t_next ≤ heap_min`, meaning every event that fires has `e.tick == heap_min == t_next`. The clock is advanced to `t_next` (`try_advance`, `lib.rs:6836`) *before* `fire_due_events` runs → **every event fires with `event.tick == clock`**, always.
- The deferred tail sits **at** the clock, not behind it — the code's own doc: "the same-tick tail stays in the heap — *at the clock, since `t_next == clock` now*" (`lib.rs:6394-6399`). Flush iterations are zero-advance (`t_next = now`), so tail events still fire with `event.tick == clock`.
- `schedule_event` Fatal-rejects `tick <= clock` (`lib.rs:3918-3920`), `t_next`'s heap-min clamp makes it impossible for the clock to pass a pending event, and `World::restore` rejects `e.tick < clock_ticks` (`lib.rs:2386-2388`). There is no path by which an event fires late.

Therefore `event_tick + period <= clock` can never hold (it requires `event_tick < clock` since `period ≥ 1`), `.max(clock + 1)` is dead code, and the re-arm is always `event_tick + period` — which stays on the absolute grid because every `LifeSupportCheck` is published on-grid (`life_support.rs:283`: `next = (clock/period + 1)·period`) and the re-arm preserves grid alignment inductively.

Corollary evidence: had the off-grid re-arm been reachable, the restore gate would have additionally Fatal'd on it — `LifeSupportCheck` snapshots are rejected when `e.tick % check_period_ticks != 0` (`lib.rs:2502-2506`). The system enforces grid alignment from both ends.

The `colonies.rs:1855-1858` comment this finding apparently leaned on — "in a deferred-batch flush the event's tick sits behind the clock" — is loosely worded (the tail sits *at* the clock, not behind it), but the clock-based re-solve it justifies is harmless either way.

**DISMISSED**: unreachable code path; the re-arm cannot leave the grid. (The `.max(clock+1)` guard could still be kept as defense-in-depth, but it is not a defect.)

---

## [LOW] Finding 6 — CONFIRMED: `ManufactureCraft` reports reachable input errors as `Fatal`

Files verified: `manufacture.rs:83-86, 118-119, 168-171, 219-222`; `economy.rs:197-207, 583-586, 812-814`; `transfers.rs:815, 898, 1248, 1343, 1480`; `lib.rs:2315-2317, 6748-6751, 6791-6792`.

- **Tick-overflow Fatal.** `mass` is gated only by `is_finite` (`:168-171`). `ticks_for` computes `(mass/rate·TICK_HZ).ceil() as u64` — a huge-but-finite mass (e.g. `mass_kg ≈ 1e19+` at the default rate) saturates `as u64` to `u64::MAX`; `now.checked_add(u64::MAX)` then fails → `SimError::Fatal("manufacture: tick overflow")` (`:219-222`) whenever `now ≥ 1`. The `check_tank_masses` gate (`transfers.rs:415-432`) only bounds mass *below* by tank wet mass — no upper bound exists, and `csp_phys::weld` accepts finite mass. So a player-supplied `CraftDef` reaches this `Fatal`, which propagates through `drain_at` → `step_frame` `Err` (`lib.rs:6791-6792`) — a frame abort with no `rejected` entry, where every sibling validation failure in the same function is `Recoverable`.
- **Config-gate severity asymmetry.** `!self.economy_cfg.is_valid()` → `Fatal` at `manufacture.rs:118-119`, while the identical check is `Recoverable` in `apply_recover_vessel` (`economy.rs:812-814`) and all five `transfer_cfg.is_valid()` sites (`transfers.rs:815-816, 898-901, 1248-1251, 1343-1346, 1480-1483`). Six call sites, five picked `Recoverable`, one picked `Fatal` for the same condition.
- **Reachability of the invalid config**: `set_economy_config` accepts an invalid config unchecked (`economy.rs:583-586`); the snapshot-restore gate then `Fatal`s on it (`lib.rs:2315-2317`), so a bad config both crashes `ManufactureCraft` and bricks restores — the command path should have rejected it first.

LOW is fair: input-reachable `Fatal`s where `Recoverable` is the established convention for identical conditions.

---

## [LOW] Finding 7 — CONFIRMED (and reachable, more strongly than the report argued): `allocation_pass` returns `grants` one sweep stale when the cap exhausts mid-change

File verified: `colonies.rs:806-861`, consumed at `:1009` (`ls_depleted`).

Code fact confirmed exactly as reported: each sweep computes `produced` from the *current* `fractions`, derives `grants` from `produced`, then updates `fractions = min(prev, computed)`. On the `!changed` exit, `grants` is consistent with the final fractions. But if the `n+1` cap is hit with `changed == true` on the last sweep, `fractions` were updated *after* `grants` was computed — the returned `Allocation` pairs a stale `grants` map with a post-loop `produced` recomputed at the final fractions (`:854`). `ls_depleted` reads `grants[(i,r)]` while `consumed`/`net_rates` charge at the final fractions → the depletion predicate and the consumption ledger disagree by one sweep's delta.

**Reachability — stronger than the report's "geometrically-converging case" hedge.** The cap is not merely theoretically hittable; a concrete authored shape hits it:

- A recipe may legally list the **same resource in `inputs` and `outputs`** — `validate_recipes` checks duplicates per-list only (`validate.rs:1406-1430`) and adds no cross-list overlap rule; `colony_def_fields_finite` checks only the `electric_charge` collision (`colonies.rs:1310-1324`). E.g. `{inputs: [(r, 1.0)], outputs: [(r, 0.5)]}` — a net-sink "recycler".
- With another grounded producer of `r` (e.g. a converter with empty `inputs` — vacuously grounded, `grounded_modules` `all()` on an empty set), the net-sink module grounds normally (`j != i` is satisfied by the other producer).
- Its fraction then obeys the affine recurrence `f ← (a + p·f)/q` (`a` = the other producer's rate, `p` = own output rate, `q` = request, with `a + p < q` for starvation). With `0 < p < q` this converges **geometrically** toward `f* = a/(q−p)` — strictly decreasing, never reaching the fixpoint in finite sweeps → `changed == true` on every sweep → the `n+1` cap exits mid-change → stale `grants` returned.
- The grounding fixpoint does NOT catch this: it only prevents a module from grounding *solely* on itself; a resource produced by a third party keeps the module grounded while its own output still feeds its own grant.

So the stale-grants path is reachable via legal authored data, and the victim is exactly the `ls_depleted` grant check. LOW is fair (requires an exotic but legal recipe shape; error bounded to one sweep's delta).

Suggested fix stands: recompute `grants` after the loop alongside `produced`, or break before the fraction update on the capped sweep.

---

## [LOW] Finding 8 — CONFIRMED: `apply_surface_impacts` propagates `Recoverable` into `step_frame`; damage also applies retroactively at range start

Files verified: `colonies.rs:1745-1804`, `:2006-2010` (env-miss `Recoverable`); `lib.rs:3690-3705` (clock reset + call site), `:6808-6821` (`step_frame` propagation), `:6748` ("`Err` escapes only for `Fatal`"), `colonies.rs:1362-1364` (`set_colony_env` wholesale replace).

- **Severity leak confirmed.** `step_bubble_range` ends with `self.apply_surface_impacts(&impacts, shadow)?` (`lib.rs:3705`); any `Err` propagates to `step_frame` (`lib.rs:6809-6820` → `shadow.restore; return Err(e)`). Inside, `apply_damage_module` → `recompute_colony` returns `Recoverable("colony: no colony environment for body")` (`colonies.rs:2006-2010`) — a `Recoverable` escaping `step_frame` as `Err`, violating the "`Err` escapes only for `Fatal`" contract. The dormant/abandoned filter exists precisely for this reason ("a dormant colony in the radius would otherwise wedge every advance", `:1770-1774`) but the env-miss arm is unfiltered.
- **Reachability confirmed**: `set_colony_env` replaces the entire table (`:1362-1364`) — a mid-run call that drops a body hosting a live colony leaves `recompute_colony`'s env lookup failing; the first surface impact within `transfer_cfg.radius_m` then makes every `step_frame` that touches the range return `Err` — a permanent wedge (and `publish_colonies` `continue`s past the env-less colony at `:2162-2163`, so nothing else trips it first — the wedge is impact-triggered). The report's own caveat applies: requires construction-config mutation mid-run, so low probability, high consequence.
- **Retroactive damage confirmed**: `self.clock = SimTime(now)` (`lib.rs:3692`) restores the range-start clock before `apply_surface_impacts` runs → `apply_damage_module` → `recompute_colony` re-anchors and re-solves at `now`, treating the damaged module as inert for the **whole** `(now, t_next]` interval — production that legitimately accrued between `now` and the impact tick is retroactively removed, and any store-bound event inside the range is dropped and republished under post-damage rates. Deterministic, bounded, but a real rewind of up to one interval per impact.

LOW is fair for both halves.

---

## [LOW] Finding 9 — CONFIRMED: a damaged `Store`/`Battery` keeps contributing capacity

Files verified: `colonies.rs:237-239` (doc claim "produces, consumes, and stores nothing"), `:1472-1499` (one-time capacity aggregation at founding), `:481-490` (`module_inert`), `:948-960` (`eps_sigma_a` filters `active`), `transfers.rs:1087-1093` (`credit_end` caps at `store.capacity`), `colonies.rs:1559-1568` (abandon salvage at `level_at`).

Confirmed: `module_inert` zeroes a damaged module's produce/consume half, but `ColonyStore::capacity` is the Σ of module capacities computed **once at founding** and never re-derived — a damaged `Battery` keeps holding charge, a damaged `Store` keeps holding units and accepting `credit_end` landings, and `AbandonColony` still salvages their contents. The thermal solve demonstrates the intended discipline — `eps_sigma_a` filters to `active` modules (`:948-960`) — so a damaged `Radiator` stops radiating while a damaged `Store` incorrectly keeps storing. Deterministic capacity windfall; LOW is fair.

---

## Report's "checked and clean" section — spot-verified, stands

- `grounded_modules` least-fixpoint uniqueness/monotone cap (`:713-741`), `earliest_store_bound` anchor-relative partition independence (`:1054-1096`), notification dedup (`:2119-2139`), dormant store-freeze (`:1975-1982`, `dormant_freezes_stores` test), toggle scrub (`life_support.rs:242-262`), manufacture spend ordering (`manufacture.rs:179-237`), vessel LS check structure (`life_support.rs:295-355`), `(tick, seq)` heap ordering — all verified as described.
- One addendum: `fire_construction_complete` is effectively total (the report's claim holds) — `deliver_built_vessel` → `reliability::allocate_records` returns only `Fatal` (`reliability.rs:438-452`), so the no-zombie-drop note at `lib.rs:6450-6470` isn't a live hole for builds.

---

## Summary table

| # | Finding | Verdict | Notes |
|---|---------|---------|-------|
| 1 | Power-pinned LS → immortal crew | **CONFIRMED** | Full mechanism verified; all 6 test LS modules use `power_ec_per_s: 0.0`; damaged twin verified (`damaged_life_support_kills` has no O2 store) |
| 2 | Stale committed transitions fire after resupply | **CONFIRMED** | Both colony and vessel scopes; fatal ordering is the natural play order; power blip verified |
| 3 | Terminator EPS skip | **CONFIRMED** | Verbatim at `colonies.rs:455-456`; rare, self-healing |
| 4 | Impact-pose inconsistency (altitude + radius source) | **CONFIRMED** | `colonies.rs:1782-1791` vs `transfers.rs:1208-1223` |
| 5 | Off-grid `LifeSupportCheck` re-arm | **DISMISSED** | `event.tick < clock` is unreachable — deferred tails fire at `tick == clock` (`lib.rs:6394-6399, 6416, 6795-6801`); `.max(clock+1)` is dead code; restore gate (`lib.rs:2502-2506`) corroborates |
| 6 | Manufacture `Fatal` severities | **CONFIRMED** | `ticks_for` saturation + 1-of-6 `Fatal` config gate; `set_economy_config` accepts invalid config |
| 7 | Stale `grants` on cap exhaustion | **CONFIRMED** | Code fact verified; *reachable* via legal same-resource input/output recipe (net-sink converter + second producer → geometric convergence, no finite-step fixpoint) — stronger than the report's claim |
| 8 | Impact `Recoverable` leak + retroactive damage | **CONFIRMED** | Env-miss arm unfiltered; `?` at `lib.rs:3705` violates `Err`-only-for-`Fatal`; damage solves at range-start clock |
| 9 | Damaged Store/Battery keeps capacity | **CONFIRMED** | Capacity aggregated once at founding; radiators done right via `active` filter, stores not |
