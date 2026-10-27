# Review Report C10 — Reliability (§3.4) + Joints/Docking (§2.4 M6)

Files reviewed in full: `crates/csp-sim/src/reliability.rs` (1,889), `crates/csp-sim/src/joints.rs` (1,256),
plus the wiring in `crates/csp-sim/src/lib.rs` (undock/save-gate/hash/restore/tick integration), the UI
surface (`csp-ui/src/program.rs` FACILITIES, `csp-ui/src/vab.rs` confidence/stress, `csp-ui/src/game.rs`
revert, `csp-view/src/lib.rs` toasts/part-menu), and the math primitives (`csp-phys/src/resolve.rs`,
`profile.rs`, `merge.rs`, `csp-core/src/rng.rs`). Full csp-sim suite run: **363 passed, 0 failed**.
One issue verified empirically with a scratch test (since reverted).

---

### [HIGH] Finding 1: Undock is a functional no-op on the live path — the pair re-captures and re-merges within 2 ticks

**File**: `crates/csp-sim/src/lib.rs:7273` (`apply_undock`) + `crates/csp-sim/src/joints.rs:481` (`lifecycle` detection)

**Problem**: `apply_undock` splits the graph momentum-conservingly but leaves the two vessels mated
port-to-port (fb.pos = nb.pos = parent pos, zero separation impulse, rel speed ≈ 0). It never inserts
the pair into `joint_refused` — the module's own mechanism for "this approach may not re-capture".
The very next `lifecycle` pass re-satisfies the capture predicate (anchors ~0 m ≤ `capture_range_m`
0.5, dirs opposing, rel speed ~0 ≤ 5 m/s), forms a new `LiveJoint`, and the following `latch_pass`
merges the pair again.

**Evidence** (empirical, scratch test through the real command path — spawn pair, capture, latch
merge, enqueue `SimCommand::Undock`, step):
```
tick 0: vessels=[(0, 0, 0), (2, 0, 0)] joints=1   <- new soft-capture one tick after undock
tick 1: vessels=[(0, 1, 0)] joints=0              <- RE-MERGED
```
At ×1 that is ~31 ms of wall time — the player cannot intervene; even an instant throttle-up cannot
beat the 5 m/s capture / 0.5 m/s latch gates from zero separation. The existing tests
(`undock_splits_at_seam_and_restores_two_vessels` lib.rs:18904, `merge_and_split_conserve_momentum`
:19141) step **exactly one tick** or call `apply_undock` directly, so the suite certifies a split the
live loop immediately undoes. The redock test (:19234) drives `latch_now` directly, bypassing
detection.

**Impact**: M6-C's undock half ("the graph splits at the seam via the breakup path", PLAN §2.4 step 3)
does not work through the command surface. Today it is agent/MCP-reachable (`Undock` is
`ui-unreachable` at lib.rs:778 per §6.4-4, so no player path yet), but the sim machinery is the
deliverable the future docking UI will wire, and the tests give false confidence now.

**Suggestion**: In `apply_undock`, insert the canonical pair into `self.joint_refused` (shadowed via
`shadow.joint_refused`) before returning. The refusal self-prunes the moment the pair leaves capture
geometry (joints.rs:518), so backing off and re-approaching re-attempts exactly as designed. Add a
regression test that undocks through the command path and steps ≥ 5 ticks asserting 2 vessels.

---

### [MEDIUM] Finding 2: §3.4 stress multipliers are displayed in the editor but not in flight

**File**: `crates/csp-ui/src/vab.rs:3509–3589` (probe table) vs. no flight-side consumer of `stress_breakdown`

**Problem**: PLAN §3.4: "*Each multiplier is **displayed to the player in editor and flight**. Hidden
stress multipliers are bad design.*" The editor half is wired (the VAB part inspector renders
`csp_phys::stress_breakdown` under an editable probe — resolve.rs:1755 exists precisely for this).
The flight half is absent: `csp-view/src/flight.rs` and `csp-ui/src/lib.rs` surface only consequences
(`thrust_factor` derate in readouts, part-menu wear Λ/θ, failure flags at csp-view lib.rs:3920–3946).
Nowhere in flight is the player shown which stress rules are currently firing (overthrottle,
overtemperature, g-load, ullage, restarts) or their multipliers.

**Impact**: A mid-flight part accruing accelerated hazard (e.g. `OverThrottle` ×3) is invisible until
it fails — the exact "hidden stress multiplier" the PRD bans. The `BubbleEnv` inputs already exist in
`bubble_part_tick`; the record even stores `lambda_per_s`.

**Suggestion**: Extend the part-menu reliability rows (`ReliabilityRow`, csp-view lib.rs:3935) with
the live `stress_breakdown` of the tick's `RegimeMetrics` (the sim already computes them per tick;
mirror them on the vessel record or recompute view-side from throttle/temp/g readouts).

---

### [MEDIUM] Finding 3: VAB mission confidence ignores live qualification and the profile's burn segments are not editable — the bench loop's payoff never reaches the designer

**File**: `crates/csp-ui/src/vab.rs:3133–3155`; `crates/csp-view/src/lib.rs:4650` (quals read for FACILITIES only)

**Problem**: M9b's exit criterion is "real `MissionProfile` confidence UI" and §3.4 says the profile
is "*Produced by the design assistant (§2.11), **editable in the editor***". The formula is real
(`csp_phys::confidence` = exp(−Λ_expected) over burn segments with per-segment stress, profile.rs:204–310),
but the VAB evaluates it with `FamilyQualifications::default()` — always zero units — and the
"Mission profile (stub)" collapsible edits only target Δv / TWR; `burn_segments` is the fixed
`terran_ascent()` preset (120 s atmo + 180 s vacuum, profile.rs:115–125).

**Impact**: The §3.4 economy loop "pay (bench ₣) → learn (qual units) → build qualified" breaks at
the last leg: after spending ~₣5–10k bench-testing a family to hazard ×0.4–0.5, the editor still
shows the *unqualified* confidence (honestly labeled "(unqualified family)", but the number never
improves). FACILITIES shows the units/hazard factor (program.rs FACILITIES rows) so the loop is
half-traceable; the editor is where the purchase decision would pay off. The design-assistant half
is a known post-v1 §2.11 gap, but "editable in the editor" is M9b scope and quals are already
readable view-side.

**Suggestion**: Plumb the save-side qualification table (`sim.tech().qualifications`, already
consumed at csp-view lib.rs:4650) into the VAB session input and drop the `default()`; add a minimal
burn-segment editor (duration/throttle/regime rows) to the Mission profile panel.

---

### [MEDIUM] Finding 4: DRY — flight-qualification burn credit duplicated 4×, failure-renewal logic duplicated 2×, aero-mask remap duplicated 2×

**File**: `crates/csp-sim/src/lib.rs:4503–4520` vs `lib.rs:4696–4715` (verbatim duplicate), `reliability.rs:894–905` (third copy); `reliability.rs:1096–1135` (`apply_failure_at`) vs `reliability.rs:1327–1361` (`apply_mode_to_record`); `joints.rs:1050–1082` vs `lib.rs:7330–7349` (ConfigMask remap)

**Problem**: The `(family, burn_s, in_atmo)` → rate-lookup → `credit_qualification` loop appears
twice verbatim inside `step_bubble_range` (once for tick-pass burns, once for removal-path burns)
and a third time in `reliability_residence_change` — three sites that must stay in sync (atmosphere
rate vs vacuum rate, shadow_tech capture, Fatal-only error mapping). `apply_failure_at` hand-inlines
the same renewal sequence `apply_mode_to_record` encapsulates (Λ-consume-to-θ, `renewal_count`
advance, `MIN_POSITIVE` θ guard, mode push) — with the same explanatory comments duplicated. The
`ConfigMask` u64/≥64 positional remap appears in both `apply_latch` and `apply_undock` with the
same 10-line comment.

**Impact**: A future change to burn credit (e.g. a third regime weight, or the credit rate table)
must be made in 3–4 places; the renewal duplication is the riskier one — the anti-stall θ semantics
("the crossing CONSUMES Λ") live in two hand-kept-identical bodies.

**Suggestion**: Extract `fn credit_flight_burns(&mut self, burns: &mut Vec<(FamilyId, f64, bool)>, shadow)` in
reliability.rs and call it from all three sites; make `apply_failure_at` delegate its record mutation
to `apply_mode_to_record` (it only adds the Λ restamp + notification + family credit around it);
extract `fn remap_config_mask(...) -> u64` in csp-phys next to `ConfigMask`.

---

### [LOW] Finding 5: `RunBenchTest` accepts any family string — unvalidated funds sink on the agent channel

**File**: `crates/csp-sim/src/reliability.rs:1168–1211` (`apply_bench_test`)

**Problem**: The apply validates site existence, `launch: true`, funds, and site inventory, but never
checks that `family` exists in the parts catalog (or has a qualification-relevant spec). `credit_qualification`
(tech.rs:139) inserts any key it is given. A typo'd or invented family (reachable via the MCP
`sim.command` generic tool; the UI only offers catalog families) charges funds + metal and credits a
qualification row nothing will ever read — money destroyed with no error.

**Impact**: Low (authenticated channel, self-punishing), but it breaks the "reservation at apply,
delivery at event" contract's spirit — the delivery is provably worthless.

**Suggestion**: Reject with `Recoverable("bench test: unknown part family")` unless the family has a
spec in the parts catalog (the `FACILITIES` input builder's `catalog ∪ live records` set is the
reference list).

---

### [LOW] Finding 6: restore gate misses PartFailure residence coherence — a crafted InBubble+pending pair wedges that record's dormant chain forever

**File**: `crates/csp-sim/src/lib.rs:3160–3198` (residence match covers Soi/Maneuver/Debris but not PartFailure) + `reliability.rs:996` (`fire_part_failure` InBubble no-op leaves `pending_failure_tick` set)

**Problem**: The crafted-save gate enforces `pending_failure_tick` ↔ event pairing (lib.rs:10636) but
not event/flag ↔ non-`InBubble` residence. A snapshot with a pending flag + matching event on an
`InBubble` vessel passes restore; the event then fires into the `InBubble` no-op guard
(reliability.rs:996) which returns **without clearing the flag**; `publish_reliability` skips
`is_some` records forever, and the to-dormant transition deliberately does not clear pending — so
that part can never fail dormant again. Unreachable in legit runs (to-bubble clears the flag and
scrubs the event), which is exactly why the restore gate exists per the e37369e hardening standard.

**Suggestion**: Either add `PartFailure` to the rails-only residence group in the restore walk, or
clear `pending_failure_tick` in `fire_part_failure`'s InBubble no-op branch (defensive self-heal;
both is best).

---

### [NIT] Finding 7: stale module docs in reliability.rs understate shipped behavior

**File**: `crates/csp-sim/src/reliability.rs:36–38` and `:63–75`

**Problem**: (a) The module doc claims "no UI prompt or revert-policy surface exists at M9b — the
milestone's 'revert policy in the UI' is deferred" — stale: `csp-ui/src/game.rs` ships the F10 revert
with the exact §3.4 disclosure ("revert is not a reroll — pre-drawn thresholds survive"). (b) The doc
says `StuckDecoupler`/`PanelJam` "record flags — the sim has no … decouple command, or deployable
state at v1, so their gameplay effect is honest absence" — both are wired with real effects
(`apply_jettison_part` lib.rs:7095 rejects on StuckDecoupler; `apply_deploy_part` lib.rs:7241 rejects
on PanelJam). Also the "latch merge's mismatched-halves" comments (reliability.rs:719, lib.rs:11185)
describe a case the merge now *refuses* (joints.rs:922–931), never produces.

**Impact**: Doc-only, but it misleads the next maintainer about both the failure-mode contract and
the revert UX.

**Suggestion**: Reword the two doc blocks to the current truth (only `StuckGimbal`/`SensorFault`
remain honest-absence at v1).

---

## Verified-correct (no findings, checked because the focus asked)

- **KDF arity/determinism at draw sites**: θ₀ = `{Reliability, instance, 0}` exactly per §3.4;
  renewals at counter `k`; mode draws at `MODE_COUNTER_BIT | k` (disjoint half of the same stream,
  documented append-only rationale); ignition `{Ignition, instance, k}` (1-based — deterministic,
  collision-free, PRD doesn't pin the base). `u64 instance` is globally unique and hashed.
- **θ anti-save-scum**: drawn once at creation, stored on the serialized record; revert restores the
  snapshot (θ identical); post-revert rebuilds re-draw the *same* θ (counter rolls back with the
  snapshot) — a retry, not a reroll, per §3.4.
- **Dormant bookkeeping**: epoch-anchored closed form (`dormant_failure_tick`), publish slot in the
  §2.6.5 constant subsystem order, `dormant_stress = 0` off-switch, non-fatal-only mode filter,
  U=0 `MIN_POSITIVE` guard against +inf θ bricking the hash. Burn-exit credits cover all four exits
  (shutdown, residence change, latch touchdown, impact/eviction/removal).
- **Bench economics**: no result bias (bench deterministically credits `units`; PRD requires no
  bench failures); ₣50/unit + 1 metal/unit + 60 s/unit vs ₣28–45k contract rewards and ₣1M start =
  meaningful but affordable; FACILITIES buttons disclose cost/stock/ETA and pre-gate affordability
  (program.rs FACILITIES section).
- **Merge physics** (joints.rs:952–983): linear `p_total/m_total` exact; angular
  `L = I_p·ω_p + r_p×m_p·v_p + (R·I_s·Rᵀ)·(R·ω_s) + r_s×m_s·v_s` about the merged CoM is the correct
  rigid aggregate (Σm·r = 0 makes the absolute/relative velocity distinction moot); merged-graph
  frame = primary frame (primary node ids preserved by `merge_graphs` remap). Conservation tested at
  §2.4's 1e-9 tolerances; redock bit-identical mass properties tested (M6 exit criterion).
- **Undock momentum stamps**: rigid split with unchanged velocity field conserves p and L exactly
  (verified against the code path, and by `merge_and_split_conserve_momentum`).
- **Hash/save coverage**: `hash_record` covers every `PartReliability` field incl. `renewal_count`;
  vessel records, craft spec tables, bench arena + `next_bench_id`, `reliability_cfg`,
  `next_part_instance_id`, joints + refused pairs all hashed; §7.4 rule 1 (refuse save on live
  joints) wired at lib.rs:3518; refused pairs serialized/restored (re-capture divergence argument in
  joints.rs:39–50 is sound); `DockSeam`s serialized per §7.2.
- **Soft-capture lifecycle edges**: capture impossible under rails warp (bubble empty is a warp
  precondition); debris capture is legal; breakup/jettison mid-window handled by `port_alive` +
  per-tick anchor re-derivation; vessel departure dissolves; mixed-spec latch refused cleanly
  (joints.rs:922) instead of corrupting positional spec tables; the 16-joint cap degrades to the
  §2.4-verbatim clean separation + one `Recoverable` per approach.

## PRD coverage table (§3.4 / §2.4 M6)

| Requirement | Status | Evidence |
|---|---|---|
| §3.4 hazard λ = λ_base·Π(stress)·qual (+infant) | WIRED | reliability.rs:503, csp-phys resolve.rs:1678/1740 |
| §3.4 θ pre-drawn, stored, revert-safe | WIRED | reliability.rs:441, game.rs revert + disclosure |
| §3.4 Λ closed-form, warp-safe, publishes next-event | WIRED | epoch anchor + `publish_reliability` in SUBSYSTEM_ORDER |
| §3.4 ignition Bernoulli per index | WIRED | reliability.rs:581 (1-based index; cosmetic) |
| §3.4 PartFailure Silent event | WIRED | EventKind::PartFailure; in-bubble via tick pass (documented §2.6.5 reading) |
| §3.4 dormant hazard: small, non-fatal-only, recorded, off-switch | WIRED | reliability.rs:552/564; notification log §7.2 |
| §3.4 stress multipliers displayed editor **and flight** | PARTIAL | editor WIRED (vab.rs:3509); flight MISSING (Finding 2) |
| §3.4 failure modes with real effects | WIRED | DegradedThrust/IgnitionFailure/Leak/Explosion + StuckDecoupler:7095 + PanelJam:7241 (doc stale, Finding 7) |
| §3.4 qualification programs (test stand = funds+res+time → bench_units; flights regime-weighted; failures count) | WIRED | apply_bench_test; credit paths at 4 exit sites |
| §3.4 MissionProfile confidence exp(−Λ), editable in editor | PARTIAL | real formula, fixed preset, zero quals in VAB (Finding 3) |
| §3.4 revert policy + UI disclosure | WIRED | game.rs:113–149 (module doc stale, Finding 7) |
| §2.4 transient joints, 16-cap, clean-separation degrade | WIRED | joints.rs:541–552 verbatim behavior |
| §2.4 soft-capture window (192 ticks default) | WIRED | joints.rs:116/503 |
| §2.4 RapierBackend island solve | DEVIATION (documented) | joints.rs:14–18 — PGS inside SimpleBackend; defensible under the §6.4-4 cut |
| §2.4 latch merge → one WeldedBody + DockSeam, bit-identical | WIRED + tested | apply_latch; undock_then_redock test |
| §2.4 undock splits via breakup path | WIRED-but-BROKEN on live path | Finding 1 |
| M6 ResourceTransfer / crew transfer across dock | WIRED | transfers remap at absorb (joints.rs:1174), `merge_vessel_records` |
| §7.4 refuse save while jointed; §7.2 serialize seams/refused | WIRED | lib.rs:3518, snapshot vessels + joint_refused |
| Docking UI absent (lib.rs:778 opt-out) | SANCTIONED | §6.4-4: "Orbital docking (M6) → post-v1, but proximity transfer stays" — opt-out honest, not a flag |

## Top 3 issues

1. **[HIGH] Undock instantly re-captures/re-merges** (Finding 1) — one-line `joint_refused` fix; tests currently mask it.
2. **[MEDIUM] §3.4 flight-side stress-multiplier display missing** (Finding 2) — the one §3.4 sentence with no flight surface.
3. **[MEDIUM] VAB confidence ignores qualification + non-editable profile** (Finding 3) — the bench economics loop never pays off in the editor.
