# Deep Review — Cluster C10 (progression layer)

Files: `crates/csp-sim/src/{research,tech,economy,contracts,reliability}.rs`
Focus: determinism + logic (micros/f64 boundary, RNG stream keys, event-driven completion, renewal counters, soft-cap integral, qualification, offer determinism).

All five files were read in full, plus the cross-cutting call sites in `lib.rs` (`step_frame`, `fire_due_events`, `remove_vessel`, `apply_recover_vessel` callers, restore gates), `csp-core/src/rng.rs`, `csp-phys/src/resolve.rs`, and `joints.rs`.

Determinism verdict overall: the epoch-anchor / event-driven structure is sound. `to_micros` is a fixed total function (finite positive → `round`, else 0); all ledger arithmetic is integer. The `{CONTRACTS, 0, offer_index}` KDF keys correctly implement anti-reroll (counter never reused; `None`-eligible indices aren't consumed). `{RELIABILITY, instance, k}` renewal counters are consistent between `apply_failure_at` and `apply_mode_to_record` (ordinal `k`, not dedup'd `failures.len()`), mode draws live at `MODE_COUNTER_BIT | k` in the same stream, ignition draws on the separate `{IGNITION}` tag. Publish steps (`publish_economy`, `publish_contracts`, `publish_reliability`) are pure functions of stamped state evaluated at event boundaries — partition-independent. The soft-cap `C' = C + (cap−C)(1−exp(−k·u/cap))` integral is correct and reproduces the quoted 63/86/95% sequence. No determinism violations found.

Findings (logic):

---

### [MEDIUM] Finding 1: `RecoverVessel` silently destroys live research projects — accrued science AND escrowed data lost, no banking
**File:** `crates/csp-sim/src/economy.rs:806–921` (the `remove_vessel` call at :911), `crates/csp-sim/src/lib.rs:3801–3819` (`remove_vessel` → `research::drop_vessel_projects`)

**Problem:** `apply_recover_vessel` banks *pooled* science records per key at the §3.2 recover fraction, then calls `remove_vessel`, which runs `drop_vessel_projects` — a path documented and designed for vessel *destruction* ("no accrual — the lab burned, work-in-progress is lost"). Recovery is not destruction: the lab comes home intact. Yet an in-flight project's (a) accrued-but-unbanked science and (b) entire escrowed `data_start` vanish silently.

**Evidence:** `economy.rs:856–880` — `science_bank` iterates only `v.science.data` (the pool); project escrow lives on `ResearchProject::data_start`, never visited. `economy.rs:873–877` — `closable` explicitly detects a live project on the key (`self.science.projects.values().any(|p| p.key == *key)`) and declines to close it, so the code *knows* a project exists and proceeds anyway. `lib.rs:3813` — `drop_vessel_projects` drops the records and scrubs the pending event pair with zero accrual.

Compare the sibling interruption paths, which all bank accrual and return the remnant: jettison (`lib.rs:5609` → `finalize_projects_on_nodes` + `credit_science_payout` at :5612), split (`lib.rs:5793–5802`), `StopResearch` (`research.rs:1043–1089`).

**Impact:** Real loss for the player: recovering a vessel with a 99%-complete lab project destroys more science than never running the project would have preserved — asymmetric with every other interruption path and with `RecoverVessel`'s own pooled-data banking. A key with pooled units at `closable == false` also gets its pool banked while the escrowed remainder is destroyed — a half-accounted key.

**Suggestion:** In `apply_recover_vessel`, finalize the vessel's projects before `remove_vessel` (e.g. `finalize_projects_on_nodes(&all_nodes, tick)` — or a per-vessel finalize) so accrued science banks and the payout credits like the jettison path; either bank the returned remnant into the pooled pass at `fraction` (it physically came home) or at minimum document the loss in the `apply_recover_vessel` doc.

---

### [MEDIUM] Finding 2: `SurveyData` predicate counts `collected` (cumulative gathered), not returned data — completes on data never returned
**File:** `crates/csp-sim/src/contracts.rs:744–757`

**Problem:** The comment says "Cumulative *returned* data at matching keys — transmitted or recovered (closed)" but the code sums `s.collected` — the soft-cap ledger of *all units ever collected* at the key — for any key whose `transmitted || closed` flag is set. `collected` includes units that were never returned: units still pooled aboard a vessel, units escrowed inside a live research project, units destroyed with a vessel, and — most importantly — units collected *after* the flag was set (a key transmits, then more data is gathered at it and never cashed out).

**Evidence:**
```rust
let units: f64 = self.science.keys.iter()
    .filter(|(k, s)| k.experiment == *experiment && k.body == *body
        && (s.transmitted || s.closed))
    .map(|(_, s)| s.collected)
    .sum();
units >= *min_units
```
Scenario: collect 10 at key K, transmit (pool drains, `transmitted=true`), collect 9 more (pooled aboard, never returned) → predicate counts 19 though only 10 data units ever returned. Or: K transmits once, then a vessel carrying 20 more pooled units at K is destroyed — still counts 30.

**Impact:** A survey contract can complete while a large fraction of the "counted" data was never returned — materially early completion vs. the documented `min_units` of returned survey data.

**Suggestion:** Track a `returned_units: f64` accumulator on `ScienceKeyState`, incremented by `units` at `TransmitData` and `RecoverData` (and the `apply_recover_vessel` pooled banking), and read that in the predicate.

---

### [LOW] Finding 3: `apply_recover_data` omits the `transmit_ratio` band check that `apply_transmit_data` enforces
**File:** `crates/csp-sim/src/research.rs:881–885` vs `:782–787`

**Problem:** `apply_transmit_data` rejects a def whose `transmit_ratio` is not in `(0, 1]` (`"transmit: def transmit_ratio out of band"`). `apply_recover_data` computes `(1.0 - def.transmit_ratio).max(0.0)` with no validation: an authored ratio > 1 silently yields `fraction = 0` — banks nothing and *closes the key* (the data is destroyed); a negative ratio yields `fraction > 1` — banks more than the pooled units.

**Evidence:** `research.rs:881–884`: `let fraction = if prior.transmitted { (1.0 - def.transmit_ratio).max(0.0) } else { 1.0 };` — reached after the key transmitted legitimately, so a corrupt ratio is reachable even without a crafted snapshot (defs are construction data, and the transmit gate only runs on the transmit path).

**Impact:** Authored-data validation gap with silent data loss (key closed at zero payout). Same concern in `apply_recover_vessel`'s `map_or(0.0, |d| (1.0 - d.transmit_ratio).max(0.0))` at `economy.rs:869`.

**Suggestion:** Apply the same `(0, 1]` band check in both recovery paths.

---

### [LOW] Finding 4: `track_levels` `u8` overflows at authored level 256 — panic (debug) / silent track reset (release)
**File:** `crates/csp-sim/src/tech.rs:265–296`

**Problem:** `track_levels` values are `u8`. `apply_research_upgrade_level` guards on `usize::from(current) >= def.levels.len()` — with `levels.len() > 255` and a `level_cap` permitting, `current` can reach 255, then `current + 1` overflows: panic under debug, wrap to `0` under release (track silently reset to level 0, making level-0's cost re-buyable).

**Evidence:** `tech.rs:295`: `self.tech.track_levels.insert(track.clone(), current + 1);` — `current: u8` from `track_level` at :76–78.

**Impact:** Data-dependent latent bug — requires >255 authored levels (unlikely but not gated anywhere; `level_cap`/`levels.len()` take `u8`/`usize` types that admit it).

**Suggestion:** Guard `current == u8::MAX` (or store levels as `u16`), or clamp `levels.len() <= u8::MAX as usize` at tree load.

---

### [LOW] Finding 5: No cross-project researcher uniqueness — one crew member can staff N concurrent projects
**File:** `crates/csp-sim/src/research.rs:944–979`

**Problem:** `apply_start_research` validates researchers are aboard *this* vessel, dedup'd *within* the assignment list, skill ≤ 5, and within lab slots — but never checks whether the same `CrewId` is already assigned to another live project (a second lab on the vessel, or a lab on another vessel the crew is not aboard — the aboard check covers only this vessel). One crew member can thus multiply the program's `R` across every lab.

**Evidence:** the per-lab dedup `self.science.projects.values().any(|p| p.vessel == vessel && p.lab_node == lab_node)` (:970–979) keys on `(vessel, lab_node)`, never on `researchers`. The restore gate (`lib.rs:7708–7716`) repeats the same per-project-only checks.

**Impact:** Physics-of-crew loophole — a scientist works two labs full-time. Mitigated by the documented v1 simplification (skill rides the command; the crew arena is a later sub-phase), but the doc's interruption rules don't name this gap.

**Suggestion:** Reject `StartResearch` when any named `r.crew` already appears in another live project's `researchers` — a one-line `projects.values().flat_map(|p| &p.researchers)` check; or document as deferred to the crew arena.

---

### [LOW] Finding 6: Fatal-mode `PartFailure` firing returns early *without* clearing `pending_failure_tick` — permanently wedges the record's dormant chain
**File:** `crates/csp-sim/src/reliability.rs:917–933`

**Problem:** `fire_part_failure` early-returns `Ok` on `mode.is_fatal()` — but the record's `pending_failure_tick` remains `Some(_)`, and `publish_reliability` dedups on `pending_failure_tick.is_some()` (`:851`), so the record can never publish a dormant crossing again — dormant Λ accrues forever against an event that can never re-arm. Every other early path is safe (missing vessel/record → flag irrelevant; InBubble → flag already cleared on transition), and the intended stale path *does* clear it (`:954`). The doc comment calls this "a stale no-op" but it is not a clean no-op: it kills the renewal chain.

**Evidence:** `:931–933` `if mode.is_fatal() { return Ok(()); }` precedes the pending-flag clear at `:954`/`:998`.

**Impact:** Crafted-state-only reachability (the publish draws `dormant_modes`, the non-fatal subset; the restore gate doesn't validate the mode field against the dormant class). Marginal, but it is exactly the wedge `apply_failure_at`'s comment warns about ("a stale `Some` would wedge the renewal chain").

**Suggestion:** Clear `rec.pending_failure_tick` before the fatal-mode early return (or fold the fatal check into the stale-fire branch so the publish slot re-derives).

---

### [LOW] Finding 7: `restore_reliability_state` dedup key is `(vessel, instance)` — cross-vessel `PartInstanceId` duplication escapes the "unique across the save" gate
**File:** `crates/csp-sim/src/lib.rs:8041–8060` (adjacent restore gate for `reliability.rs`)

**Problem:** The gate comment requires "every part instance is unique across the save," but `instances.insert((vid, rec.instance))` only catches duplicates *within one vessel's* table. Two different vessels carrying records with the same `PartInstanceId` pass the check (and the `< next_part_instance_id` bound).

**Evidence:** `lib.rs:8049`: `if !instances.insert((vid, rec.instance)) { Fatal }` — `instances: BTreeSet<(VesselId, PartInstanceId)>`.

**Impact:** Crafted snapshot can clone an instance across vessels — both records share the `{RELIABILITY, instance, k}` / `{IGNITION, instance, k}` draw streams (identical θ₀ and renewal series — duplicated "lucky" thresholds). Determinism itself is not broken, but the anti-save-scum uniqueness invariant the gate documents is not enforced.

**Suggestion:** Key the dedup set by `rec.instance` alone.

---

### [LOW] Finding 8: A zero-salary `Active` roster produces a perpetual `SalaryDue` event chain — contradicts the module doc's "no perpetual no-op event" intent
**File:** `crates/csp-sim/src/economy.rs:673–731`

**Problem:** `publish_economy` gates publishing on `crew.values().any(|c| c.status == Active)` — and `fire_salary_due` re-arms on the same predicate. A roster of `Active` members all at `salary_per_s == 0` produces `due = 0` → `to_micros(0) = 0` → `charge_funds` early-returns `Ok(0)` — a Silent event re-armed every `salary_period_ticks` forever, while the module doc's stated rule is "a program with no payroll has no salary to drain and gets no perpetual no-op event."

**Evidence:** `economy.rs:676–683` (publish gate), `:719–730` (re-arm gate) — both test status, not nonzero payroll.

**Impact:** Perpetual no-op event churn (one Silent event per 30 sim-days per save, forever) — deterministic but contradicts the documented publish rule and keeps an otherwise-empty heap occupied.

**Suggestion:** Gate on `any(|c| c.status == Active && c.salary_per_s > 0.0)`.

---

### [LOW] Finding 9: Orbit-regime contract predicates only see `OnRails` — an `InBubble` vessel in a matching orbit cannot satisfy `SatellitePlacement` / `PartTest::Orbit`
**File:** `crates/csp-sim/src/contracts.rs:724–738, 764–772`

**Problem:** `objective_met` reads `v.orbit()`, which is `Some` only for `VesselState::OnRails` (`lib.rs:367–371`). A physics-resident (`InBubble`) vessel holding a matching orbit — e.g., a station kept bubble-resident for a long stretch — evaluates `false` until it returns to rails, and nothing `InBubble`-side fires `OrbitAchieved`. The `DataBanked`/`VesselRecovered` triggers evaluate all contracts on *any* trigger, so a single unrelated event can rescue it — but a long-quiet bubble residence silently starves the predicate.

**Evidence:** `contracts.rs:728` `let Some(o) = v.orbit() else { return false };` — same at `:765` for `PartTest::Orbit`.

**Impact:** Edge-case completion delay (contract may expire unfulfilled while the vessel is physically in the required orbit); arguably intended ("a stable stamped conic is the contract's orbit"), but it's an undocumented residence asymmetry next to the transition-trigger list the module doc gives.

**Suggestion:** Either document the OnRails requirement in the contract params doc, or let the predicate read the bubble body's derived elements when `InBubble`.

---

## Verified-clean areas (no findings)

- `to_micros`: total, deterministic (`round` on fixed product, `[0, u64::MAX]` clamp); all downstream funds math is integer `u64` (module doc's claim holds).
- Soft-cap integral: `room * (-expm1(-k·u/cap))` bounded `< room`; `collected` asymptotically approaches `cap`; test pins the §3.2 sequence exactly.
- `data_at`/`exhaust_seconds`/`exhaust_tick`: two-regime closed form correct; ceil-to-tick is conservative (event lands at-or-past true ε crossing); `fire_research_exhausted` banks `η·D0` as documented.
- RNG stream keys: `{RELIABILITY, instance, k}` ordinal renewal (not `failures.len()` — repeat modes correctly advance draws); `MODE_COUNTER_BIT` separates mode draws; `{IGNITION}` separate tag; `{CONTRACTS, 0, offer_index}` — unconsumed indices on `None` redraw identically when eligibility changes.
- `contract_notify` event-driven completion: evaluation at boundaries only; fact stamping before predicate evaluation; deadline pairing strict both directions in the restore gate.
- `fire_salary_due` re-arm on absolute grid (`event_tick + period`); lazy publish is partition-independent.
- `drop_vessel_projects` / `finalize_projects_on_nodes` / `split_vessel_science` / `merge_vessel_science`: shadow-before-mutate ordering correct; event-pair scrub correct; remnant routing by node correct.
- `bubble_part_tick`: per-tick epoch advance (`epoch = tick + 1`) consistent with lazy `hazard_at`; dormant-class crossings draw the non-fatal subset; Λ consumption at crossing (`= theta`) matches both failure paths; one-tick `thrust_factor` lag is consistent.
- Hash walks: all `BTree*` iteration is canonical; ledger/notifications correctly excluded as recorded history; `renewal_count` appended last per append-only schema.
