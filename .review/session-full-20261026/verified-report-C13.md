# Verification of review-report-C13 — Physics: part model + readouts

Verifier method: every finding re-traced against source in the working tree; the HIGH finding's
numbers re-derived by hand from the shipped fixture and the shipped model code; `cargo test -p
csp-phys --lib readout` re-run (4/4 pass, including `two_stage_readout_separates_mass_and_propellant`).

Summary: 7 confirmed (2 with corrections to sub-claims), 0 dismissed.

---

### Finding 1: VAB stage readout models per-stage engine ignition flight cannot perform — CONFIRMED

**Original**: readout.rs:217 builds each stage row's `engine_ids` from `parts_in_stage`; the sim has
no engine activation (vessel-wide `SetThrottle` only, lib.rs:607; `propellant_plans` plans every
engine-spec node, lib.rs:9027-9050; thrust applied at every `Thrust` site at vessel throttle,
simple.rs:1443-1456). Flight HUD `burn_step` sums all attached engines (flight.rs:2089-2197).
Reviewer's fixture numbers: readout burn 275 s / TWR 0.86 vs flight 187 s / TWR 1.28.

**Verification**:
- Readout engine set: confirmed — readout.rs:217-222 filters `parts_in_stage(stage)` for `Engine`
  modules; module doc (readout.rs:60-61) says "Engines this stage ignites". The VAB staging panel
  (vab.rs:3219-3300, `staging_section`) renders `st.delta_v_m_per_s / twr_surface / burn_time_s /
  thrust_vac_n` verbatim, and `craft_section` (vab.rs:3106-3123) shows the totals + profile verdict.
- No per-engine command path: confirmed exhaustively — the full `SimCommand` enum (46 variants)
  has no engine activation/ignition/throttle-per-engine verb; only vessel-wide `SetThrottle`
  (lib.rs:607-613). `simple.rs` applies `throttle * site.rated_n` at **every** `Thrust` site of
  every part in `body.part_index` (the thrust-sites block after the aero loop, ~1443-1456).
  `propellant_plans` (lib.rs:9027-9050) iterates every node with `part.engine` — no stage filter;
  per-engine variation exists only via fuel coverage, never player command.
- Flight HUD agrees with sim, not readout: confirmed — `burn_step` iterates ALL attached engine
  nodes (`for n in attached.values()`), coverage-pooling exactly like `propellant_plans`
  ("the same pooled min(1, avail/Σdemand) accounting propellant_plans runs per tick",
  flight.rs:1247-1250); `delta_v_walk_from` (flight.rs:2339-2376) burns the whole attached set to
  depletion per drop-set step. In-tree test `propulsion_walk_marks_depletion_and_stage_drops`
  (flight.rs:3163+) exercises a two-engine pooled fixture and documents "The pooled fuel on node 4
  still feeds both engines — the v1 crossfeed model".
- Numbers re-derived from the shipped fixture (booster 20 kN/280 s + sustainer 10 kN/300 s,
  2000 kg fuel, wet 2380 kg, G0 = 9.81):
  - Readout stage-1 row (engine set = {booster}): ṁ = 20000/(280·9.81) = 7.281 kg/s → burn =
    2000/7.281 = **274.7 s**; TWR = 20000/(2380·9.81) = **0.857**; Δv = 280·9.81·ln(2380/380) =
    **5039.5 m/s** (booster Isp credited over the whole ratio). The shipped test asserts exactly
    these (wet 2380, burned 2000, dv1 = 280·G0·ln(2380/380)); re-ran, passes.
  - Flight/HUD/sim at full throttle (engine set = {booster, sustainer}): F = 30 kN, ṁ =
    10000/(300·9.81) + 20000/(280·9.81) = 3.399 + 7.281 = 10.680 kg/s → burn = **187.3 s**;
    TWR = 30000/(2380·9.81) = **1.285**; Isp_eff = 30000/(9.81·10.680) = 286.4 s → Δv over the
    same ratio = **5154 m/s**.
  - Deltas: burn −31.8% (readout vs flight), liftoff TWR +49.9%, stage-1 Δv −2.2% (5039.5 vs
    5154). Matches the report's "~30-50%".
- Staging-boundary engine sets: the readout models {booster} → {sustainer}; flight models
  {booster+sustainer} → {sustainer} (jettison removes the booster nodes — lib.rs `fire_stage` →
  `JettisonPart`, csp-view/src/lib.rs:3289-3296). No player action reproduces a booster-only
  firing with the sustainer attached-and-silent, so the stage-1 row (F = 20 kN, TWR 0.86,
  burn 274.7 s) is unreachable in flight. Throttle is vessel-wide; there is no partial escape.
- Mission check: confirmed — `MissionProfile::evaluate` (profile.rs:136-152) reads the first
  engine stage's `twr_surface` against `min_launch_twr` 1.2 (profile.rs:113). For the fixture it
  judges 0.857 (FAIL) on a craft whose actual liftoff TWR is 1.285 (PASS). Since the readout's
  first-stage engine set is always a subset of flight's, the error is one-directional
  (conservative), but it still rejects flyable crafts — "judges a TWR the craft will not launch
  with" is accurate.
- Stock reachability: `data/parts/` ships both `csp.engine.sparc.ron` and `csp.engine.ember.ron`;
  engines on both sides of `csp.decoupler.s125` are a stock build.

Severity HIGH justified: this is the M3 headline readout (per-stage Δv/TWR/burn rows) planning
numbers the shipped flight loop cannot produce, on the canonical stock configuration.

### Finding 2: Vessel-wide fuel pooling + ascending-id drain makes serial staging strictly counterproductive — CONFIRMED (one sub-claim corrected)

**Original**: readout.rs:282-291 pools propellant over the whole attached set; lib.rs:9032-9037
`avail_of` sums the whole vessel; transfers.rs:501-523 `debit` drains ascending `PartNodeId`; in
the top-down build order the upper tank drains first, so the booster drinks the sustainer dry
through the decoupler; readout's own test asserts the 0-Δv starved stage-0 row; flight identical;
staging buys nothing.

**Verification**:
- Pooling: confirmed — readout.rs:282-291 sums `nm.tanks` over ALL attached ids (mpu > 0 filter);
  sim `avail_of` (lib.rs:9033-9039) sums `tank_contents.values()` across the whole vessel with no
  connectivity cut; flight `burn_step` pools `avail` over all attached nodes identically.
- Ascending drain: confirmed — transfers.rs `debit` iterates `&craft.graph.nodes` (BTreeMap,
  ascending id) taking `min(held, remaining)` from each tank in turn; readout's consumption loop
  (readout.rs:333-348) iterates `for id in &attached` (BTreeSet, ascending) identically;
  `consume_drawn` (flight.rs:2200-2224) same. Session ids are allocation-ordered, and the
  canonical VAB flow builds root-first top-down, so the sustainer's tank (lowest id) drains first.
- The shipped test asserts the starvation: readout.rs:645-664 — stage-0 row wet = 250 (dry-only),
  `propellant_burned_kg == 0`, `delta_v_m_per_s == 0.0` after stage 1 burned all 2000 kg.
  Re-ran: passes. Flight does the same by construction (same pool, same drain order).
- "Strictly worse than single-stage": verified arithmetically. Because fuel is pooled and the
  upper tank drains first, remaining fuel always sits in the booster tank: staging at depletion
  jettisons empty dry mass only (Δv identical to never staging — 5154 m/s both ways in flight
  terms); staging early discards the still-full booster tank. There is no staging timing at which
  the sustainer gains exclusive propellant without sacrificing more fuel than it frees. Net:
  staging ≤ never staging, strictly < once the decoupler's 20 kg dead mass (carried parent-side
  through every burn, readout.rs:351-372 keeps the dec attached) is counted.
- **Correction (does not change the verdict)**: the parenthetical "readout's own test asserts
  exactly this … + an `EngineWithoutPropellant` warning (readout.rs:655-664)" overstates. The test
  asserts the 0-Δv/empty-tank numbers only; the warning reference is a code comment. And
  `missing_propellants` (session.rs:2138-2199) would NOT fire for this craft: it checks that the
  engine's post-separation supply region contains a tank *capability* for each propellant
  resource — the sustainer's own (full-capacity) tank is in its region, so no warning. The
  validator cannot see cross-stage depletion. The core claim (pooled fuel + drain order starves
  the upper stage; both models agree; optimal play is never to stage) stands on the test and code.

### Finding 3: Two Δv/burn engines in two crates (DRY), already diverged — CONFIRMED

**Original**: readout.rs:143-384 vs flight.rs:2089-2378 implement the same physics with different
engine sets and depletion loops; Finding 1 is the existing divergence; future fuel-flow changes
must land twice.

**Verification**: confirmed by direct comparison. readout.rs `analyze`: session/BuiltPart-driven,
per-stage engine sets (readout.rs:217-222), per-resource demand from `ṁ = F/(Isp·g₀)`
(readout.rs:253-259), single limiting-resource burn (readout.rs:294-303), `libm::log`
(readout.rs:311). flight.rs: `build_walk`/`burn_step`/`consume_drawn`/`delta_v_walk_from` — live
`tank_contents`-driven, ALL attached engines, coverage-pool model mirroring `propellant_plans`
(shared denominator, min-clamped coverage, per-resource depletion loop up to 64 passes),
`f64::ln` (flight.rs:2357). The engine-set rule differs exactly as claimed (per-stage vs
all-attached), and the HUD side is the one matching the sim. The maintainability hazard is real:
any crossfeed/activation change must be mirrored in csp-phys readout, csp-sim propellant_plans,
AND csp-view burn_step (three sites today, not two — the report names two; the sim is the third
authority the HUD mirrors).

### Finding 4: Parachute inflation ramp bypassed when deploy commanded outside the catch envelope — CONFIRMED

**Original**: `decel_area_m2` (simple.rs:2651-2664) ramps `((at_s − t0)/deploy_time_s).clamp(0,1)`
from the deploy *command* stamp; gates evaluated per tick; staged deploys (`fire_stage` auto-issues
`DeployPart`) command high/fast, so at first gate-pass the ramp is already 1 → 0.4 → 120 m² in one
tick; ~MN snap vs 150 kN `max_axial_n`; external-load attribution hands the force to the chute node.

**Verification**:
- Ramp-from-command: confirmed — `apply_deploy_part` (lib.rs:7209-7257) only checks InBubble /
  node / has-parachute / not-already-deployed / PanelJam, then stamps `deployed.insert(node,
  clock.ticks())`. No envelope gate at apply. `refresh_decel_sites` (lib.rs:8911-8944) copies that
  stamp to `deployed_at_s`. `decel_area_m2` (simple.rs:2651-2664) gates on `alt ≤ deploy_alt_m &&
  speed ≤ deploy_speed_mps` at the *current* tick and ramps from `t0`. A command issued outside
  the envelope reaches gate-pass tens of seconds later, so `(at_s − t0)/1.5 s` clamps to 1.0 —
  full canopy on the first gate-passing tick. Authored data confirmed: csp.chute.s125.ron:52-56
  (6000 m / 260 m/s / 0.4 m² / 120 m² / 1.5 s) and `max_axial_n: 150_000` (ron:43).
- Staged-deploy path: confirmed — `StageActions.deploys` doc (flight.rs:600-616) and
  `fire_stage` issue `DeployPart` per deployable at stage fire with no envelope check
  (csp-view/src/lib.rs:3283-3288). The report's cite "flight.rs:611-615" is the doc site; the
  issuing code is lib.rs:3283 — same behavior.
- Snap force re-derived with the game's own density law (ρ = p·M/(R·T), p = 101325·exp(−h/5600),
  linear T 288.15→216.65 K over 0-12 km; simple.rs:539-546 + data/systems/home/terrin/body.ron):
  - 6 km @ 260 m/s (the gate corner): ρ = 0.479 → F = 0.5·0.479·120·260² = **1.94 MN** = 13.0×
    the 150 kN rating.
  - 5 km @ 230 m/s: 1.78 MN (11.8×). 4 km @ 200 m/s: 1.57 MN (10.5×). MN-scale is robust across
    any plausible gate-pass; no clamp exists in the decel force push (simple.rs:1427-1438).
- Attribution + consequence: confirmed — the canopy force is pushed to `site.node` (the chute's
  own node) in BOTH `accumulate_forces` and `external_loads` (the shared `decel_area_m2` doc
  promises the two channels cannot disagree); the sim feeds `external_loads` into
  `csp_phys::joint_loads` → `failed_joints`, which fails a joint whose load exceeds the child
  part's authored limits (lib.rs:4300-4335; structural.rs:271-299 `axial_n > max_axial_n`).
  A ~1.6-1.9 MN canopy load on a 150 kN-rated radial attachment fails the chute joint on the
  catch tick. "Chute shreds/detaches at first catch" is the expected outcome.

### Finding 5: `BuiltPart.data_set_hash` never stamped on the player path, no consumer — CONFIRMED (§7.7 sub-claim corrected)

**Original**: `stock_build` stamps zeros (vab.rs:1898-1927 → `BuiltPart::stock` → `[0;32]`,
resolve.rs:281); `TechState::build_part` has no production caller; nothing compares
`BuiltPart.data_set_hash` against `csp_data::data_set_hash()`; §7.7 "loading with changed data
warns" unimplemented.

**Verification**:
- Core confirmed: `VabSession::stock_build` (vab.rs:1898-1925) freezes id/variant/upgrades but
  never touches `data_set_hash` — every placed part carries `[0;32]`. `TechState::build_part`
  (tech.rs:108-134) and its `World::build_part` wrapper (tech.rs:383-400) accept a hash but a
  repo-wide grep finds zero callers outside tech.rs's own tests. No code reads
  `EffectivePart::data_set_hash` (resolve.rs:480-483) for comparison — it is only copied
  (resolve.rs:2369), serialized, and hashed. The `BuiltPart::stock` doc's "the manufacture path
  fills every field" is false for the path players use.
- **Correction**: the claim "§7.7 'loading with changed data warns' is unimplemented" is wrong at
  the save level — `csp-app/src/lib.rs:1168-1185` (`do_load`) compares the save's recorded
  `data_set_hash` against the live data root and posts "DATA DRIFT — this save was written against
  a different data set" (saves stamp it at lib.rs:1120). What remains true is the *per-part*
  half: since every player craft records zeros, a future per-part §7.7 check could never fire on
  existing craft, and the field is dead weight today. Same correction applies to the report's
  follow-up line "§7.7 load-time data-set drift warning: absent from the save/load path (C11)" —
  that follow-up is stale/dismissed. Finding stands as a per-part provenance gap; with the
  save-level warning already shipped, MEDIUM is arguably generous but defensible (dead serialized
  field + false doc claim on the player path).

### Finding 6: §3.1 wear composition (`apply_wear`/`wear_multiplier`/`hazard_per_s`) is a dead display projection — CONFIRMED

**Verification**: confirmed. The live λ is `spec.base_hazard_per_s()·stress_product·qual·infant`
at reliability.rs:511; repo-wide grep shows `EffectivePart.reliability.wear_multiplier` /
`.hazard_per_s` have zero readers outside resolve.rs itself (only the pub re-export at
csp-phys/src/lib.rs:122). The resolve.rs:190-200 note honestly documents the supersession and the
removal follow-up. NIT, correctly scoped.

### Finding 7: MissionProfile only half editable; confidence evaluates FRESH qualification — CONFIRMED

**Verification**: confirmed. vab.rs:3156-3182: only `target_delta_v_m_per_s` and `min_launch_twr`
are editable (DragValue → `VabAction::SetTargetDeltaV`/`SetMinLaunchTwr`); `burn_segments` is
displayed as a count only ("{} burn segments · g₀ ref …") and stays at `terrin_ascent`'s fixed
120 s atmo + 180 s vac (profile.rs:115-127). Confidence is computed with
`FamilyQualifications::default()` and labeled "(unqualified family)" (vab.rs:3133-3148) — the
save's family table is not plumbed in. `ExcessRestarts` keyed by segment index (profile.rs:279)
trips ember's authored rule (threshold 0.0, multiplier 3.0 — csp.engine.ember.ron:72) on the
second segment even for one continuous ignition, as stated. Both limitations are labeled in-UI,
so the NIT rating is right.

---

## PRD coverage rows (requested verification)

| Row | Verdict | Evidence |
|---|---|---|
| M3 "live Δv/TWR/burn-time" readouts — WIRED **DEGRADED** | **CONFIRMED** | Readout + panel wired (readout.rs:143-384; vab.rs:3080-3123 totals, 3219-3300 stage rows) but the stage engine model is fictional vs the shipped sim/HUD (F1 numbers above). |
| `MissionProfile` — WIRED **PARTIAL** | **CONFIRMED** | profile.rs:103-153 evaluate; vab.rs:3156-3182 — segments not editable, count-only display (F7). |
| VAB Δv/TWR == flight delivery — **DEGRADED** | **CONFIRMED** | Re-derived: stage-1 burn 274.7 s vs 187.3 s, TWR 0.857 vs 1.285, stage-1 Δv 5039.5 vs 5154 m/s; root causes F1 (engine set) + F2 (pooled fuel), and the two models agree with each other per-model, i.e. the VAB is the outlier vs sim+HUD. |
| §3.1 one resolver shared by editor/sim/launch — WIRED | Spot-confirmed | launch_craft re-resolves through the VAB analysis (`v.recompute()`, csp-view/src/lib.rs:2770-2790); readout resolves FRESH quals (readout.rs:155-157). |
| §7.7 drift warning "absent from save/load path" (Follow-ups line) | **DISMISSED (stale)** | Implemented at the save level: csp-app/src/lib.rs:1168-1185 posts the DATA DRIFT notice on mismatch. The per-part gap (F5 core) remains real. |

## Verdict tally

- Findings: 7 confirmed (F1, F2, F3, F4, F5, F6, F7), 0 dismissed as findings.
- Corrections to sub-claims (finding still stands): F2 (no `EngineWithoutPropellant` warning is
  actually emitted or asserted for the canonical stack — the validator checks tank capability in
  the supply region, not cross-stage depletion), F5 (save-level §7.7 drift warning DOES exist;
  only the per-part half is dead).
- Follow-up line "§7.7 load-time data-set drift warning: absent" — dismissed (stale).
