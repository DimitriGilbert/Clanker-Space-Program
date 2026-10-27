# Verified Report C10 — Reliability (§3.4) + Joints/Docking (§2.4 M6)

Verification of `review-report-C10.md` against the working tree at HEAD `7dd4b86`.
Every finding was checked at the cited file:line with surrounding context; the HIGH finding
was re-verified empirically with a scratch test in a throwaway git worktree under /tmp
(deleted after; the project working tree was never modified). The original csp-sim suite
(363 tests) passes in the worktree; my scratch undock test is the only failure there, by design.

Verdict summary: **7/7 findings CONFIRMED, 0 dismissed.** The report's "Verified-correct"
spot checks requested (merge angular-momentum algebra, KDF stream keys, PRD rows) also hold.

---

### Finding 1: Undock is a functional no-op on the live path — CONFIRMED (HIGH)

**Original**: `apply_undock` (lib.rs:7273) splits momentum-conservingly but never inserts the
pair into `joint_refused`; the next `lifecycle` pass re-captures and the following `latch_pass`
re-merges. Tests step one tick or call the apply directly, so they certify the split the live
loop immediately undoes.

**Verification**:
- Code: `apply_undock` spans lib.rs:7273–7524. It stamps `fb.pos = nb.pos = parent_body.pos`
  (7314, 7321) with material-point velocities (rel speed ~0 at the seam) and touches
  `joint_refused` nowhere — the only shadow interaction is `shadow_vessel`. Confirmed.
- State machine: tick loop calls `joints::latch_pass` (lib.rs:4741) then `joints::lifecycle`
  (lib.rs:4762) at end of every bubble tick. `lifecycle` step 3 detection (joints.rs:526–565)
  skips a pair only if already jointed or in `refused` (joints.rs:532–533); capture predicate
  (joints.rs:383–394) is distance ≤ 0.5 m, opposing dirs, rel speed ≤ 5 m/s — all trivially
  satisfied by a just-undocked mated pair. Defaults confirmed: `DEFAULT_CAPTURE_RANGE_M = 0.5`,
  `DEFAULT_CAPTURE_REL_SPEED_MPS = 5.0`, window 192 (joints.rs:113–122).
- **Empirical reproduction** (scratch test through the real command path in /tmp worktree —
  spawn port pair, capture, latch-merge, enqueue `SimCommand::Undock`, step):
  ```
  tick 0: vessels=[0, 2] joints=1   <- re-captured the same tick
  tick 1: vessels=[0]    joints=0   <- re-merged
  ticks 2-7: vessels=[0]            <- stays one vessel
  ```
  Identical to the report's evidence (2 ticks ≈ 31 ms at 64 Hz).
- Tests: `undock_splits_at_seam_and_restores_two_vessels` (lib.rs:18904) steps exactly one
  `step_frame` (18926) then asserts 2 vessels (18927); `merge_and_split_conserve_momentum`
  calls `w.apply_undock` directly (19141); the redock test (19234) drives `latch_now` →
  `joints::apply_latch` directly (19205), bypassing detection. Confirmed.
- Reachability: `Undock` is commented `ui-unreachable … MCP/benchmarks only today`
  (lib.rs:778–779), and csp-server's `t_sim_command` is a raw `SimCommand` passthrough
  ("escape hatch", mcp.rs:537, 816) — agent-reachable today.
- **On §6.4-4 severity**: PLAN:1551 sanctions cutting the *docking UI/gameplay* to post-v1
  ("Orbital docking (M6) → post-v1, but proximity transfer stays"), and the report discloses
  the ui-unreachability honestly. That does not downgrade the finding: M6-C's machinery
  (latch/undock/seam) is shipped, tested v1 deliverable, reachable via MCP today, and the
  one-line fix (`joint_refused` insert; refusal self-prunes once the pair leaves capture
  geometry, joints.rs:518–520) is exactly what the module's own refusal mechanism exists for.
  HIGH stands: a shipped command that silently undoes itself within 2 ticks, with a test
  suite that masks it, is a correctness bug in the deliverable the future UI will wire.

---

### Finding 2: §3.4 stress multipliers not displayed in flight — CONFIRMED (MEDIUM)

**Original**: editor half wired (VAB probe), flight half absent.

**Verification**:
- PLAN.md:1166 (§3.4) verbatim: "Each multiplier is **displayed to the player** in editor and
  flight. Hidden stress multipliers are bad design." Quote accurate.
- Editor half: vab.rs:3506–3592 renders `csp_phys::stress_breakdown`/`stress_product` under an
  editable probe regime (throttle/T-fraction/g/burn-s/restarts/ullage). Confirmed.
- Flight half: repo-wide grep shows `stress_breakdown`/`stress_product` consumed ONLY in
  csp-ui/src/vab.rs (editor) plus csp-sim's own hazard math (reliability.rs:511). The flight
  part surface (`ReliabilityRow`, csp-view lib.rs:3935–3946) shows consequences only —
  failures, misfire, `thrust_factor` derate, wear Λ/θ, dormant class. csp-view/src/flight.rs
  has no stress-rule/multiplier surface. Confirmed: no flight-side consumer exists.

---

### Finding 3: VAB confidence ignores live qualification; profile burn segments not editable — CONFIRMED (MEDIUM)

**Original**: confidence evaluated with `FamilyQualifications::default()` (always zero units);
"Mission profile (stub)" edits only Δv/TWR over the fixed `terran_ascent` preset.

**Verification**:
- vab.rs:3143: `let quals = csp_phys::FamilyQualifications::default();` — the editor never
  reads the save-side table; label honestly says "(unqualified family)" (3146). Confirmed.
- Profile editing surface: the only profile VabActions are `SetTargetDeltaV`/`SetMinLaunchTwr`
  (vab.rs:251–254, applied at 1025–1034); the "Mission profile (stub)" collapsible (3156+)
  edits exactly those two. `burn_segments` is the fixed `terran_ascent()` preset —
  120 s atmo + 180 s vacuum, `MissionProfile::default()` (profile.rs:109–128, 155–158).
  Confirmed: not editable.
- The formula is real (`expected_hazard`/`confidence`, profile.rs:204–310 with per-segment
  stress rules), and quals ARE read view-side for FACILITIES rows (csp-view lib.rs:4650–4670:
  `sim.tech().qualifications`, units/hazard factor) — the loop is half-traceable as reported.
- Scope framing checks out: PLAN:1506 makes M9b "All of §3.4: … real `MissionProfile`
  confidence UI", and PLAN:1178 says the profile is "editable in the editor". The confidence
  number itself is real, so the exit criterion is minimally met; the miss is the
  qualification plumbing (bench payoff invisible in the editor) and the non-editable
  burn segments. MEDIUM is the right weight.

---

### Finding 4: DRY — burn credit ×3, failure renewal ×2, aero-mask remap ×2 — CONFIRMED (MEDIUM)

**Original**: three in-sync burn-credit sites; `apply_failure_at` hand-inlines
`apply_mode_to_record`; `ConfigMask` remap duplicated.

**Verification**:
- Burn credit: lib.rs:4503–4520 (tick-pass) and lib.rs:4697–4714 (removal-path) are the same
  loop verbatim (the tick-pass copy adds a comment); reliability.rs:894–905
  (`reliability_residence_change`) is a third copy taking `burns` by value. Confirmed
  (3–4 sites counting the loop body only; the report's site list is accurate).
- Renewal: `apply_failure_at` (reliability.rs:1096–1135) and `apply_mode_to_record`
  (1327–1361) both hand-carry: mode push-if-absent, `hazard_integral = theta`,
  `renewal_count += 1`, the `{Reliability, instance, k}` draw, `MIN_POSITIVE` clamp,
  IgnitionFailure/DegradedThrust effects — with the same two explanatory comments
  ("crossing CONSUMES Λ", "+inf θ bricks the hash"). Confirmed; the anti-stall semantics do
  live in two hand-kept bodies.
- ConfigMask remap: joints.rs:1050–1082 (merge) and lib.rs:7330–7349 (undock retained) both
  implement the u64/≥64 positional convention with near-identical comments. Confirmed, with
  one nuance: the two loops differ in shape (2-sources→1 merge vs 1→1 subset), so a shared
  `remap_config_mask` helper is a convention-sharing extraction rather than a verbatim lift;
  the duplication observation itself is accurate.

---

### Finding 5: `RunBenchTest` accepts any family string — CONFIRMED (LOW)

**Original**: apply validates site/funds/inventory but not family existence; funds+metal
destroyed crediting a row nothing reads.

**Verification**:
- `apply_bench_test` (reliability.rs:1168–1253) validates units, site existence, `launch`,
  site state row, funds, site inventory — no check of `family` against `self.site_defs`/parts
  catalog. Confirmed.
- `credit_qualification` (tech.rs:139–158) inserts any key via `entry(family).or_default()`
  with no validation. Confirmed.
- Reachability: `t_sim_command` raw `SimCommand` passthrough (mcp.rs:537/816) accepts an
  arbitrary `RunBenchTest { family, .. }`. The UI FACILITIES builder only offers
  catalog ∪ live-record families (csp-view lib.rs:4644–4661). Confirmed. LOW severity is
  appropriate (authenticated channel, self-punishing).

---

### Finding 6: restore gate misses PartFailure residence coherence — CONFIRMED (LOW)

**Original**: a crafted InBubble vessel with pending flag + matching event passes restore;
`fire_part_failure`'s InBubble no-op leaves the flag set; the record can never fail dormant
again.

**Verification**:
- Restore residence match (lib.rs:3170–3204) covers DebrisDecay/SoiTransition/SoiRescan/
  ManeuverNodeStart; `PartFailure` is not in any rails-only group. The pairing gate
  (`restore_reliability_state`, lib.rs:10636–10642) only checks pending↔event tick equality.
  A crafted InBubble+pending+event snapshot therefore passes. Confirmed.
- `fire_part_failure` InBubble guard (reliability.rs:996–998) returns without clearing the
  flag — in contrast to the fatal-mode branch (1007–1027) which clears it with a comment
  describing exactly this wedge ("leaving it set would make `publish_reliability` skip the
  record forever AND wedge the next snapshot restore"). Confirmed, and the inconsistency
  strengthens the finding.
- Chain: `publish_reliability` skips `is_some` records (reliability.rs:927–929) and skips
  InBubble vessels entirely (923–925); to-dormant deliberately keeps the flag
  (reliability.rs:786–787); to-bubble clears it and scrubs the event (847, doc 788–793) —
  so legit runs cannot reach the state, matching the report's unreachability claim.
- One addition (makes it slightly worse, same severity): after the no-op fire the flag is
  orphaned (event consumed), so a LATER save of that world fails its own restore gate with
  "pending failure tick / event mismatch" — the wedge is not just "never fails dormant
  again" but can brick the follow-up save/load. Still crafted-save-only → LOW stands.

---

### Finding 7: stale module docs in reliability.rs — CONFIRMED (NIT)

**Original**: (a) doc claims no revert UI at M9b; (b) doc claims StuckDecoupler/PanelJam are
honest absence; (c) "mismatched-halves" comments describe a case the merge now refuses.

**Verification**:
- reliability.rs:35–38 claims "no UI prompt or revert-policy surface exists at M9b …
  deferred" — but csp-ui/src/game.rs ships F10 "Revert to launch" with the PLAN-mandated
  disclosure and confirm prompt (game.rs:1–7, 113–159), and PLAN:1506 lists "revert policy
  in the UI" as M9b scope. Stale. Confirmed.
- reliability.rs:68–72 says StuckDecoupler/PanelJam "record flags — the sim has no … decouple
  command, or deployable state at v1" — but `apply_jettison_part` rejects on StuckDecoupler
  (lib.rs:7095–7103) and `apply_deploy_part` rejects on PanelJam (lib.rs:7240–7250). Both
  wired with real effects. Stale. Confirmed.
- The "latch merge's mismatched-halves" comments (reliability.rs:818–820, lib.rs:11184–11187)
  describe a non-covering spec table produced by a latch — but `apply_latch` refuses
  mixed-spec pairs outright (joints.rs:922–931, "The merge refuses the pair instead"), and
  spawn/restore gates enforce coverage, so the described producer no longer exists.
  Confirmed. NIT is the right weight.

---

## Verified-correct spot checks (requested)

- **Merge angular-momentum algebra (joints.rs:952–983)**: VERIFIED CORRECT, including an
  independent empirical run (scratch test, /tmp worktree): two port craft, primary at
  identity, secondary rotated 90° about Y with off-axis velocity — intrinsic angular
  momentum about the island CoM across `apply_latch` conserved to |dL| = 0.000000.
  joints.rs:965 reads `v_s_local = bp.rot.conjugate().rotate(bs.vel)` — the secondary's
  velocity rotated into the PRIMARY's frame (verified by grep against the working tree;
  an initial misreading as `bs.rot` was disproven both by source and by the conservation
  run). `SymMat3::rotate` = R·I·Rᵀ (tensor.rs:55–82); the secondary spin term
  (R·I_s·Rᵀ)·(R·ω_s) and both r×m·v terms are frame-consistent in the primary frame;
  merged frame = primary (`body_m.rot = bp.rot`).
- **KDF stream keys**: θ₀ `{Reliability, instance, 0}` (reliability.rs:441–448); renewals
  counter = `renewal_count` (1114–1124, 1347–1355); mode draws `MODE_COUNTER_BIT | k`
  (1<<63, reliability.rs:131, 529–536 — disjoint half of the stream); ignition
  `{Ignition, instance, k}` (581–594). All confirmed at the draw sites.
- **PRD rows**: §7.4 rule-1 save gate on live joints wired (lib.rs:3518–3522);
  `Undock` ui-unreachable comment at lib.rs:778–779 with PLAN:1551's §6.4-4 cut quoted
  accurately; VAB editor stress display wired (vab.rs:3506–3592); suite count 363 matches
  (worktree run: 363 original tests pass; only my intentional scratch assertion failed);
  `joint_refused` serialized/hashed and gated canonical on restore (lib.rs:10551–10563).
- Report's line-number citations were accurate throughout (one off-by-two at most).

## Final verdict

7 confirmed / 0 dismissed. The report is accurate, well-evidenced, and correctly weighted;
its one HIGH finding is empirically reproducible through the shipped command path and its
fix suggestion (insert the canonical pair into `joint_refused` before returning) matches the
module's own refusal lifecycle (self-pruning at joints.rs:518–520).
