# Verified Report C05 — csp-sim `lib.rs` §D inline test suite (L11881–24461)

Verifier: session-full-20261026, verification agent for review-report-C05. Target: current
working tree. Method: independently re-enumerated the suite and re-ran every search the report's
claims rest on (`RecoverVessel`, `TransferCrew`, `TerminateVessel`, `SetScriptEnabled`,
`wake_tick`, `World::restore`, notification reads, hand-latch/PhysicsConfig literals), then
deep-read each cited test body plus the module suites (economy.rs, research.rs, contracts.rs,
colonies.rs, manufacture.rs, tech.rs, save.rs, scripts.rs) and the eval-scenarios.

Result: **7 confirmed / 0 dismissed** (two findings confirmed with noted count/characterization
corrections that do not change their validity).

---

## Enumeration sanity (method claims)

- `#[test]` count in L11881–24461: **199** (grep and rg agree). `#[ignore]`: **0**. CONFIRMED.
- Module test counts: colonies.rs **43**, contracts.rs **16**, economy.rs **11**, manufacture.rs
  **3**, tech.rs **7**, research.rs **4**, save.rs **12**, life_support.rs **5**, scripts.rs
  **2** — all match the report's inventory exactly. CONFIRMED.
- Minor method-note discrepancy (affects no finding): the report says "every restore-using test
  (33)"; my count is **29 distinct test fns containing `World::restore(`** (50 call sites —
  several tests restore more than once). Direction of every claim is unaffected.

---

### Finding 1: Orbit → science in space → recover never chained; `RecoverVessel` never tested with science aboard — CONFIRMED

**Original summary** [HIGH]: the §3.2 flagship loop is tested only in pieces; `RecoverVessel`
appears in exactly two lib.rs tests, neither with science aboard; economy.rs recovery tests use
`latched_vessel(cost, fuel)` with zero science; no test asserts recovered science is banked.

**Verification/Reason**: all absence claims reproduced exactly.
- `rg 'RecoverVessel' lib.rs` in range → only L20330 and L23682 construct the command (plus doc
  comments at 20305/23643).
- `recover_vessel_banks_and_frees_the_pad` (L20309): craft is `box_craft(1_000.0, …, 0.0)` —
  no experiments, no `science` data; asserts only `funds_micros` increase + vessel removal
  (L20336–20341).
- `gated_commands_reject_non_leaseholders` (L23647): the `RecoverVessel` at L23682 is one of
  three lease-gate probes — asserted rejected (L23694–23699), never applied.
- economy.rs `RecoverVessel` tests (L1277, 1300, 1321, 1339 in `recovery_at_launch_site` :1269,
  `recovery_at_ordinary_site` :1292, `abandon_recovery_pays_quarter` :1314,
  `recovery_rejects_cleanly` :1334): fixture is `latched_vessel(cost, fuel)` (economy.rs:1072) —
  `cost_craft(cost)` + one `liquid_fuel` tank, `crew: Vec::new()`, no science module. Asserts
  funds + site inventory only.
- research.rs's 4 tests (L1274–1330) are closed-form math (`closed_form_*`,
  `softcap_reproduces_plan_sequence`) — no World, no commands.
- Flight tests `deployed_chute_decelerates_and_lands` (L14721) and
  `table_aero_reentry_decelerates` (L15224): no `experiment`/`science`/`Run` strings in their
  bodies.
- `data/eval-scenarios/` (ascent-mission, idle-warp, lease-launch, log-replay, script-ascent):
  `rg -il 'RecoverVessel'` → none; `rg -il 'RunExperiment'` → none. No scenario chains it.
- Chain absence follows by construction: any launch→orbit→science→recover chain needs a
  `RecoverVessel` (or at minimum a science-banking recovery) that lands in space aboard a flown
  vessel; the only two lib.rs uses are funds-only and rejected-outright.

The suggestion's feasibility anchor also checks out: `auto_rails_converts_a_coasting_orbiter`
(L20046) contains the `v_circ = sqrt(3.5316e12 / hi.norm())` + `SpawnPhysicalVessel` trick
(L20062–20072).

### Finding 2: In-flight `TransferCrew` + source-vessel loss + save/load unguarded — CONFIRMED

**Original summary** [MEDIUM]: `TransferCrew` in exactly two tests, neither removes a vessel
mid-transfer nor snapshots mid-window; `save_load_mid_transfer` exists for resources only;
`TerminateVessel` used in exactly one pad-occupancy test.

**Verification/Reason**:
- `rg 'TransferCrew'` across crates/csp-sim/src → in the test range only L20840/L20859
  (`crew_board_and_transfer`) and L21002 (`roster_crew_transfer_reseats_location`). Zero
  occurrences in colonies.rs or any other module (colony crew moves go through other verbs).
- `crew_board_and_transfer` (L20803): happy path stepped the full 640 ticks (L20848), then a
  full-seat rejection (L20856–20866). No snapshot, no vessel loss.
- `roster_crew_transfer_reseats_location` (L20992): full 640 ticks first (L21010), and only
  then snapshot→restore→hash (L21021–21023). No mid-window snapshot, no source/destination loss.
- `save_load_mid_transfer` (L20715): `SimCommand::TransferResource` (L20729) — resources only;
  snapshots at tick+96 of a 192-tick pipe (L20738–20739). The §7.5-shaped harness exists and was
  never applied to crew, exactly as claimed.
- `TerminateVessel` in range: L20276 (success) and L20296 (unknown-id rejection), both inside
  `terminate_vessel_frees_the_pad` (L20247) — pad occupancy/spawn gate only; craft is an
  experiment-less `box_craft`, no crew boarded, no transfers, no scripts.
- Loss-path tests (`roster_jettison_marks_lost` :21029, `roster_breakup_…` :21061,
  `roster_removal_marks_lost` :21140) all lose vessels with crew seated statically — never with
  a 640-tick transfer in flight.
- C04 gate evidence verified at the cited site: lib.rs:9952–9967 — restore `Fatal`s
  "in-flight crew is not seated at the transfer's source" unless the crew record is still on
  `t.from`; `TerminateVessel`/breakup reseat is the poison producer.

### Finding 3: Disable/strike a script with a pending `ScriptWake`, then save — unguarded — CONFIRMED

**Original summary** [MEDIUM]: both disable tests run wake-free scripts; the wake tests never
disable; no test saves after disable-with-pending-wake.

**Verification/Reason**:
- `disabled_script_releases_lease` (L23505): source is `"fn on_tick(ctx) {}"` (L23521) — arms
  nothing; test ends at the lease assertion (L23550), no `snapshot()`.
- `strike_disable_releases_lease` (L23584): `FaultEngine` (L23556) whose `run_tick` always
  returns `Runtime("boom")` — never `request_wake`. No snapshot.
- `SetScriptEnabled` in the range appears in exactly two tests: L23539 (applied, above) and
  L23788 (`external_cannot_unload_lease_holding_script` — asserted *rejected*, L23793–23797,
  so no state change). Neither has a wake live.
- Wake tests (`script_wake_fires_and_runs_extra_tick` :22773, `in_bubble_at_tick_wake_delivers_tick_once`
  :22801, `wake_issued_command_stamps_next_tick` :22818 — including the manual plant technique
  at L22852–22861, `rails_loaded_script_gets_a_bootstrap_tick` :23083,
  `bubble_to_rails_rearms_scripts` :23132): none disable/strike/unload the waked script and
  none snapshot post-disable. `script_persist_survives_snapshot` (L22733) uses
  `Behavior::WritePersist` on an enabled script — no wake, no disable.
- `unload_script_releases_lease` (L~23466) and `external_cannot_unload_lease_holding_script`
  (L23748): both load the same wake-free `"fn on_tick(ctx) {}"` source.
- scripts.rs's 2 tests are `elliptic_apsis_resolution` / `hyperbolic_apsis_resolution` — pure
  conic math, unrelated.
- `wake_tick` reads in the range: only the wake tests above, plus crafted-save fixtures with
  `wake_tick: None` (L13630/13649/13691). No test combines `wake_tick == Some(t)` with a
  disable verb and a save.
- C11 gate evidence verified: lib.rs:3332–3342 — restore `Fatal`s
  "snapshot: script wake / pending event mismatch" on any `wake_tick` ↔ pending-event pairing
  break.

### Finding 4: `RecoverVessel` with a live research project — half-unguarded C08 class — CONFIRMED

**Original summary** [MEDIUM]: the suite pins the `RecoverData` refusal gate
(`recover_rejects_while_project_holds_key`) but never runs `RecoverVessel` against a vessel
with a live `StartResearch` project; no test observes project state after `RecoverVessel`.

**Verification/Reason**:
- `recover_rejects_while_project_holds_key` (L21849): both commands are `RecoverData`
  (L21883 and L21912); the refusal message "recover: a live project still holds the key"
  asserted at L21893; StopResearch remnant return + post-close round-trip at L21900–21934.
  Exactly as the report describes.
- `RecoverVessel` total population (from Finding 1's enumeration): 2 lib.rs tests
  (funds-only box craft; lease-rejected) + 4 economy.rs tests (`latched_vessel`, no research).
  By construction no `RecoverVessel` ever runs with a live project, and no test reads
  `w.science().projects` after a `RecoverVessel`.
- The code seam is real: economy.rs `apply_recover_vessel` banks pooled `v.science.data`
	(credit_science_payout at economy.rs:971 within the ~880–1010 block) then calls
  `remove_vessel`, and `drop_vessel_projects` exists at research.rs:572 — the escrow-drop
  path C08 verified has no tripwire.

### Finding 5: §7.5 three-way gaps — five restore-without-hash tests; notification log never restore-asserted — CONFIRMED (with one characterization correction)

**Original summary** [MEDIUM]: five behavior tests snapshot+restore but stop before the hash;
the notification log is serialized per §7.2 but never asserted to survive snapshot/restore;
`save.rs`'s round-trip is a bytes/codec test.

**Verification/Reason**:
- Four of the five verified as snapshot+restore-`unwrap()` with no hash comparison:
  `latch_absorb_drops_bound_autopilot_and_restores` (restore at L18896–18897),
  `roster_jettison_marks_lost` (L21053–21054), `roster_breakup_reseats_fragment_and_marks_lost`
  (L21114–21115), `recover_rejects_while_project_holds_key` (L21924–21934). Each contrasts with
  the file's own convention (e.g. L21021–21023, L20744, L23844's `lease_is_serialized_and_hashed`).
- **Correction (does not flip the verdict, but the fix list needs it)**: the fifth,
  `closed_key_refuses_new_banking` (L21942), restores and asserts `is_err()` **by design**
  (L22011–22023: injected pooled-data-under-closed-key must fail the restore gate). The
  suggested one-line hash-equality assert cannot apply to this test; the §7.5 hash gap is
  genuine for the other four only. Report should drop :21942 from the "add the hash line" list.
- Notifications: serialized state confirmed — snapshot field `notifications: VecDeque<…>`
  (lib.rs:2418), written `self.notifications.clone()` (L3597), restored
  `snapshot.notifications` (L3480). Test reads: L20080 (`auto_rails_converts_a_coasting_orbiter`
  — OnRails notice), L22197 (`dormant_failure_publishes_fires_silently_and_records` — the
  report's "22170" is the test's fn line, the read is at 22197), L22233 (empty-check),
  L22432 (Explosion count). None of those tests performs a snapshot→restore, and
  `theta_survives_snapshot_restore_and_revert` (L22237) — the one nearby restore test — never
  touches notifications. No test anywhere asserts notification content/order post-restore.
- `save_round_trip_current` (save.rs:618): hand-built `Save` with synthetic
  `state_hash: Some([0xCD; 32])` through encode/decode/inspect — a container-codec test; no
  `World::restore` in save.rs at all. Confirmed.

### Finding 6: Colonies/contracts/manufacture/tech — module-only coverage, seams untested — CONFIRMED

**Original summary** [LOW]: no claim of zero coverage; the four subsystems have zero command
presence in the lib.rs integration suite; the cross-subsystem seams are untested everywhere.

**Verification/Reason**:
- Command absence in range: `AcceptContract`, `FoundColony`, `ManufactureCraft`,
  `JoinColonyInventory` — zero occurrences in L11881–24461 (the only colony strings are
  crafted-save fixtures: `crafted_colony` L13858, `ColonyId(0)` inserts L14010–14104, matching
  the report's "L14019-area"). `Subsystem::Colonies/Manufacturing/Contracts` entries at
  L12458/12461/12464 (report's "L12451-area"). `spawn_physical_refuses_occupied_site` at
  L19984 — the only occupied-site gate test, spawn-side, as claimed.
- Module citations all verified: contracts.rs `AcceptContract` L1414,
  `accept_pays_advance_and_arms_deadline` :1424, `satellite_completes_on_orbit_achieved` :1458,
  `snapshot_round_trip_and_restore_gate` :1562; colonies.rs `FoundColony` :2563 (also :3200),
  `serialization_hash_determinism` :3249, `colony_to_vessel_transfer_credits_at_completion`
  :4527; manufacture.rs tests at :547/582/641 (report's 548–642); tech.rs incl. restore+hash
  (:752–761, `state_hash_covers_tech` :767).
- Seam absence (berth-collision delivery, resupply chain + `JoinColonyInventory` + life
  support, contract completion mid-recovery): no test constructs any of these — follows from
  the command-population zeros above plus no `JoinColonyInventory` use in life_support.rs's
  5 tests (checked: none reference colony inventory commands).

### Finding 7 (DRY): hand-latch and restore-config literals repeated — CONFIRMED (count nuance)

**Original summary** [LOW]: the `SurfaceLatched(SurfacePose { .. })` hand-latch literal 11
times; the `Some(PhysicsConfig { test_env(false), "p", DEFAULT_BUBBLE_RADIUS_M })` restore
block 21 times; `save_load_mid_transfer` already factors a `physics()` closure.

**Verification/Reason**:
- Hand-latch literal `VesselState::SurfaceLatched(SurfacePose {` — exactly **11** occurrences
  in range. Matches the report exactly.
- Restore-physics blocks: **23** total occurrences of the `bubble_radius_m:
  DEFAULT_BUBBLE_RADIUS_M` block in range — **5 closure-factored** (`let physics = ||`/`let
  physics =`, incl. L20716 and L20763) and **18 inline**; the inline set is not perfectly
  uniform (L13147 uses `test_env(true)`, L14406 `test_env(spin)`, L14998 a local `env`,
  L18207 a different dominant), so the strictly-identical inline count is ~14–16, not 21. The
  report's "21" is approximate — the substance (heavy verbatim repetition; the factoring
  pattern exists at L20716 and covers only 5 of 23 sites) is verified. Suggested helpers
  remain valid; a shared `restore_physics()` would want an env parameter to absorb the
  `true`/`spin` variants.

---

## PRD coverage table spot-check

- "§3.2/§8.5 science-recovery-via-RecoverVessel = no" and "launch→orbit→science→recover
  chain = no": verified via Finding 1's enumeration (only 2 lib.rs `RecoverVessel` uses;
  economy/research modules add no science-bearing recovery). CONFIRMED.
- "§7.5 strong-where-applied": the four cited §7.5 rows all check out —
  `snapshot_restore_roundtrips_vessels_and_events` L13042 (both worlds step 20_000 then
  hash-equal), `snapshot_restore_mid_flight_and_latched` L15953,
  `save_load_mid_transfer` L20715, `snapshot_restore_mid_research` L22029 (fn at 22029 under
  the §7.2/§7.5 doc at 22026). CONFIRMED.
- Row anchors spot-checked and present: `player_alarm_fires_as_clamping_at_its_tick` L12475,
  `warp_command_semantics` L12242, `warp_x1_vs_x1e5_equivalence_10_sim_days` L13135,
  `duva_reachable_in_sim_end_to_end` L18269, `lease_is_serialized_and_hashed` L23844,
  `script_persist_survives_snapshot` L22733, `recover_rejects_while_*` :21764/:21849,
  `transmit_ratio_banks_and_marks_key` :21419. CONFIRMED.

## Verdict

7 confirmed, 0 dismissed. Two corrections carried into the record: Finding 5's fifth test
(`closed_key_refuses_new_banking`, L21942) asserts restore failure by design and must be
dropped from the "add a hash assert" fix list; Finding 7's "21 times" is 23 total blocks
(18 inline, 5 factored, with minor env variants). Neither changes any finding's severity or
validity. The HIGH (Finding 1) and the two MEDIUM poison-save guards (Findings 2, 3) are all
real absences with verified in-code gate sites.
