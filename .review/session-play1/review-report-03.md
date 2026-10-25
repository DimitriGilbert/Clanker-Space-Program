# Review Report — P03: Propellant consumption (determinism-load-bearing)

**Commit reviewed:** `d8cd46f feat(sim+app): engines consume propellant, vessel on map, dead-stick fix` — the only cluster that can move the §7.1 state hash (HASH_SCHEMA_VERSION 19→20, `WeldPart::engine` in the craft walk at `crates/csp-sim/src/lib.rs:9500-9509`).

**Verdict:** the core design is sound — no free-impulse hole, no overdraw, deterministic ordering, idempotent re-scale, honest stalled burns. Found **1 Medium** logic issue (partial-throttle under-delivery in the depletion regime), **1 Medium** data-integrity issue (save boundary — the new `engine` field has no migration story), and 3 Low issues. Details below; the verified-correct checklist for every focus bullet is at the end.

---

## Findings

### F1 — MEDIUM (logic/fidelity): coverage is normalized to *full-throttle* demand but applied at every throttle — under-delivery + asymptotic depletion when `avail < Σneeds`

- **File:** `crates/csp-sim/src/lib.rs:7699-7813` (`propellant_plans`), `3558-3613` (bubble-loop debit), `7821-7838` (`apply_fuel_scales`); force application `crates/csp-phys/src/simple.rs:1315-1324`.
- **Problem:** pooled coverage `cov_r = min(1, avail_r / Σneeds_r)` is computed against **full-throttle** needs (L7755-7790) and folded into `rated_n` as `base·factor·cov` — a property of the tick, not of the commanded throttle. The actual throttle only enters afterward: force `= throttle·rated_n` (simple.rs:1319), debit `= u·scale·throttle` (lib.rs:3603-3612).
- **Evidence:** when `avail < Σneeds_full`, an engine at throttle `t` delivers `t·cov·rated`. But the tanks can sustain the *commanded* draw whenever `t·Σneeds ≤ avail` — the throttle-normalized coverage would be `min(1, avail/(t·Σneeds))`. Delivered-vs-ideal: code gives `t·(avail/Σneeds)·rated`; the fuel-consistent answer is `min(t, avail/Σneeds)·rated`. Two regimes of shortfall:
  - `t·Σneeds ≤ avail < Σneeds` (e.g. 50% throttle, tank holds 90% of a full-throttle tick): ideal delivers **full commanded thrust**; code delivers `avail/Σneeds` of it — a landing burn under-thrusts ~2× despite sufficient propellant for the commanded rate.
  - `avail < t·Σneeds` (the true dry-out tick): code delivers `t·avail/Σneeds` vs ideal `avail/Σneeds` — under-delivers by factor `t`, and drains only `t·avail`, leaving `(1−t)·avail`.
- **Second-order effect:** because debit `= t·avail` on each depleted tick, contents decay geometrically (`avail ← (1−t)·avail`) — at low throttle the tank never cleanly empties; a denormal residue lingers in hashed `tank_contents` for ~minutes of sim time while `engine_can_draw` stays true and thrust ≈ 0. Harmless for determinism, sloppy bookkeeping.
- **Impact:** NOT a free-impulse hole — delivered impulse and debited mass correspond exactly at `isp·g0` per engine (`u = dm_kg·frac/mpu`, so `Σ_r u·scale·t·mpu = scale·t·dm_kg` = `thrust·dt/(isp·g0)`), and impulse is conserved over the whole tail. The cost is behavioral fidelity in the depletion regime only; determinism is unaffected.
- **Suggestion:** scale the demand pool by the tick's *actual* commanded throttle. `body.throttle` is already final at plan time for non-`Burning` vessels (drain applied `SetThrottle` at L3510; `drive_node_burn` only overwrites while `Burning`). Keeping full-throttle normalization for `Burning` vessels is the safe bound there — the executor's final-tick partial throttle then under-debits conservatively (leaves residue, never overdraws). Alternatively, document the under-delivery as intended v1 semantics — but the current docs claim `min(1, avail/Σneeds)` "lands debit == availability," which is only exact at `throttle = 1`.

### F2 — MEDIUM (data integrity, save boundary): `WeldPart::engine` added with no save migration — restored pre-change saves become unmetered

- **File:** `crates/csp-phys/src/part.rs:303` (`pub engine: Option<EngineSpec>`), `crates/csp-sim/src/save.rs:74` (`SAVE_VERSION_CURRENT: u32 = 2`, unchanged), `crates/csp-core/src/hash.rs:191-197` (acknowledges "save format change" but only bumps the hash schema).
- **Problem:** `WeldPart` is serialized inside `Vessel.craft → CraftDef.parts`. CBOR encodes structs as field-name maps, and serde gives `Option<T>` fields an implicit `None` on a missing key — so a v2 body written before this commit **decodes successfully** with `engine: None` on every part, including resolved engines.
- **Impact:** a restored vessel's engines silently become *unmetered* (`engine: None` is the documented free-burn convention, part.rs:315-319) — infinite propellant after load. No error, no migration; the save header's `hash_schema_version` only explains the divergence after the fact. The `altitude_m` precedent (hash.rs:181-189) shows the project treats silent default-decodes as a deliberate, documented choice — here `None` is not a neutral default.
- **Suggestion:** since the spec cannot be reconstructed from `WeldPart` alone (isp/propellants live in `PartDef`), the honest options are: (a) bump `SAVE_VERSION` to 3 and reject v≤2 bodies that carry any `Engine` history, or (b) if saves-across-builds is policy, add `#[serde(default)]` explicitly *and* a restore-time re-resolution/notice. At minimum, document that pre-20 saves restore with unmetered engines.

### F3 — LOW (bookkeeping precision): `drive_node_burn` books Δv against pre-debit mass

- **File:** `crates/csp-sim/src/lib.rs:7909-7916` (`full = f_max·dt/body.mass_kg`), called at L3595 **before** this tick's `transfers::debit` + `reweld_if_mass_changed` (L3620-3629) and the post-debit `batch.push(body.clone())` (L3644).
- **Problem:** the stepped body carries the *post-debit* mass; on contents-quantum flip ticks (mass drop ≤ `cap·mpu/1024` per tank) the integrator delivers `F·dt/m_post` while the executor books `F·dt/m_pre` — delivered Δv exceeds the `dv_remaining` decrement. Cumulative over a burn ≈ `Δv·(mass shed during burn)/mass` — order ~1% for a fuel-heavy stage; burns overshoot commanded Δv slightly.
- **Impact:** deterministic (same computation both sides of a replay); contradicts the doc's "lands exactly on the node's command" (L7876-7878). Bounded by the §2.4 quantum model's own error budget, so arguably acceptable — but the claim is imprecise.
- **Suggestion:** either accept + document the bound, or recompute `full` against post-debit mass (nontrivial: the executor's throttle decision feeds the debit — the order is load-bearing; documenting is cheaper).

### F4 — LOW (edge semantics): `engine_can_draw` is vessel-global — one starved metered engine kills unmetered thrust sites

- **File:** `crates/csp-sim/src/lib.rs:7846-7871`, clamp at L4495-4498.
- **Problem:** `any_spec` is set by *any* `engine: Some` part; if no spec'd engine is `live_site && fueled`, the gate returns `!any_spec` = false and clamps the **body-global** `throttle` to 0 — which also kills `engine: None` (unmetered) `Thrust` sites on the same craft, contradicting the documented "unmetered keeps its throttle channel" convention (L7843-7845). Likewise a spec'd engine whose part has no positive-rated site forces a permanent clamp even though it could never thrust anyway.
- **Impact:** only reachable on hand-authored crafts that mix metered and unmetered parts, or carry a thrust-less `EngineSpec` — resolved data always specs every engine module. Degenerate-input semantics, not a live-content bug.
- **Suggestion:** if the mixed case should be honored, the gate needs per-site semantics (unmetered sites can't be distinguished at the body-global throttle anyway — throttle is per-body, so a per-part gate isn't expressible; arguably the honest fix is documenting "a craft is either fully metered or fully unmetered" and/or rejecting mixed crafts at spawn validation).

### F5 — LOW (resolver mirror gap): a second `Engine` module overwrites the spec but keeps both thrust sites

- **File:** `crates/csp-phys/src/resolve.rs:1695-1723`; validator rule at `crates/csp-data/src/validate.rs:~2826-2835` (`"more than one engine module"`).
- **Problem:** `resolve_weld_input` assigns `engine = Some(...)` per `ModuleDef::Engine` (last wins) while each module pushes a `Thrust` force site. The resolver documents that it "cannot assume validation ran" — and it mirrors most per-module rules (`check_engine_propellants`, tank dedup, the one-`Lab` rule at L1795-1803) — but does **not** mirror the one-engine-per-part rule. An unvalidated def resolves to combined thrust debited against only the *last* engine's `isp`/`propellants`.
- **Impact:** never a free burn (a spec exists → metered; `propellant_plans` debits `rated = Σ all sites` against it), but the consumption rate/mix is wrong vs. authored intent. Deterministic.
- **Suggestion:** add the `Lab`-arm precedent: second `ModuleDef::Engine` → `ResolveError::InvalidStat`.

### F6 — INFO (for the P04 reviewer): `SetThrottle` on a `SurfaceLatched` vessel unlatches it

- **File:** `crates/csp-sim/src/lib.rs:4495-4498` → `unlatched_body` (L3316-3358) re-materializes a latched vessel `InBubble` on **any** `SetThrottle`, including `throttle: 0` and including on a dry engine (clamp happens after materialization). Pre-existing convention shared with `SetAttitude` — the new `engine_can_draw` check runs before it and doesn't change the behavior, but it's a residence transition triggered by a throttle command worth confirming against the RecoverControl/latch rules.

---

## Verified correct (per the focus list)

- **Pooled coverage lands `debit == availability` on the dry-out tick:** at `throttle = 1`, `Σ_plan u·scale ≤ cov·Σneeds = avail` per resource; co-claimants split the pool (`min` over each plan's propellant coverages), so two engines can never both claim the same units. `transfers::debit`'s `take = old.min(remaining)` (transfers.rs:504) hard-clamps any f64 summation residue — contents can never go negative. (The `avail_sum − Σ sequential takes` association asymmetry leaves a ≤ulp residue; harmless.)
- **`units = INFINITY` → starve, never free burn:** un-holdable/massless propellants put `INF` in `demand` → `total.is_finite()` false → `cov = 0` (L7795-7802) → `scale = 0` (L7803-7811); the resulting `INF·0 = NaN` debit is guarded by `amt.is_finite()` (L3608). `engine_can_draw` agrees (`vessel_resource = 0` → not fueled → clamp).
- **Scale composition idempotent across the re-weld:** `apply_fuel_scales` writes `base·factor·scale` read from the *craft* baseline `part.force_sites[j]` (never the body's), so re-application after `rebody` rebuilds is a bitwise recompute (L3639-3641). Positional `j` alignment is guaranteed: `weld` copies `ep.force_sites` verbatim into `[site_start, site_start+site_len)` (weld.rs:618-630).
- **Debit ordering deterministic:** plans iterate `craft.graph.nodes` (`BTreeMap<PartNodeId>`), `demand`/`coverage` are `BTreeMap<ResourceId>`, `avail_of` sums `tank_contents.values()` ascending node, `debit` drains ascending node — all fixed-order; f64 summation order is pinned everywhere it matters.
- **`engine_can_draw` semantics as documented:** `craft: None` → true (unmetered debug craft keeps throttle); engine-less → `!any_spec` → true; any-spec-but-all-starved → false → `SetThrottle` clamps to 0. `SetThrottle` cannot relight a dry engine (test L17300-17346 passes by construction; the gate runs on live `tank_contents` at apply).
- **Burn executor honesty:** `drive_node_burn` reads `net_thrust_axis` *after* `apply_thrust_factors` + `apply_fuel_scales` (L3577-3595), so a dry tank mid-burn → `f_max = 0` → node completes with honestly-delivered partial Δv (L7894-7905), `throttle` zeroed, vessel returns to rails. A dry vessel's scheduled node completes instantly on materialization — correct.
- **No tank/`tank_contents` desync:** jettison removes the node's contents (L6106), breakup partitions via `split_vessel_records` (transfers.rs:1641-1662), latch-merge remaps (transfers.rs:1596-1617), and `restore_vessel_contents` (L8814-8850) rejects contents on non-nodes/undeclared resources/out-of-capacity and wet-mass overflow — so `avail_of` can never count undrawable phantom units.
- **Malformed specs cannot enter:** the weld input gate re-validates `EngineSpec` (positive finite isp, non-empty unique positive fractions summing to 1, weld.rs:443-465); resolver mirrors validator bounds (`check_engine_propellants`, resolve.rs:1089-1127); spawn enforces the wet-mass convention (`check_tank_masses`, L5048; `spawn_contents` fills tanks full).
- **Hash coverage correct:** `h.opt(&p.engine, …)` at L9500-9509 (schema v20); the *scaled* body `rated_n` is deliberately not hashed — it's a derived per-tick view, recomputed from hashed inputs (craft spec + `tank_contents` + reliability).
- **`leak_part` interplay:** post-step leak drains `tank_contents` via `bubble_part_tick` and re-welds via `rel_rebody` (L3902-3911); the rebuilt baseline sites are rescaled by next tick's pre-pass before any consumer reads them — no unstale-rated window.

## Minor notes (not findings)

- `engine_can_draw`'s `live_site` reads *baseline* `rated_n`, so a reliability-derated-to-0 engine still counts "live" — throttle stays legal but delivers 0; consistent either way.
- A latched vessel that goes `InBubble` via `SetThrottle{0}` is F6 above.
- `propellant_mpu` takes the first declaring tank in graph order — assumes per-resource `mpu` uniformity across parts (true for resolved data; mixed-mpu hand-authored tanks are degenerate input).
