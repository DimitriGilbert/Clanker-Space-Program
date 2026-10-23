# Verified Report — Cluster C10: program layer (research / tech / economy / contracts / reliability)

**Source reviewed:** `crates/csp-sim/src/{research,tech,economy,contracts,reliability}.rs`, cross-checked against `crates/csp-sim/src/lib.rs` (apply_jettison 5496–5617, event dispatch 6530–6645, restore gates 2370–2654 / 7743–7797 / 8033–8085, `event_names_vessel` 7447–7460), `crates/csp-sim/src/joints.rs` (:955), `crates/csp-phys/src/graph.rs` (:27), and `crates/csp-data/src/validate.rs` (`validate_contract_space` 2871–2997, `validate_contracts` 3020–3133).

**Note on paths:** the report's `csp-sim/src/…` paths resolve to `crates/csp-sim/src/…`. All line numbers below verified against current source.

---

## Verdicts

### Finding 1 (HIGH): `finalize_projects_on_nodes` not vessel-scoped — **CONFIRMED**

- `research.rs:1147–1153`: the filter is exactly `nodes.contains(&p.lab_node)` — no `p.vessel` check. `projects` is a global `BTreeMap<ResearchId, ResearchProject>`; `lab_node` is a `csp_phys::PartNodeId` (`graph.rs:27`, `pub struct PartNodeId(pub u64)`), an id into *that vessel's own* `craft.graph.nodes` — per-craft, so every vessel's graph can (and commonly does) contain `PartNodeId(0)`.
- Sole caller is `apply_jettison_part` (`lib.rs:5604–5609`): `departing` = `{node}` taken from the jettisoning vessel's graph, passed with no `VesselId`; `let (_, banked) = …` drops the remnant `Vec`. `credit_science_payout(banked)` at :5612 banks/credits the victim's accrual (numerically consistent — `science.banked` is program-global — but wrong-world).
- Event-pair scrub at `research.rs:1165–1176` removes `ResearchDataExhausted`/`ResearchProjectComplete` keyed by *project id*, so the victim vessel's pending pair is scrubbed too.
- The sibling functions all carry the vessel scope the report says is missing: `split_vessel_science` `p.vessel == vessel.id && frag_nodes.contains(&p.lab_node)` (research.rs:467); `merge_vessel_science` re-seats only `p.vessel == secondary.id` projects and remaps `lab_node` through `map` (research.rs:547–552 — itself proof the ids are per-graph); `drop_vessel_projects` filters `p.vessel == vessel` (research.rs:570,575).
- Jettison is `InBubble`-gated (lib.rs:5505), but the *victim* project can sit on a vessel in any state — no gate prevents two vessels from holding projects on numerically equal node ids.
- **Net effect (verified):** jettisoning node N on vessel A finalizes every project on *any* vessel whose `lab_node == N`: accrual banks globally (payout credited), escrowed remnant silently dropped, scheduled event pair scrubbed. Silent cross-vessel project destruction. The one-line fix (pass `VesselId`, add `p.vessel == vessel` to the filter) is correct and mirrors `split_vessel_science`. Severity HIGH is appropriate.

### Finding 2 (MEDIUM): `fire_part_failure` fatal-mode early return leaves `pending_failure_tick` set — **CONFIRMED**

- `reliability.rs:931–933`: `if mode.is_fatal() { return Ok(()); }` — fires *after* the record lookup (:923–930) and returns without clearing `rec.pending_failure_tick`. Confirmed the flag is **not** cleared on this path.
- Wedge half confirmed: `publish_reliability` skips `rec.pending_failure_tick.is_some()` records (:851–853), and the consumed event is gone, so no live producer remains — the part stops getting dormant `PartFailure` publishes. Precision note: "permanent" = permanent *while dormant* — the flag is still cleared by a bubble-entry residence change (`reliability_residence_change`, reliability.rs:784), fragment split (lib.rs:8771), or latch merge (joints.rs:955). If none of those happens before the next snapshot, the second consequence lands:
- Time-bomb half confirmed: `restore_reliability_state` enforces `pending_failure_tick = Some(t)` ⇒ `pf_events.get(&(vid, instance)) == Some(&t)` else `Fatal` (lib.rs:8052–8058). After the fatal-mode firing consumes the event, any subsequent snapshot+restore fatals on "pending failure tick / event mismatch".
- Reachability confirmed as crafted-snapshot class: the only `PartFailure` producer is `publish_reliability` (:884), which draws `dormant_modes` — `spec.failure_modes.iter().filter(|(m,_)| !m.is_fatal())` (reliability.rs:510–516). Restore validates PartFailure events for vessel existence, `(vessel,part)` uniqueness, and tick pairing (lib.rs:2410, 2542–2548) but **does not check `mode` is non-fatal or in the spec's mode table** — so a snapshot pairing a fatal-mode `PartFailure` with `pending_failure_tick` restores cleanly, trips the wedge on firing, and fatals on the next restore. The report's "crafted/restored event" framing is accurate.
- The report's claim that the *other* early returns are harmless checks out: missing vessel (:917) and missing instance (:929) mean the record (which owns the flag) is gone; `InBubble` (:920) implies the flag was cleared on bubble entry (:784) — and `event_names_vessel` includes `PartFailure` (lib.rs:7460), so pending events are scrubbed on vessel-named transitions, consistent with the doc at reliability.rs:739–741.
- Severity MEDIUM holds; the suggested fix (clear `rec.pending_failure_tick` before the fatal-mode return, matching the stale-fire branch at :954) is correct — the record accessor is already in scope by :923–930.

### Finding 3 (MEDIUM): `RecoverData`/vessel-recovery applies `1 − ratio` to post-transmit units — **CONFIRMED (mechanics); severity is a design judgment**

- `apply_recover_data` (research.rs:846, 881–897): `units` = *all* currently pooled units at the key; `fraction = (1.0 − def.transmit_ratio).max(0.0)` iff `prior.transmitted`; `banked = units * fraction`. Exactly as described.
- `apply_transmit_data` drains the key from every node pool on the vessel (:803–806) and sets `transmitted = true` (:813), so any `units > 0` at a transmitted key is necessarily post-transmit collection.
- `apply_run_experiment` refuses only `closed` keys (:717–718), never `transmitted` — post-transmit re-collection is legal and accumulates `collected` + pooled units normally.
- `apply_recover_vessel` (economy.rs:856–878) uses identical `fraction` semantics per key — confirmed.
- Worked example holds: collect 100 → transmit (bank 60 at ratio 0.6) → collect 50 → recover banks `50·0.4 = 20` vs. the 50 a never-transmitted key would bank; the key then closes.
- Caveat for the parent: whether this is a *bug* hinges on the §3.2 reading. The code is internally consistent and deterministic, and the function doc ("or the `1 − ratio` remainder for a previously transmitted key", research.rs:822–823) literally describes the implemented blanket discount — but does not disclose that it also discounts units collected *after* the transmit. The report's request — either track the untransmitted remainder or document the per-key blanket semantics on `ScienceKeyState::transmitted` — is the right framing; the mechanical facts are fully confirmed.

### Finding 4 (LOW): `SurveyData` sums `collected`, not returned units — **CONFIRMED**

- `contracts.rs:739–757`: sums `s.collected` over keys matching `(experiment, body)` with `s.transmitted || s.closed`; the inline comment even says "Cumulative *returned* data" (:744–745) while summing `collected`.
- `collected` increments at collection time in `apply_run_experiment` (research.rs:737) — before any transmit/recover — and is never decremented. So it counts units still pooled aboard (a transmitted key can hold post-transmit re-collection — see Finding 3), units escrowed in live lab projects, and units later lost to jettison/vessel destruction — all at keys that were merely ever transmitted or closed.
- The report's example is reachable: transmit 1 unit at a key with `collected = 100` ⇒ `transmitted = true` ⇒ `min_units: 100` satisfied with 99 units never returned. Contracts complete early; `failed`/`completed` counters and reward math unaffected. LOW severity is fair.

### Finding 5 (LOW): already-satisfied contracts complete at `AcceptContract` — **CONFIRMED, and slightly understated**

- `apply_accept_contract` credits the advance (:698–702) then runs `objective_met(&params, None)` (:705) → `complete_contract` credits `completion_funds` (:876–899). `SatelliteOrbit` (:724–738) and `PartTest` (:759–778) scan `self.vessels.values()` excluding only `debris_since`; `SurveyData` reads the cumulative ledger; `Rescue` correctly returns `false` without its `VesselRecovered` trigger (:779–781).
- Extension the report missed: **`ColonyResupply` is in the same class** — its predicate sums `facts.colony_deliveries` accumulated *before* acceptance (:793–806), so pre-existing deliveries also auto-complete a fresh offer.
- The behavior is deliberate and documented ("an already-satisfied contract completes at acceptance", :658–660, :703–704; `bodies_visited` stamped before predicate eval at :826–848), so it is design — but the economic consequence (advance + completion for zero work, repeatable every `publish_contracts` refill at :640–649) is real. CONFIRMED as described; verdict: documented-design-with-exploit-shape, LOW.

### Finding 6 (LOW): empty authored spaces → `SimError::Fatal` at publish — **CONFIRMED, with a correction that makes it *more* reachable**

- Sim side confirmed exactly: `pick(rng, 0)` = `(next01 * 0.0) as usize` = `0` (contracts.rs:223–225); every space does `.get(pick(rng, xs.len())).ok_or(SimError::Fatal(…))` (contracts.rs:239–319); `generate_offer`'s eligibility filter checks only `weight > 0.0 && prereqs_met` (:593–597); `publish_contracts` propagates with `?` inside the refill loop (:640–649). One poisoned template ⇒ Fatal every publish.
- Correction to the report's "(apparently) at load": `csp-data::validate_contracts` **does** gate empty `experiments` (`validate.rs:2932`), `modules` (:2949), and `resources` (:2985), plus `weight`, rewards, and `deadline_days` (:3066–3086).
- **But `bodies` is *not* gated:** `bodies_ok` (validate.rs:2885–2894) computes `ok = !bodies.is_empty()` into its return value, pushes **no** error for the empty case, and all six call sites discard the bool (`bodies_ok(bodies, out);`). An empty `bodies` list in any space kind (SatellitePlacement, SurveyRun, PartTest's `RegimeSpace`, Rescue, ColonyResupply) passes validation green → Fatal at the first offer roll. Prior session already reached the same conclusion (`verified-report-03.md:21`: "a template authored with `bodies: []` passes `validate_contracts` … returns `SimError::Fatal` from `draw_params`").
- Net: CONFIRMED — narrower ungated surface than the report implies (bodies lists only, via a separate `bodies_ok` discarded-return bug), but reachable through the *normal validated-data* path, not just unvalidated `set_contracts` callers.

### Finding 7 (LOW): minor edge defects — **all five bullets CONFIRMED**

- `economy.rs:691`: `let next = (self.clock.ticks() / period + 1) * period;` — unchecked add+mul; overflows (panic in debug) at `clock` near `u64::MAX`. Unreachable in practice. Confirmed.
- `research.rs:357`: `p.start_tick.saturating_add(ticks).max(p.start_tick + 1)` — the `p.start_tick + 1` operand is a plain `+`; panics at `start_tick == u64::MAX`. Confirmed (the report's phrasing is precise: `.max(...)` doesn't protect the operand's own overflow).
- `contracts.rs:918–919`: `fire_contract_deadline` returns `Recoverable` on `deadline_tick != Some(event_tick)` — confirmed; inconsistent severity vs. sibling firings, unreachable in legit play (events only fire at their scheduled tick).
- `contracts.rs:676–680`: `(d * DAY_S * TICK_HZ).ceil().max(1.0) as u64` — `NaN → 1` tick (f64::max drops NaN), `+inf → u64::MAX` (saturating cast), then `saturating_add`. Confirmed mechanically; mitigating note: `deadline_days` *is* validated `finite > 0` at load (validate.rs:3082–3086), so the authored path is gated — only unvalidated `set_contracts` input reaches it.
- `reliability.rs:1144–1149`: `(units * cfg.bench_seconds_per_unit * TICK_HZ).ceil() as u64` saturates to `u64::MAX` → `checked_add` → `Fatal("bench test: tick overflow")`. Confirmed.

## Spot-checks of the "found clean" section (sampled, all hold)

- `restore_economy_state` insolvent gate `insolvent && funds_micros > 0 → Fatal` — lib.rs:7745–7746. Holds.
- Vessel-scoped project handling in `split_vessel_science`/`merge_vessel_science`/`drop_vessel_projects` — research.rs:467, 547–552, 570–575. Holds (this is exactly what Finding 1 is missing).
- `pending_failure_tick` lifecycle — set only at publish (:881), cleared at apply (:998), stale-fire (:954), bubble entry (:784), fragment split (lib.rs:8771), latch merge (joints.rs:955); restore pairing both directions (:8052–8058 + event-side uniqueness :2542–2548). Holds, modulo Finding 2's fatal-mode hole.
- `dormant_failure_tick` closed form `epoch + ceil(remain/λ·TICK_HZ)` with `remain ≤ 0 → epoch+1` — reliability.rs:522–534. Holds.
- Deadline/complete pairing — `complete_contract` drops pending `ContractDeadline` (contracts.rs:891–898); firing re-verifies tick (:918). Holds.
- `accept` prereq re-check + fact stamping before predicate eval — contracts.rs:672–674, 826–848. Holds.
- `apply_recover_data` closed-key invariant gates (other-vessel pools, live projects) — research.rs:867–880; mirrors `apply_recover_vessel`'s `closable` (economy.rs:873–877). Holds.

## Residual risks / notes for the parent

1. **Finding 6's correct fix likely needs two changes:** gate empty candidate lists in `generate_offer`/`draw_params` (Recoverable or skip) *and* fix `bodies_ok` in `csp-data/src/validate.rs:2885–2894` to push a `ValidationError` for empty lists (its `ok` return is dead code). The validator half duplicates the already-verified verified-report-03 finding — dedupe recommended.
2. Finding 3 is a semantic/design decision, not a mechanical defect — if the blanket `1 − ratio` on post-transmit units is intended, it needs a doc note on `ScienceKeyState::transmitted`; if not, it needs a per-key remainder bucket. The report's suggested fixes are both viable.
3. Finding 2's wedge is "permanent while dormant" rather than strictly permanent (residence change/split/merge clear the flag), but the next-snapshot `Fatal` makes the distinction moot for the crafted-input path. One-line fix is safe.
4. Finding 5 extends to `ColonyResupply` (pre-acceptance deliveries count, contracts.rs:793–806) — same instant-completion channel the report describes for the state-reading kinds.
