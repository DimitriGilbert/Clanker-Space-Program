# Ruthless Review: crates/csp-phys/src/resolve.rs (attachment resolution / constraint solving)
- Reviewer session: ruthless-20261025 (report 9)
- Date: 2026-10-25
- Mode: STATIC ONLY (no cargo; tree mid-edit)
- Status: IN PROGRESS

## Findings


### [SEVERITY: HIGH] Finding 1: `qual_factor` output gate checks one point, but the curve ships and is re-evaluated live — a malformed curve passes resolve and can poison the save at runtime
**File**: crates/csp-phys/src/resolve.rs:2111-2120 (gate), :1570 (passthrough), :1578-1599 (`is_valid`)

**Problem**: `resolve_reliability` gates only the *point evaluation* of the qualification curve at the family's units *at resolve time*:
```rust
let qf = qual_factor(&def.reliability.qualification_curve, qual.units());
if !(qf.is_finite() && qf > 0.0 && qf <= 1.0) { return Err(...); }
```
The curve itself then travels verbatim: `ReliabilitySpec::from_effective` (resolve.rs:1570) copies `qualification_curve` into the craft spec, and the sim re-evaluates it at **live** units with no gate (csp-sim/src/reliability.rs:816, :1382, :1432 — `csp_phys::qual_factor(&spec.qualification_curve, units)`). `ReliabilitySpec::is_valid` (resolve.rs:1578-1599) checks MTBF, infant, dormant, stress rules, failure modes, engine — and never the curve.

**Evidence**: A curve that is well-behaved at resolve-time units but malformed beyond them — e.g. knots `[{0, 1.0}, {100, NaN}]` or `[{0, 1.0}, {100, 2.0}]` — resolves a FRESH family cleanly (units=0 → first knot 1.0, gate passes). The validator forbids this (validate.rs:2901-2931), but this file's entire R14-F2/F5 gate series exists on the stated premise that "the resolver cannot assume validation ran" (resolve.rs:1104-1110, 2083-2087: gates exist so unvalidated defs "fail typed rather than ... leak NaN/∞ into `ResolvedReliability`"). For this one field the gate is at the wrong place.

**Impact**: Family qualification is *live* state (the module doc's step 7): mid-flight the family banks bench/flight units, the sim crosses the bad knot, and `qual_factor` returns NaN (or >1, silently doubling λ). NaN λ flows `restamp` → `hazard_at` → `usage.hazard_integral` (reliability.rs:216-220), which is §7.2-serialized state. On the next load, `resolve_part` → `apply_wear` → `PartUsage::is_valid` (resolve.rs:397-410) rejects the NaN accumulator → `SimError::SaveBlocking` (resolve.rs:721-725). Player-visible: bench-test a family, and an in-flight craft's save becomes permanently unloadable and unpersistable — the worst failure class short of a crash. NaN also makes every `Λ > θ` comparison false, so the affected part silently never fails.

**Suggestion**: In `resolve_reliability`, gate the whole curve exactly as the validator does (mirroring every other gate in this file): non-empty, all knots finite, `qualification_units >= 0` strictly increasing, `hazard_multiplier` finite in `(0, 1]`. Keep the point gate for defense in depth. Alternatively clamp/validate in `qual_factor` itself — but the resolver-side curve gate is the fix consistent with the file's own contract.

### [SEVERITY: MEDIUM] Finding 2: reliability passthrough gates are finiteness-only — range-valid rules are not mirrored, so bad parts resolve in the editor and fail only at manufacture with a craft-level error
**File**: crates/csp-phys/src/resolve.rs:2088-2110 (gates), :1578-1599 (`is_valid` ranges)

**Problem**: The M3-C passthrough gate in `resolve_reliability` checks only `is_finite()` for `infant_mortality`, `dormant_stress`, regime thresholds/multipliers, and failure-mode weights. The `csp-data` validator enforces ranges on every one of these — `infant_mortality` in `[0, 1)` (validate.rs:2889), `dormant_stress` in `[0, 1]` (validate.rs:2895), stress multipliers `>= 1.0` (validate.rs:2940), non-empty `failure_modes` with positive weights (validate.rs:2947-2953) — and `ReliabilitySpec::is_valid` in this same file enforces the same ranges at spawn.

**Evidence**: A def with `infant_mortality: 1.5`, `dormant_stress: 2.0`, a stress rule `multiplier: 0.5`, or an empty `failure_modes` list passes `resolve_part` cleanly (finite), displays in the editor, and produces a serializable `EffectivePart` — directly contradicting the `EffectivePart` construction guarantee (resolve.rs:460-467: "every emitted numeric family is gated at the stage that produces it") and the M3-C comment's intent (resolve.rs:2083-2087). The rejection happens only when `manufacture` runs `craft.reliability.iter().all(|s| s.is_valid())` (csp-sim/src/manufacture.rs:148-155).

**Impact**: Player-visible: a craft with such a part builds, costs out, and saves fine in the VAB, then every build/launch order fails with the generic `"manufacture: incoherent reliability table"` — a message that names neither the part nor the field, on a def the editor already accepted. Compare: every other out-of-range stat in this file fails at resolve time with a typed `InvalidStat` naming the part and the rule.

**Suggestion**: Extend the resolve.rs:2088-2110 gate block to the validator's ranges (`(0.0..1.0)` / `(0.0..=1.0)` / `>= 1.0` / non-empty modes with `w > 0`) so the failure is typed, part-named, and surfaces at the editor boundary like every other gate in this file.

### [SEVERITY: MEDIUM] Finding 3: two `Engine` modules resolve to *different* engines depending on the consumer — `WeldPart.engine` is last-wins, `ReliabilitySpec::engine` is first-wins — and the comment claims a mirror that does not exist
**File**: crates/csp-phys/src/resolve.rs:1767-1781 (last-wins), :1550-1562 (first-wins), :1773-1777 (comment)

**Problem**: In `resolve_weld_input`, the per-module loop does `engine = Some(EngineSpec { isp_vac_s, propellants, .. })` — a second `Engine` module silently overwrites the first. In `ReliabilitySpec::from_effective`, `eff.modules.iter().find_map(...)` picks the **first** engine's `rated_burn_s`/`ignitions`. The comment at resolve.rs:1773-1777 says "validator: at most one Engine module per part, mirrored by `check_engine_propellants`" — but `check_engine_propellants` (resolve.rs:1156-1194) only validates each module's propellant list; it does not check the one-engine rule, so nothing mirrors it.

**Evidence**: The validator does reject multiple engines (validate.rs:2832 "more than one engine module"), so this is reachable only by a def that reached the resolver unvalidated — the same defence-in-depth premise the file states everywhere else (and enforces for Lab: resolve.rs:1874-1876 refuses a second `Lab` module with a typed error, showing the intended pattern). On such a def with two different engines, the sim's propellant burn bookkeeping (`throttle·rated/(isp_vac·g₀)` split by the EngineSpec fractions — the comment's own description) debits against the *last* engine's isp/mix while `BurnBeyondRated` stress and the ignition budget key on the *first* engine's rating — silently inconsistent numbers, plus two thrust sites whose `rated_n` match neither bookkeeping view.

**Impact**: Honest reachability: validated data cannot trigger it. Under the file's own premise (unvalidated/hand-edited/serialized defs), the divergence is silent — no error, just burn-rate and stress numbers computed from two different engines. Player-visible symptom would be fuel draining faster/slower than the Δv readout predicts.

**Suggestion**: Reject a second `Engine` module typed, exactly like the `Lab` arm ten lines below: `if engine.is_some() { return Err(bad("resolve: more than one Engine module on a part")); }`. That makes the comment true and the record unambiguous.

## Coverage notes (checked, no findings)

- **Determinism**: clean. `BTreeMap`/`BTreeSet` for every map/set, `libm::pow` the only transcendental on the stat path (resolve.rs:837), no `f32` anywhere, fixed summation orders (`propellant_mass`, `fold_multipliers`), per-part independence with output order = `built` order; tests assert bit-identity and permutation safety (resolve.rs:3034-3066).
- **Degenerate configs**: zero/negative mass gated (resolve.rs:2195-2203), empty `Convex` gated (resolve.rs:1752-1754), denormal-MTBF ∞ hazard gated (resolve.rs:2074-2082), wear overflow gated (resolve.rs:1476-1481). Coincident attach nodes and graph cycles are weld/graph concerns, correctly not this file's.
- **Placeholder math is honestly labeled**: octagonal-prism cylinder, point-cloud `Convex` inertia, wear stub — each documented as approximation/stub at the definition site, and the stub confirmed display-only (sim λ uses `ReliabilitySpec` methods, not `ResolvedReliability.hazard_per_s`).
- **Inertia formulas verified** against `SymMat3::solid_box`/`solid_cylinder` (half-extents/half-height contracts match, tensor.rs:182, :204) and the `shift` parallel-axis form (tensor.rs:87).
- **Scale/stretch math** (`s^e · t^max(e−2,0)`, cross-section-only node diameter, `linear_z` on the stack axis) is internally consistent across collider, nodes, dock ports, `size_m`, and thrust site.
- Note for the coordinator: this file is the §3.1 *effective-stat* resolver, not the joint/contact solver — "attachment resolution order/fairness", "iteration caps", and "energy injection at joints" live in `weld.rs`/`backend.rs` and are out of this file's surface; the degenerate-config, determinism, and silent-failure focus items were applied in full.

## Verdict

- 1 HIGH: qualification-curve gate checks a point, the curve ships ungated and is re-evaluated live — NaN λ can poison serialized `PartUsage` and refuse the save (Finding 1).
- 2 MEDIUM: reliability range rules not mirrored at resolve — parts pass the editor, fail manufacture generically (Finding 2); two-engine last-wins vs first-wins divergence under an unmirrored comment claim (Finding 3).
- Everything else held up: gates are unusually thorough, determinism is real and tested, placeholder math is documented, not disguised.
- REQUEST CHANGES — Finding 1 must be fixed before the reliability loop can be trusted with live qualification state.
