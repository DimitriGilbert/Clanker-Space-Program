# Verified report C06 — verification of review-report-C06.md

Verifier: independent re-read of every cited file:line in the working tree, plus targeted test runs
(`cargo test -p csp-sim --lib orbit::` 22/22 ok; `maneuver` 12/12 ok; `bubble` 9/9 ok;
`duva_reachable_in_sim_end_to_end`, `publish_spend_is_physical_and_bounded`,
`maneuver_executor_delivers_commanded_dv` all ok). No source files were modified.

Score: 5 confirmed / 0 dismissed. Two findings confirmed with factual corrections to
supporting detail (Findings 2 and 5); neither correction weakens the finding — one strengthens it.

---

### Finding 1: §2.6.6 item 4's "h is reported in the debug overlay" is coded but not wired — CONFIRMED

**Original summary**: `PatchedConics::scan_strides` (orbit.rs:1293) is documented as the
§2.6.6-item-4 debug-overlay surface and shares the scan's own stride helpers, but has zero
production consumers; the miss guarantee ("never misses a residence longer than h") is
unobservable and bugreports cannot carry it.

**Verification/Reason**:
- PLAN.md:584-587, item 4, quoted verbatim: "**The real guarantee, stated as such:** encounters
  whose entire SOI residence is shorter than `h` may be missed. `h` is reported in the debug
  overlay. Increasing `v_rel_max` fidelity reduces `h`; the guarantee is not 'never misses', it
  is 'never misses a residence longer than `h'`." The requirement is exactly as the report states.
- `rg -uu scan_strides` across the whole workspace (target/ excluded): orbit.rs:72, 815, 1049,
  1060, 1239, 1293 (definition, docs), orbit.rs:2208/2253/2311/2389 (own tests), and
  crates/csp-sim/src/lib.rs:14274 — which sits inside `#[test] fn publish_spend_is_physical_and_bounded`
  (lib.rs:14248). Zero non-test callers. The `SoiScan` struct (orbit.rs:207) likewise has no
  consumer outside orbit.rs.
- The report's phrase "there is no debug overlay in csp-app/csp-view/csp-ui" needs one
  correction: an M0 debug panel DOES exist (`csp_ui::debug_panel`, csp-ui/src/lib.rs:170-207,
  fed from csp-view/src/lib.rs:535) — but it shows only adapter/backend/tick/frame/warp
  mirrors and carries no SOI or stride data. The substantive claim (no surface anywhere
  reports `h`) holds. crates/csp-app/src/bugreport.rs (244 lines) has zero matches for
  soi/stride/scan.
- orbit.rs's own test `soi_graze_residence_below_h_is_missed` (orbit.rs:1899, passes) proves
  sub-`h` residences are missed by design, so the unreported guarantee has real player-facing
  consequence. orbit.rs:71-74 and 1288-1292 both promise the overlay parity that nothing
  consumes.

**Evidence**: rg outputs above; orbit.rs:1288-1305; PLAN.md:584-587; csp-ui/src/lib.rs:170-207;
csp-app/src/bugreport.rs. Severity HIGH (coded-but-not-wired against an explicit spec
sentence) is justified.

---

### Finding 2: Executor burn starts AT the node tick — no ignition lead; predict_maneuver biased first-order in burn length — CONFIRMED

**Original summary**: The map's predicted conic (predict_maneuver, maneuver.rs:201-222) applies
Δv impulsively at node.tick; the executor materializes and burns forward from node.tick
(lib.rs:6698, 6787-6796, 9230-9261), so the effective impulse trails the node by T_burn/2.
No half-burn lead exists anywhere.

**Verification/Reason**:
- lib.rs:6698: `self.schedule_event(EventKind::ManeuverNodeStart { vessel }, at, shadow)` —
  scheduled at the node tick itself. `fire_maneuver_start` (lib.rs:6750-6837) evaluates the
  conic at exactly that tick (6787-6788), materializes there, arms `Burning` with
  `dv_remaining = dv_total`. `drive_node_burn` (lib.rs:9230-9261) then burns full throttle
  forward until the countdown zeroes. Confirmed: effective impulse ≈ node.tick + T_burn/2.
- maneuver.rs:201-222: `predict_maneuver` adds the whole Δv at `SimTime::from_ticks(node.tick)`.
  Confirmed consumed by the map's planning surface: csp-view/src/map.rs:1701 and :1963.
- No lead logic anywhere: rg for ignition/half-burn/burn-lead in csp-view/csp-ui returns only
  reliability "ignition misfire" (an unrelated M9b hazard). The auto-planner is also
  uncompensated: `transfer::departure_node` (crates/csp-sim/src/transfer.rs, ~line 761+)
  sets `tick = burn_tick` with no half-burn offset and computes `dv_world = v_post − v_b`
  impulsively at that tick.
- Decisive in-repo corroboration the report missed: `duva_reachable_in_sim_end_to_end`
  (lib.rs:18269) *manually compensates* — lib.rs:18279-18284: "The node is pulled half a burn
  early so the ~0.4 s burn arc centres on t_b — the achieved conic then reads as the impulsive
  burn AT the design point" (`node_tick = inj.t_b.saturating_sub(burn_ticks / 2)`). The test
  author knows the executor trails; players/UI get no such correction.
- Executor agreement test confirmed as claimed: `maneuver_executor_delivers_commanded_dv`
  (lib.rs:17153) burns 100 m/s at 50 m/s² (~2 s) and pins 1e-3 element agreement — the bias is
  invisible at that burn length.
- One numeric correction that STRENGTHENS the finding: the report's illustration ("period
  ~5.8 ks … ~150 s ≈ 9° of arc") is wrong. Terrin: radius 6.000e5 m, mu 3.5316e12
  (data/systems/home/terrin/body.ron:16-17); canonical 75 km LEO (a = 6.75e5) has period
  2π·sqrt(a³/μ) ≈ 1855 s (~1.85 ks), v ≈ 2287 m/s (matches PLAN's M1 row). A 300 s burn whose
  impulse trails by 150 s is ~29° of arc, not 9° — the mismatch is ~3× worse than illustrated.
  The first-order-in-burn-length mechanism is unaffected.

**Evidence**: lib.rs:6698, 6787-6796, 9230-9261, 17153-17203, 18279-18284; maneuver.rs:201-222;
map.rs:1701/1963; transfer.rs `departure_node`; terrin/body.ron. MEDIUM is appropriate
(planning surface systematically wrong exactly for long burns; no crash, player can iterate).

---

### Finding 3: No thrust of any kind off the construction-time dominant body — moon missions are ballistic flybys — CONFIRMED

**Original summary**: SetManeuver, node-start, and ToBubble all require
`orbit.parent == physics.dominant`; `dominant` is fixed at construction, so inside Rask's SOI a
vessel can never thrust, schedule a node, or latch — no orbit insertion, no return burn, no
landing. PLAN.md:1501 sanctions deferring "landable" only; the thrust gap is on no cut list.

**Verification/Reason**:
- Gates verified verbatim: lib.rs:6652-6656 (`apply_set_maneuver`: `if orbit.parent != dominant
  ⇒ Recoverable "maneuver: orbit parent is not the dominant body"`), lib.rs:6779-6783
  (`fire_maneuver_start`: same refusal), lib.rs:6531-6535 (`apply_to_bubble`: same refusal).
  maneuver.rs:37-43 documents the scope honestly in code.
- `dominant` is immutable after construction: bubble.rs:396-411 (`PhysicsConfig`: "data-derived,
  handed in at construction and at `World::restore`"); the only assignments in lib.rs are
  construction/restore sites (2675, 2693, 3463) — rg finds no SetDominant/switch command
  anywhere in the workspace.
- Rask is the v1 moon: PLAN.md:949 ("Solen (star) | Terrin (home) | Rask (moon) | Duva
  (neighbour)"), data/systems/home/rask/body.ron:12 (`parent: Some("terrin")`), and PLAN.md:1561
  ("Minimum shippable v1 = … M5(reduced: home + moon only) … Home planet + moon"). Duva itself
  is post-v1 per §6.4 item 3 (PLAN.md:1550), so Rask is the case that ships — and for Rask there
  is no deferral note at all.
- PLAN.md:1501 (M5 row), quoted verbatim: "*'reachable' demonstrated in-sim (csp-sim
  `duva_reachable_in_sim_end_to_end`: spawn → SetManeuver → executor → Exit{terrin} →
  Enter{duva}); 'landable' deferred per the §6.4 reduced-scope cut (landing needs a
  `dominant=duva` bubble world — tracked for post-v1)*". The note defers "landable" only;
  orbit insertion / powered return at the moon are not mentioned.
- §6.4 cut list (PLAN.md:1543-1558) enumerated in full: R3 render, life support, third body
  (Duva), orbital docking, web tier. Nothing covers thrust-off-dominant. The report's claim
  "not on the §6.4 cut list" is accurate.
- `duva_reachable_in_sim_end_to_end` (lib.rs:18269, test passes) demonstrates exactly the
  sanctioned bar: node + burn at home, ballistic rails to the target. Player-facing refusal
  confirmed at csp-view/src/lib.rs:2646: `self.flight_notice("VESSEL NOT RECOVERABLE — NOT ON
  RAILS HERE".to_string())` behind `to_bubble_eligible`.

**Evidence**: lib.rs:6531-6535, 6652-6656, 6779-6783; bubble.rs:396-422; maneuver.rs:37-43;
PLAN.md:949, 1501, 1543-1562; rask/body.ron:12; csp-view/src/lib.rs:2646/2688. MEDIUM is
appropriate (playable-but-narrow v1 scope exceeding the sanctioned deferral; a design/scope
gap, not a crash).

---

### Finding 4: §2.8 item 4's "log the elements" on solver non-convergence never happens — CONFIRMED

**Original summary**: orbit.rs:48-52's doc claims "the elements are logged at the call site";
PLAN.md:664-665 requires "log the elements"; every caller pushes only the `&'static str`
error onto `degraded`, so the doc claim is aspirational.

**Verification/Reason**:
- PLAN.md:663-665, item 4, quoted: "Iteration cap 64. On non-convergence: return
  `SimError::Recoverable`, **log the elements**, and fall back to the previous valid state for
  that tick. Never silently return garbage."
- orbit.rs:48-52: "Non-convergence is `SimError::Recoverable` — the caller keeps the previous
  valid state and logs the failing [`OrbitState`]'s elements ([`SimError`] payloads are
  `&'static str` by design, so the elements are logged at the call site rather than baked into
  the error)." The doc claim is exactly as reported.
- Actual caller `advance_orbits` (lib.rs:7738-7761): `Err(e @ SimError::Fatal(_)) => return
  Err(e), Err(e) => degraded.push(e)` — only the error (whose payload is the fixed string
  "kepler solve: iteration cap (64) exceeded", orbit.rs:531) is pushed. rg for element-logging
  at `state_at` call sites in lib.rs: none. The fallback half is sound — `advance_orbits`
  mutates nothing, so the epoch-anchored state survives on Err.

**Evidence**: orbit.rs:48-52, 531; lib.rs:7738-7761; PLAN.md:663-665. LOW severity is right
(diagnostic gap on a should-never-happen path; mechanism otherwise correct).

---

### Finding 5: align_rotation's near-aligned shortcut leaves up to 1.4e-3 rad of pointing slop — CONFIRMED

**Original summary**: maneuver.rs:259-261 `cos > 0.999_999 ⇒ IDENTITY` caps executor attitude
error at ~0.081° (~1.4 m/s transverse per 1 km/s burn); the exact branch is no more expensive;
csp-bench inherits it "via the same helper".

**Verification/Reason**:
- maneuver.rs:257-276 verified: the shortcut at 259-261 (`if cos > 0.999_999 { return
  DQuat::IDENTITY; }`). acos(0.999999) = 1.4142e-3 rad = 0.0810°; on a 1 km/s burn the
  transverse component is 1000·sin(1.4142e-3) ≈ 1.41 m/s (0.14%). Arithmetic checks out.
- lib.rs:9251 confirmed: `body.rot = maneuver::align_rotation(f_local / f_max, burn.burn_dir);`
  wholesale per tick — bounded but systematic whenever the pair lands in the band. The exact
  axis-angle branch is one cross product + one libm::acos — no measurable saving at one call
  per tick. Suggestion (tighten to ~1 − 1e-12, where the residual angle is ≤1e-6 rad) is sound.
- One wording correction: csp-bench does NOT call maneuver::align_rotation — its scripted
  controller has its own duplicate, `thrust_attitude` (crates/csp-bench/src/scenarios.rs:309-325),
  with the identical `cos > 0.999_999` shortcut. So "inherited via the same helper" should read
  "inherited via the duplicated convention (same threshold, same slop)". The finding's
  substance and magnitude are unaffected.

**Evidence**: maneuver.rs:257-276; lib.rs:9251; csp-bench/src/scenarios.rs:309-325. LOW/nit is
appropriate.

---

## Deep-verify item (a): scan_strides consumers — verdict CONFIRMED as reported

Workspace-wide rg (`-uu`, target/ excluded) yields exactly: orbit.rs (definition at 1293 + doc
links + 4 test-internal uses) and lib.rs:14274 (inside `#[test] fn
publish_spend_is_physical_and_bounded`, lib.rs:14248). No app/view/ui/server/bench caller; no
non-Rust reference outside .review reports. PLAN.md:584-587 item 4 requires "`h` is reported in
the debug overlay" (quoted under Finding 1). The guarantee "never misses a residence longer
than h" is therefore unobservable in the shipped game — HIGH stands.

## Deep-verify item (b): the two MEDIUMs — both CONFIRMED

Finding 2: mechanism, code sites, missing lead, map consumption, planner non-compensation, and
the in-repo half-burn-early test workaround all verified (see above); one illustration error
(LEO period 1.85 ks, not 5.8 ks → ~29° arc error for a 300 s burn, i.e. worse than reported).
Finding 3: gates, immutability of `dominant`, Rask-as-v1-moon, and the PLAN/§6.4 boundary all
verified with verbatim quotes (see above): PLAN.md:1501 defers "landable" only; §6.4
(1543-1558) lists five cuts, none of which is thrust-off-dominant.

## Deep-verify item (c): clean verdicts spot-checks

1. **Solver round-trip ≤1e-6**: test grid verified as spec-literal —
   `elements_round_trip_at_epoch` (orbit.rs:1598) and `round_trip_property_e_dt_sweep`
   (orbit.rs:1629): e ∈ {0, 0.5, 0.9, 0.999, 1.0, 1.001, 3.0} × Δt 1e-3…1e9 s (12 decades),
   1e-6 pos+vel budget; `stumpff_series_matches_closed_form_at_boundary`,
   `state_at_is_bitwise_pure` also present. Ran `cargo test -p csp-sim --lib orbit::` →
   22 passed / 0 failed. Constants per spec: STUMPFF_EPS 1e-6 (orbit.rs:111),
   LAGUERRE_N = 5.0 (115), SOLVER_MAX_ITERS 64 (113), BRENT_MAX_ITERS 64 (117). The report's
   temporary-probe extremes (e = 1±5e-10) were reverted and cannot be re-run without editing a
   file; the in-grid evidence passes as claimed.
2. **SOI scan/Brent + graze**: stride formula and candidate enumeration shared with
   `scan_strides` (orbit.rs:1243-1305); `brent_root` (orbit.rs:1419) with cap 64 and the
   documented ≤1-tick tolerance; graze pair `soi_graze_residence_above_h_is_detected` (1854) /
   `soi_graze_residence_below_h_is_missed` (1899) — both ran green; sample-budget back-pressure
   test `soi_scan_sample_budget_is_enforced` (2415) green.
3. **Auto-rails floor oscillation-free**: gate verified at lib.rs:4807-4827 — the auto
   bubble→rails edge computes `rp = h²/(μ(1+e))` from the live cartesian state and requires
   `rp > radius + max(atm_top, bubble_radius)` (`if rp.partial_cmp(&floor) != Some(Greater)
   { continue; }`). The edge is one-directional (nothing auto-converts rails→bubble on low
   altitude; rails→bubble happens only via commands/events), and a Keplerian conic's periapsis
   is invariant, so a railed vessel cannot dip back under the floor. bubble.rs:412-417
   documents the §2.3 reinterpretation (bubble_radius as the rail floor with atmosphere-top
   max) exactly as the PRD row states. No oscillation path exists.

## PRD coverage table re-verification

- `§2.8 OrbitState/OrbitModel/PatchedConics — WIRED (advance deliberately absent)`: verified —
  orbit.rs:275-284 documents the partition-independence rationale ("Re-stamping M += n·Δt per
  call is not associative under float partitioning … No code may re-add a per-tick/per-interval
  anomaly re-stamp"). Status accurate.
- `§2.8 solver items 1-3 — WIRED`: verified (constants above; period-reduction and Stumpff
  series branches present). Accurate.
- `§2.8 item 4 — WIRED except log-the-elements`: accurate (Finding 4).
- `§2.8 round-trip — WIRED`: accurate (tests pass; see (c).1).
- `§2.6.6 items 1-3, 5 — WIRED (+ sample-budget extension)`: accurate (see (c).2).
- `§2.6.6 item 4 — DEGRADED (Finding 1)`: accurate.
- `Maneuver nodes/executor — WIRED at dominant, DEGRADED elsewhere (Finding 3)`: accurate.
- `Executor burn timing — PARTIAL (Finding 2)`: accurate.
- `§2.3 bubble radius — REINTERPRETED as rail floor, no oscillation`: accurate (see (c).3);
  the deviation-from-spec note is fair.
- `§2.6.4 surface latch — WIRED`: doc-verified (bubble.rs:3-16: body-fixed `(lat, lon, alt,
  orientation)` record, exact-reproduce-on-unlatch) and 9 bubble tests pass. The
  simple.rs:1322-1324 spin-phase sub-claim was not independently re-derived (out of the
  C06 file set); no contradicting evidence found.
- `State carryover / determinism rows`: verified — materialization takes weld-default
  rot/ang_vel and the executor owns attitude from tick 1 (lib.rs:6803-6810, maneuver.rs:52-56);
  libm-only math in all three files (rg: every sqrt/trig/exp call is `libm::`; non-libm hits
  are comments); BTreeMap candidate order (orbit.rs:1250-1256); `state_at_is_bitwise_pure`
  passes.

## Playability-chain verdict

Spot-checked the one non-obvious link: `finish_burns` (lib.rs:4929-4977) on a failed
`apply_to_rails_at` (e.g. sub-floor post-burn conic) pushes the error to `degraded`,
republishes a `SoiRescan`, and leaves the vessel `InBubble` — the vessel stays physical for
re-entry exactly as the report describes. Chain verdict supported.
