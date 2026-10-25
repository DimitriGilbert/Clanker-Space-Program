# Ruthless Review — Cluster 6: Colony Simulation + Contract System

Reviewer: Cluster 6 agent (static review only, no cargo runs)
Files: crates/csp-sim/src/colonies.rs, crates/csp-sim/src/contracts.rs
Date: 2026-10-25

Findings appended incrementally below as confirmed.


### [SEVERITY: MEDIUM] Finding 1: One poisoned template permanently wedges every future `step_frame`
**File**: crates/csp-sim/src/contracts.rs:705-723
**Problem**: `publish_contracts` propagates a `Recoverable` draw error out of the interval advance, but `offer_index` is only advanced *after* a successful draw — so the deterministic retry hits the identical error forever.
**Evidence**: 
```rust
while self.contracts.offers.len() < self.contract_cfg.board_size as usize {
    let index = self.contracts.offer_index;
    let Some(offer) = self.generate_offer(index)? else { break; };   // `?` returns BEFORE the increment
    self.contracts.offer_index = index.checked_add(1)...;
```
`generate_offer` is a pure function of `(defs, facts, offer_index, seed)`: a template whose space draws dishonest params (empty `bodies`/`resources` list, NaN band — reachable via `set_contracts`, which bypasses the data validators) errors at the same index on every call. `advance_interval_subsystems` (lib.rs:6694 `self.publish_contracts(shadow)?`) propagates the `Recoverable`, `step_frame` rolls back and returns `Err` (verified at lib.rs:7565-7572), and the board can never reach `board_size` — every subsequent `step_frame` on this world returns the same error. The sim is permanently immobile.
**Impact**: A single bad template in an unvalidated install bricks the whole world (no tick ever advances again) — while the module doc (contracts.rs:26-31) sells this as "the board publish degrades `Recoverable` ... the world rolls back intact", implying continued operation. The `advance_interval_subsystems` signature already has a `degraded: &mut Vec<SimError>` channel (used by `advance_orbits`) that this path ignores.
**Suggestion**: On `Err` from `generate_offer`, push the error onto `degraded`, still advance `offer_index` (skip the poisoned draw), and continue the refill loop (bounded skips: stop after `board_size` consecutive failures so an all-poisoned install degrades to a short board instead of a spin). Keep the `?` only for `Fatal`.

### [SEVERITY: MEDIUM] Finding 2: Instant-completion faucet is unbounded — "once per offer index" is infinite indexes
**File**: crates/csp-sim/src/contracts.rs:812-816 (mechanism), 734-749 (doc)
**Problem**: `apply_accept_contract` evaluates `objective_met(params, None)` over *cumulative, pre-acceptance* state and immediately pays advance + completion. The board then refills, minting fresh offer indexes, each of which can repeat the cycle.
**Evidence**:
```rust
if self.objective_met(&self.contracts.active[&id].params, None) {
    self.complete_contract(id, shadow)?;
}
```
`ColonyResupply` reads the cumulative `facts.colony_deliveries` (contracts.rs:884-899), `SurveyData` reads cumulative returned units, `SatelliteOrbit`/`PartTest` read *any current vessel*. Once 500 ore has been delivered (or a qualifying vessel exists), every subsequently generated offer with `amount <= 500` auto-completes at accept. Loop: accept → advance+completion credit → `publish_contracts` refills → accept next. There is also no cap on `contracts.active` anywhere (checked `apply_accept_contract` and lib.rs dispatch). The doc's cost statement — "a farmed board can pay advance+completion for no new work, **once per offer index**" — understates it: `offer_index` grows without bound, so the faucet is unlimited funds for one command per payout, forever.
**Impact**: Economy-breaking, trivially exploitable, and it defeats the `ContractsCompleted`/`CrewLostAtMost` prerequisite economy (failed/paid counts inflate freely). Documented as intended v1 semantics, hence MEDIUM not HIGH — but the doc reads as if bounded when it is not.
**Suggestion**: Baseline the fact at acceptance (e.g. record `delivered_at_accept` / `returned_at_accept` per active contract and require the *delta since accept* to reach the ask), or restrict instant completion to a signed evaluation that excludes pre-accept state; optionally cap simultaneous active contracts.

### [SEVERITY: MEDIUM] Finding 3: Converter output into a resource with no store capacity is silently voided — inputs consumed, product destroyed, no signal
**File**: crates/csp-sim/src/colonies.rs:2277-2279 (drop site), 1581-1620 (store creation), 999-1018 (net rates)
**Problem**: `net_rates` is computed for every produced/consumed resource, but `recompute_colony` writes rates only into `colony.stores` entries — which exist solely for resources with a `Store`/`Battery` module (or a `def.stores` init row). A converter whose recipe outputs a resource with no store module runs at fraction 1.0, drains its inputs, and the output simply ceases to exist.
**Evidence**:
```rust
for (r, s) in colony.stores.iter_mut() {
    s.net_rate_per_s = solve.net_rates.get(r).copied().unwrap_or(0.0);
}
```
No store entry for `metal` → `net_rates["metal"] = +0.6` is dropped on the floor. Grounding and grants only inspect *input* requests, so nothing throttles the module. Neither the sim gate (`colony_def_fields_finite`, colonies.rs:1405-1441) nor the data validator (`validate_colonies`, csp-data/src/validate.rs:2064+) requires recipe outputs to have storage. There is no event, rejection, or fraction clamp — `module_fractions[i]` happily reads 1.0.
**Impact**: A player who builds a smelter without a metal tank silently destroys ore and EC while the UI reports the converter running at full rate. Resource destruction with zero feedback is the worst class of colony-game trap; the "flow network" reading (live coupling works via grants, only surplus lost) is defensible for *excess*, but here the *entire* output of a dedicated converter is lost.
**Suggestion**: Either reject at founding ("recipe output `X` has no store capacity") like the unknown-recipe rejection at colonies.rs:1428, or clamp the module's fraction by output headroom (production into zero capacity → fraction 0, like a starved input), or at minimum emit a `ColonyStoreFull`-class notification for output into zero capacity.

### [SEVERITY: LOW] Finding 4: Abandon salvage silently discarded when the site has no economy record
**File**: crates/csp-sim/src/colonies.rs:1684-1688
**Problem**: Salvage lands only `if let Some(st) = self.economy.sites.get_mut(&site)` — when the site record is absent the whole salvage payload vanishes with no error, no event, and the colony is still marked `Abandoned` (terminal, unrecoverable).
**Evidence**:
```rust
if let Some(st) = self.economy.sites.get_mut(&site) {
    for (r, u) in salvage {
        *st.inventory.entry(r).or_insert(0.0) += u;
    }
}
```
`set_sites` (economy.rs:648) creates `SiteState`s for installed defs, so the miss requires a mid-run `set_sites` swap that dropped the colony's site (or a crafted save) — a construction-config misuse, but the failure mode is silent destruction of the player's recovered resources on a one-way terminal command.
**Impact**: Player abandons a colony, gets nothing, no feedback. Low likelihood, high confusion.
**Suggestion**: Create the `SiteState` on demand (`entry(site).or_insert_with(...)`), or return `Recoverable("abandon: site has no inventory record")` before mutating anything.

### [SEVERITY: LOW] Finding 5: Founding store levels silently clamped; duplicate init rows silently overwrite
**File**: crates/csp-sim/src/colonies.rs:1609-1620
**Problem**: `store.anchor_level = s.level.min(store.capacity)` silently truncates an authored founding level that exceeds aggregate capacity, and a `def.stores` list naming the same resource twice keeps only the last row (assignment, not accumulation).
**Evidence**: validation only rejects non-finite/negative levels (`s.level.is_finite() && s.level >= 0.0`); a def asking 1000 units into a 100-unit store founds at 100 with no rejection, unlike every other founding defect which gets a typed `Recoverable`.
**Impact**: Authored starting inventories quietly disagree with the data; harmless in play but it is the one founding field whose violation is swallowed rather than refused, inconsistent with the module's own "reject, never silently inert" discipline (cf. the unknown-recipe rejection).
**Suggestion**: Reject `s.level > capacity` at founding like the other field gates, and reject (or sum) duplicate `def.stores` rows for one resource.

### [SEVERITY: LOW] Finding 6: `body_light` silently fabricates a fixed sun when the orbit model lacks the body
**File**: crates/csp-sim/src/colonies.rs:416-431
**Problem**: `orbits.body_orbit(body).ok()` swallows the lookup error and defaults to `(n = 0, L0 = 0)` — "the star itself" — so a colony whose body has a `colony_env` entry but no orbit-model record gets `synodic_rate = omega`, a sun that never moves in inertial space, and potentially zero terminators, with no diagnostic anywhere.
**Evidence**: `apply_found_colony` checks `colony_env` presence (colonies.rs:1555) but never cross-checks the orbit model; `ColonyBodyEnv.rotation_period_s: None` (tidally locked) plus the defaulted `n = 0` yields `synodic_rate = 0` — permanent `Daylight` at founding tick, no crossings ever, solar output constant forever.
**Impact**: A data-install inconsistency (env table and orbit table disagreeing about a body) produces a silently wrong power model rather than a founding rejection — the exact class of quiet divergence the workspace's determinism discipline tries to surface.
**Suggestion**: In `apply_found_colony`, reject when `body_orbit(body)` errors for a non-star body (or when `env.rotation_period_s.is_none()` but the body has no elements), mirroring the "no colony environment" rejection.

### [SEVERITY: LOW] Finding 7: `advance_funds`/`completion_funds` that are negative or NaN silently pay zero instead of rejecting
**File**: crates/csp-sim/src/contracts.rs:805-809, 1010-1014, 1061-1065; crates/csp-sim/src/economy.rs:97-102
**Problem**: `to_micros` maps any non-finite or non-positive value to `0`, so a template authored with `advance_funds: -50` (intended as a deposit?) or `NaN` (unvalidated `set_contracts` install) credits nothing and reports success — the accept completes, the deadline arms, and the player signed a contract whose stated economics never execute. Same for `completion_funds`/`failure_penalty_funds` at complete/expire.
**Evidence**: `pub fn to_micros(funds: f64) -> u64 { if !(funds.is_finite() && funds > 0.0) { return 0; } ... }` — contrast with the neighbouring `deadline_days` NaN gate (contracts.rs:772-775) which *does* reject non-finite values typed.
**Impact**: Silent economics divergence for poisoned installs only (data validators gate the RON path); inconsistency with the module's own deadline gate is the real smell.
**Suggestion**: Validate `rewards` finite/non-negative in `apply_accept_contract` next to the `deadline_days` check, one shared predicate.

---

## Quality verdict (honest, 5 lines)

1. Both files are far above average rigor for this codebase: pure solves, epoch-anchored stores, BTreeMap-everywhere determinism, libm-only transcendentals, firing-time predicate re-checks, and duplicate-notification guards are all genuinely done, with tests pinning each invariant.
2. No CRITICAL/HIGH correctness bug found in the colony loop: levels cannot go negative or NaN (clamped `[0, cap]`, all inputs validated finite), the grounding fixpoint kills self-feeding cycles, and dormant/abandoned freezing is correct.
3. The contracts module's weakest points are lifecycle-wide: an unbounded documented-but-understated instant-completion faucet and a permanent step_frame wedge from one poisoned template — both economy/lifecycle issues, not determinism issues.
4. The recurring theme is silent swallowing at config seams: voided converter output, dropped salvage, defaulted sun model, zeroed rewards — each individually gated behind misuse, collectively inconsistent with the "reject, never silently inert" discipline the code itself preaches.
5. Determinism discipline holds everywhere I could statically verify (fixed draw order, anchor-relative bounds, sorted iteration, no f32); no map-order, accumulation-order, or partition-independence violations found.

**Findings: 7 — CRITICAL: 0, HIGH: 0, MEDIUM: 3, LOW: 4.**
