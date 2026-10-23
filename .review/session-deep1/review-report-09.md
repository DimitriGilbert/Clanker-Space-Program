# Deep Review — Cluster C09 (colony flow network / manufacture / life support)

Scope: `crates/csp-sim/src/{colonies,manufacture,life_support}.rs`, cross-read against the machinery they ride on: `lib.rs` (`step_frame`, `drain_at`, `fire_due_events`, `step_bubble_range`, `advance_interval_subsystems`, `schedule_event`, `TickShadow`), `transfers.rs` (`credit_end`, `land_crew`, `apply_transfer_colony_resource`, `vessel_colony_linked`, `latched_pos`, `debit`/`credit`), `events.rs` (`EventKind`, `class_of`, `ScheduledEvent` ordering), `economy.rs` (`EconomyConfig::is_valid`, `charge`, `craft_cost_micros`).

Verdict up front: the fixpoint/anchor architecture is genuinely solid — the least-fixpoint grounding + greatest-fixpoint grant solve is order-independent, the anchor-relative `earliest_store_bound` is partition-independent by construction, and the boundary scrub/republish discipline is clean. The real problems are two **logic** holes in the transition layer — an `LifeSupport` module that can't be grounded leaves crewed colonies in an immortal zombie state, and committed transition events fire without re-checking the predicate that earned them — plus a set of smaller edge defects (terminator EPS skip, impact-pose inconsistency, off-grid check re-arm, Fatal-vs-Recoverable severity mismatches, a stale-grants cap path).

---

### [MEDIUM] Finding 1: A crewed `LifeSupport` module pinned at fraction 0 by the grounding fixpoint stops consuming all reserves — crew is immortal on stocked stores
File: `crates/csp-sim/src/colonies.rs:713-741` (`grounded_modules`), `:556-568` (`module_requests` — `LifeSupport` arm), `:997-1017` (depletion scan)

Problem
`grounded_modules` computes the least fixpoint of "every requested resource is stocked or produced by ANOTHER grounded module" and applies it to *every* viable module, `LifeSupport` included. A `LifeSupport` module's request set is `{consumes_per_crew…} ∪ {power_ec_per_s → electric_charge}` — so if *any* requested resource is ungrounded (EC with an empty battery and no live producer is the realistic one), the whole module becomes `active[i] = false`: it registers no consumer rows and consumes **nothing** — including the consumables that *are* stocked.

The depletion scan (`:997-1017`) only kills crew when a `consumes_per_crew` store is `<= EPS` *and* under-granted. With the module pinned and every consumable stocked, `stock > EPS` on every row → `ls_depleted` stays false forever, and the stocked reserves never drain because the module consumes nothing.

Concrete reproduction: colony = crewed `LifeSupport { consumes_per_crew: [(oxygen, q)], power_ec_per_s: p > 0 }` + O2 store + battery at 0 + no EC production. `power_starved` fires `PowerLost` → `Degraded{PowerLost}` (the latent EC request counts the LS draw); in every subsequent solve the LS module stays ungrounded (EC neither stocked nor produced) → O2 is frozen, never consumed → crew lives forever inside a permanently dead colony. Every shipped test uses `power_ec_per_s: 0.0`, which is why the tests pass — with `power_ec_per_s: 0` the only request is O2, which grounds off its own stock.

The damaged-module twin is identical in mechanism: `module_inert` (`:481-490`) makes a damaged LS module non-viable → same pin → a colony with a *destroyed* life support and a stocked pantry keeps its crew alive forever (the `damaged_life_support_kills` test only works because it has no O2 store).

Impact
The "life support switches to reserves and keeps draining; crew dies when a required store empties" contract (module doc `:72-80`) is violated in exactly the states it was written for. A `PowerLost` colony with stocked O2 and a powered-scrubber LS module is a permanent zombie: `Degraded{PowerLost}` forever, crew `Active`, O2 never depleting. Deterministic, but observably wrong — and it silently removes the death pressure the depletion event exists to deliver.

Suggestion
Exempt `LifeSupport` from the grounding requirement (it is a pure drain, not a producer-gated converter — ground it on viability only) so its consumable draw continues on reserves and the depletion boundary actually arrives. Alternatively, treat an ungrounded/inert crewed LS module's unsatisfied *requests* — not just `consumes_per_crew` — as depletion evidence, so an unpowered LS kills on schedule rather than freezing.

---

### [MEDIUM] Finding 2: Committed transition events fire without re-checking their scheduling predicate — a same-tick resupply cannot save the crew (both colony and vessel scope)
File: `crates/csp-sim/src/colonies.rs:2108-2140` (notification scheduling), `:1938-1983` (`LifeSupportDepleted` firing), `:2089` (`drop_colony_events(id, true)` keeps notifications); `crates/csp-sim/src/life_support.rs:327-334` (depletion scheduling), `:363-394` (`fire_vessel_ls_depleted`); `crates/csp-sim/src/transfers.rs:1081-1099` (`credit_end` → `recompute_colony`)

Problem
Transition notifications (`PowerLost`, `PowerRestored`, `LifeSupportDepleted`, `VesselLifeSupportDepleted`) are scheduled one tick out and are deliberately not scrubbed by later recomputes. The firings re-check *meta* gates (`live` state, `life_support_enabled`, `Nominal`/`Degraded{PowerLost}` membership, `v.crew.is_empty()`) but **never re-evaluate the predicate that scheduled them**. Anything that clears the predicate between scheduling and firing — a resupply landing in the same tick — does not cancel the transition.

Concrete chains, both fully reachable via ordinary commands:
- Colony: O2 hits 0 → `ColonyStoreEmpty` at T fires → `ls_depleted` → `LifeSupportDepleted` queued for T+1. A `TransferComplete{→colony, oxygen}` with higher seq fires at T → `credit_end` lands O2 and reruns `recompute_colony` → `ls_depleted` now false — but the pending event survives (`boundary_only=true`, `:2089`). At T+1 the firing checks only `live && enabled` (`:1945`) → every crew member `Lost`, colony `Dormant`, **with O2 stocked**.
- Vessel: `LifeSupportCheck` at T debits what exists and queues `VesselLifeSupportDepleted` for T+1 (`life_support.rs:327-334`); a `TransferComplete` at T with higher seq fills the tanks; the firing (`:368-376`) checks flag/vessel/crew-presence only → crew `Lost` with full O2.
- Power (same class, self-healing): EC store empty → `PowerLost` queued at T+1; an EC credit lands at T → at T+1 the colony still goes `Degraded{PowerLost}` with a charged battery; the firing's recompute then queues `PowerRestored` for T+2 and recovers — a spurious one-tick Degraded blip plus an extra notification pair.

The event kinds were made per-colony/per-vessel dedup'd precisely to prevent *duplicates* (`:2119`, `:331`) — the same machinery has no *cancellation* path, and unlike the toggle scrub, no producer ever removes a notification whose predicate cleared.

Impact
Permanent, wrong-world crew loss (`CrewStatus::Lost` + `Dormant`) or a spurious power-blip from a one-tick window — and a common player action (emergency resupply beating the depletion by a tick) is explicitly punished. Deterministic in every partition, but a real logic hole: the code already re-checks meta-gates at firing, so re-checking the predicate is consistent with its own belt-and-suspenders design.

Suggestion
For the colony side: at `LifeSupportDepleted`/`PowerLost`/`PowerRestored` firing, re-run the cheap predicate half of the solve (or at minimum re-check the named store levels / `power_starved` terms) and no-op if cleared. For the vessel side: have `fire_vessel_ls_depleted` re-run the shortfall check (`debit`-free: `vessel_resource >= required` for the period) before killing. The alternative — scrubbing notifications from `recompute_colony` when the predicate is false — also works but changes event counts; the firing-side re-check is the minimal honest fix.

---

### [LOW] Finding 3: `next_terminator_tick` skips a real crossing inside the EPS window
File: `crates/csp-sim/src/colonies.rs:445-458`

Problem
`let dt = ((PI/2 − h_now)/synodic_rate).rem_euclid(spacing); let dt = if dt > EPS { dt } else { spacing };` — `dt == 0` means "sitting exactly on the terminator" (next crossing is a half solar day out), but the guard also swallows `dt ∈ (0, 1e-9 s]`. A recompute landing within one nanosecond before a crossing publishes the *next* crossing a full `spacing` later, skipping the imminent one. The stored `daylight` was just stamped by `daylight_at(now)` for the *pre*-crossing side and won't be re-stamped until the skipped event — i.e., solar modules run on the wrong daylight state (and wrong net rates / wrong store bounds) for up to half a rotation period.

Evidence
Any integer tick is a potential recompute (commands, store bounds, inspections), and crossings land at arbitrary phase. `dt ∈ (0, 1e-9]` requires the closed-form crossing to fall within 1 ns after an integer tick — rare per crossing (~1e-7–1e-6 of a tick at `TICK_HZ=64`) but nonzero and unbounded in count over a long save.

Impact
Deterministic and self-healing (the *next* crossing still fires and `daylight_at` re-stamps the correct side), but a genuine schedule error with material consequences for the window: wrong `net_rate_per_s` for half a sol.

Suggestion
Compare against a *tick-scaled* epsilon — the intent is "dt rounds down to zero ticks", i.e. `dt * TICK_HZ < 1` should still schedule `now + 1` (the ceil branch already does this correctly); only exact `dt == 0` should take the `spacing` branch. `if dt > 0.0 { dt } else { spacing }` (or a much smaller floor) fixes it.

---

### [LOW] Finding 4: `apply_surface_impacts` measures colony distance at `altitude_m: 0` and the physics env radius — `vessel_colony_linked` uses site altitude and the colony-env radius
File: `crates/csp-sim/src/colonies.rs:1782-1794` vs `crates/csp-sim/src/transfers.rs:1208-1223`

Problem
Two different pieces of code compute "the colony's surface point" for the same `transfer_cfg.radius_m` reach and disagree on both inputs:

- `apply_surface_impacts` builds `SurfacePose { altitude_m: 0.0, … }` and calls `latched_pos(_, env.radius_m)` with the **physics** environment's radius for the dominant body.
- `vessel_colony_linked` builds the same pose with `altitude_m: site_defs[co.site].altitude_m` and calls `latched_pos(_, colony_env[body].radius_m)` with the **colony** env's radius.

The `Colony` record itself stores lat/lon but no altitude; founding resolves the site's pose but drops `altitude_m` (`:1448-1453` keeps only body/lat/lon). So a colony founded on an elevated site (`SiteDef::altitude_m > 0`) has its impact proximity measured to a point `altitude_m` *below* its actual pose — a debris impact at the colony reads ~`altitude_m` away and exceeds `transfer_cfg.radius_m`. Sites at altitude are effectively impact-immune; the `radius_m` difference is a second, quieter divergence if the two env tables ever disagree for the dominant body.

Impact
The M8-C impact seam works at datum-level sites (all tests use `altitude_m: 0.0`) and silently never damages elevated colonies. Deterministic; wrong by an authored-data-dependent distance.

Suggestion
Resolve `site_defs[colony.site].altitude_m` (or store altitude on `Colony` at founding — it's hashed-adjacent pose data) and use the same radius source as `vessel_colony_linked` (`colony_env[body].radius_m`) for one shared colony-surface-point helper.

---

### [LOW] Finding 5: `LifeSupportCheck` re-arm can leave the absolute grid after a deferred-batch flush
File: `crates/csp-sim/src/life_support.rs:341-352`; flush mechanics at `crates/csp-sim/src/lib.rs:6416-6417`, `:6753-6759`

Problem
The check is documented as firing "once per `check_period_ticks` on the absolute period grid" (`:12-15`). The publish computes `next = (clock/period + 1)·period` — on-grid. The in-firing re-arm computes `next = event_tick + period` — also on-grid *when `event_tick` is on-grid*. But in a §2.6.5 deferred-batch flush (`MAX_EVENTS_PER_BATCH` exceeded at a tick), events fire with `event.tick < clock`; then `event_tick + period` (still grid-aligned) may be `<= clock`, so `.max(clock+1)` wins → the next check lands at `clock + 1`, an **off-grid** tick. Every subsequent re-arm adds `period` to that shifted tick — the cadence is permanently phase-shifted off the documented grid until a toggle scrub or publish repair resets it.

Impact
Deterministic (depends only on `(tick, seq)` order), but contradicts the module's "absolute grid" claim and shifts the debit cadence by up to `period` — and the shift persists indefinitely: `publish_life_support` only fires when *no* check is pending, so it never re-grids.

Suggestion
Compute the re-arm on the grid like the publish does: `next = (event_tick/period + 1)·period` then clamp `> clock` (e.g. `max(next, clock+1)` is only correct if the grid step itself exceeds the flush delay — a `while next <= clock { next += period }`-style normalization to the first grid tick strictly after the clock is the honest form).

---

### [LOW] Finding 6: `ManufactureCraft` reports reachable input errors as `Fatal` — saturated build-tick overflow and the config gate's severity asymmetry
File: `crates/csp-sim/src/manufacture.rs:83-86` (`ticks_for`), `:118-119` (config gate), `:219-222` (`complete_tick`)

Problem
Two reachable paths produce `SimError::Fatal` where every sibling apply uses `Recoverable`:

- `ticks_for(mass, rate)`: `mass` is only checked `is_finite` (`:168-171`). A huge-but-finite mass (e.g. 1e18 kg) makes `mass/rate·TICK_HZ` saturate `as u64` to `u64::MAX`; `now.checked_add(u64::MAX)` then fails → `SimError::Fatal("manufacture: tick overflow")`. The command rolls the whole iteration back correctly, but `step_frame` returns `Err` to the caller — per `lib.rs:6749` "`Err` escapes only for `Fatal`" — so a player-supplied craft converts a validation-class rejection into a frame abort with no `rejected` entry.
- `!self.economy_cfg.is_valid()` → `SimError::Fatal("economy: invalid config")`. `set_economy_config` accepts an invalid config without complaint; the identical gate in `apply_recover_vessel` (`economy.rs:812-814`) and all five `transfer_cfg.is_valid()` sites (`transfers.rs:815,898,1248,1343,1480`) uses `Recoverable`. One of six config gates picked a different severity for the same condition.

Impact
Minor — bad config or absurd mass — but both are input-reachable `Fatal`s (the class reserved for invariant bugs), and the snapshot-restore gate (`lib.rs:2315`) also validates `economy_cfg`, so an invalid-config save additionally can't be restored at all; the command path should have rejected it first.

Suggestion
Clamp `ticks_for` (`t.min(u64::MAX - now)`) or reject `complete_tick` overflow as `Recoverable` before commit; downgrade the config gate to `Recoverable` to match the other five sites.

---

### [LOW] Finding 7: `allocation_pass` returns `grants` one sweep stale when the fixpoint cap exhausts mid-change
File: `crates/csp-sim/src/colonies.rs:806-858`

Problem
Each sweep computes `produced` from `fractions`, derives `grants` from `produced`, then updates `fractions = min(prev, computed)`. On the normal exit (`changed == false`) `grants` is consistent with the final `fractions`. But if the `n + 1` sweep cap is hit with `changed == true` on the last sweep, `fractions` were updated *after* `grants` was computed — the returned `grants` corresponds to the previous fraction vector, while `produced` is recomputed post-loop at the final fractions (`:854`). The returned `Allocation` is internally inconsistent: `ls_depleted` (`:1009`) reads `grants[(i, r)]` while `consumed`/`net_rates` charge at the new `fractions`.

Evidence
Convergence needs ~depth sweeps, so `n + 1` covers chains — but nothing bounds a cyclic/starved network's contraction to `n` decrements; a geometrically-converging case exhausts the cap legitimately. When it does, the depletion predicate and the consumption ledger disagree by one sweep's delta — enough to flip a `g < req − EPS` verdict at the margin.

Impact
Deterministic (the cap is fixed), rare, and bounded to one sweep of error — but it's exactly the `ls_depleted` path (crew death) that consumes the stale map.

Suggestion
Recompute `grants` after the loop alongside `produced` (one extra pass over `consumers` with final fractions), or break *before* the fraction update when the cap is reached so the last committed `grants`/`fractions` pair is consistent.

---

### [LOW] Finding 8: `apply_surface_impacts` propagates `Recoverable` into `step_bubble_range` — a non-Fatal error aborts `step_frame` and wedges the advance; damage also applies retroactively at range start
File: `crates/csp-sim/src/colonies.rs:1745-1804`; `crates/csp-sim/src/lib.rs:3690-3705` (call site + clock reset), `:6809-6821` (propagation)

Problem
`step_bubble_range` ends with `self.clock = SimTime(now)` (range start), then `self.apply_surface_impacts(&impacts, shadow)?`. Two issues on this seam:

- **Severity leak.** The `?` forwards `apply_damage_module` → `recompute_colony`'s only `Recoverable` — `"colony: no colony environment for body"` (`:2006-2010`) — straight into `step_frame`'s `return Err(e)`. The dormant/abandoned filter exists precisely because "a dormant colony in the radius would otherwise wedge every advance" (`:1770-1774`), but the env-missing arm is unfiltered: a mid-run `set_colony_env` that drops a body (or a restore that re-installs env without one) leaves a live colony on that body — the first impact in radius then makes `step_frame` return `Err(Recoverable)` forever, a permanent wedge that also violates the "`Err` escapes only for `Fatal`" contract.
- **Retroactive damage.** `apply_damage_module` → `recompute_colony` runs at `now` (range start) while the impact physically occurred mid-range. The damaged module is treated as inert for the *whole* `(now, t_next]` interval — a store-bound event inside the range is dropped and republished under post-damage rates, and production between `now` and the impact tick is retroactively removed. Deterministic, but the model quietly rewinds up to a full interval per impact.

Impact
First half: livelock-class failure reachable only via construction-config mutation mid-run — low probability, high consequence when it bites. Second half: bounded state error per impact, partition-consistent.

Suggestion
Filter `!colony_env.contains_key(&colony.body)` alongside the dormant filter (or downgrade `recompute_colony`'s env miss to a skip for the event path); if rewind-vs-land matters, anchor damage at the impact's recorded tick — that would require per-tick impact stamps, so documenting the range-start application as the model's quantization is the cheaper honest fix.

---

### [LOW] Finding 9: A damaged `Store`/`Battery` keeps contributing capacity — "stores nothing" is only half-enforced
File: `crates/csp-sim/src/colonies.rs:237-239` (doc claim), `:1472-1499` (one-time capacity aggregation at founding), `:481-490` (`module_inert`)

Problem
`ColonyModule`'s doc says a damaged module "produces, consumes, and stores nothing". `module_inert` enforces the produce/consume halves, but `stores[resource].capacity` is the Σ of module capacities computed **once at founding** and never re-derived: a damaged `Battery` keeps holding charge and a damaged `Store` keeps holding units (and keeps salvaging them at `AbandonColony`). Notably the thermal solve does this right — `eps_sigma_a` filters to `active` modules (`:951-960`), so a damaged `Radiator` correctly stops radiating while a damaged `Battery` incorrectly keeps storing.

Impact
Deterministic; a modest capacity windfall after damage (and it makes `ImpactDamage` — the terminal degrade reason — even more toothless against storage-heavy colonies).

Suggestion
Recompute `capacity` per re-solve over undamaged `Store`/`Battery` modules (a store's `capacity` is derived data anyway — the anchor is the state), or drop the "stores nothing" clause from the damage contract to match the implementation.

---

## Checked and clean (for the record)

- **Fixpoint ordering**: `grounded_modules` least-fixpoint is unique/order-independent; grant solve is monotone-decreasing with `min(prev, computed)` under a hard `n+1` cap — deterministic even non-converged.
- **Partition independence**: `earliest_store_bound` is anchor-relative (`colonies.rs:1067-1096`) — bitwise-identical bound for every query tick in `[anchor, bound)`; `level_at` is a pure linear eval.
- **Notification dedup**: per-kind heap scan prevents the double-schedule the restore-pairing walk would reject; `inspection_tick` consumption (`:2080`) closes the leaked-deadline bug the comment describes.
- **Dormant/Abandoned**: stores re-anchored + rates zeroed at the transition (no phantom salvage), all events dropped, publish skips them, `level_at` frozen — verified against `dormant_freezes_stores`.
- **Toggle**: `SetLifeSupportEnabled` scrubs all three LS kinds on both scopes, firing arms re-check the flag, re-solve applies the crew=0 gate in both directions.
- **Manufacture atomicity**: all affordability gates precede `shadow_economy`; BOM merge, cap reservation (`vessels + builds < max_vessels`), id-capture under `shadow_builds`; `fire_construction_complete` is total (only `Fatal` paths inside, which roll the event back whole).
- **Vessel LS check**: ascending-`VesselId` debit, per-resource `rate·period·crew`, whole firing atomic under one `ev_shadow`, publish repairs a dropped check.
- Event heap ordering `(tick, seq)`, `EventKind`/`class_of` tables, `hash_*` walks: all deterministic.
