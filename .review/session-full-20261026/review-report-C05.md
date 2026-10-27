# Review Report C05 — csp-sim `lib.rs` §D inline test suite (L11881–24461)

Session: session-full-20261026. Target: current working tree. Scope: `crates/csp-sim/src/lib.rs`
L11881–24461 ONLY (test code). PRD anchors: §8 (test strategy), §7.5 (snapshot↔hash↔restore),
§3.2/§3.5/§3.6 (player flows), §8.5 (sim e2e).

Method: enumerated all 199 `#[test]` fns (0 `#[ignore]`d), classified by feature, deep-read ~35%
prioritizing the new player surface (auto-rails, recover/terminate, manufacture, save/revert,
colonies/crew/contracts, alarms, scripts) and every restore-using test (33). Module-level suites
in the same crate (`colonies.rs` 43, `contracts.rs` 16, `economy.rs` 11, `manufacture.rs` 3,
`tech.rs` 7, `research.rs` 4, `save.rs` 12, `life_support.rs` 5, `scripts.rs` 2) were inventoried
so "zero coverage" claims are made honestly — several drive the command surface directly.

---

### [SEVERITY: HIGH] Finding 1: The flagship player loop — orbit → science in space → recover — is never tested as a chain, and `RecoverVessel` is never tested with science aboard
**File**: crates/csp-sim/src/lib.rs:20309 (and absence across L11881–24461)
**Problem**: The suite tests every piece of the §3.2 science economy in isolation but never the
loop the PRD is built around. `RecoverVessel` appears in exactly two tests: the funds/pad test
(`recover_vessel_banks_and_frees_the_pad`, L20309 — craft is `box_craft`, no experiments, no
data, asserts only `funds_micros`) and as a lease-gating example in
`gated_commands_reject_non_leaseholders` (~L23682). No test in this suite asserts that recovered
*science* is banked (`credit_science_payout`'s edge), and `economy.rs`'s recovery tests
(`recovery_at_launch_site` economy.rs:1269, `recovery_at_ordinary_site` :1292) use
`latched_vessel(cost, fuel)` — part value + tank only, zero science.
**Evidence**: `grep RecoverVessel` over L11881–24461 → 2 uses; science tests
(`run_experiment_*`, `transmit_ratio_banks_*`, `recover_rejects_while_*`) all exercise
`RecoverData`/`TransmitData` on hand-latched vessels that never flew; flight tests
(`deployed_chute_decelerates_and_lands`, `table_aero_reentry_decelerates`) never carry
experiments. The csp-bench eval-scenarios (`data/eval-scenarios/`: ascent-mission, idle-warp,
lease-launch, log-replay, script-ascent) don't chain it either.
**Impact**: PRD §3.2's "collect → transmit/recover → funds" loop and §8.5's "launch→orbit→land"
sim-e2e row are unprotected at the integration level; this is precisely the seam where the
verified C08 bug (RecoverVessel silently drops lab escrow) shipped unnoticed.
**Suggestion**: One chained test in this suite: spawn fueled craft → burn/`SpawnPhysicalVessel`
with clearing conic (reuse `auto_rails_converts_a_coasting_orbiter`'s v_circ trick) →
`RunExperiment` in space → `TransmitData` a fraction → land (`SurfaceLatched`) →
`RecoverVessel{site: Some(pad)}` → assert (a) part value AND banked-science funds credited,
(b) the science key closed, (c) crew returned `Unassigned`, (d) snapshot→restore→N ticks
hash-equal. This single test would have caught C08 and guards the whole §3.2 recovery edge.

### [SEVERITY: MEDIUM] Finding 2: Verified C04 class unguarded — in-flight `TransferCrew` + source-vessel loss + save/load (poison save)
**File**: crates/csp-sim/src/lib.rs:20803, 20992 (and absence across the range)
**Problem**: `TransferCrew` is used in exactly two tests. `crew_board_and_transfer` (L20803)
runs the happy path to completion and a full-seat rejection; `roster_crew_transfer_reseats_location`
(L20992) steps the full 640 ticks and only then does snapshot→restore→hash. No test removes the
*source* (or destination) vessel while the 640-tick transfer is in flight, and no test snapshots
mid-transfer-window for crew (the resource analogue `save_load_mid_transfer` L20715 does exactly
this for tanks — the shape exists, it was never applied to crew).
**Evidence**: C04's verified poison: `TerminateVessel`/breakup reseat breaks the
"crew still seated at transfer source" restore invariant (lib.rs:9952–9967 gate); snapshot
accepts, restore Fatals → permanently unloadable save. `TerminateVessel` itself is used in
exactly one test (`terminate_vessel_frees_the_pad`, L20247 — pad occupancy only, no crew, no
transfers, no science, no scripts attached to the terminated vessel).
**Impact**: A repeat of the C04 class (any new producer of source loss — breakup, crash,
jettison of the source part) reds nothing until a player's save dies.
**Suggestion**: `TransferCrew A→B`; advance ~100 of 640 ticks; `TerminateVessel{A}`; advance past
completion; `snapshot()` then `World::restore` must succeed (today: Fatal); then step both worlds
to a common target and `assert_eq!(hashes)`. A second variant snapshotting mid-window (before any
loss) pins the §7.5 mid-transfer state for crew the way L20715 does for resources.

### [SEVERITY: MEDIUM] Finding 3: Verified C11 class unguarded — `SetScriptEnabled{false}` with a pending `ScriptWake` (stranded wake → poison save)
**File**: crates/csp-sim/src/lib.rs:23505 (and absence across the script tests L22674–23196)
**Problem**: `disabled_script_releases_lease` (L23505) disables a script whose source is
`"fn on_tick(ctx) {}"` — no wake armed. The wake machinery is well tested on its own
(`script_wake_fires_and_runs_extra_tick` L22773, `wake_issued_command_stamps_next_tick` L22818 —
which even demonstrates the manual plant technique), but no test disables (or strike-disables,
or unloads) a script *while `wake_tick == Some(t)` and the paired `ScriptWake` event is pending*,
and then saves.
**Evidence**: C11's verified poison: disable paths break the `wake_tick` ↔ pending-event pairing;
snapshot writes it, restore Fatals ("script wake / pending event mismatch", lib.rs:3332–3342)
and every later save is unloadable. `strike_disable_releases_lease` (L23584) uses `FaultEngine`
with no `AtTick` wake either; `script_persist_survives_snapshot` (L22733) persists an enabled
script only.
**Impact**: Same class as Finding 2 — a legal UI verb (AUTOMATION panel's disable toggle, now
player-reachable per program.rs:715–730) can brick saves with zero test signal.
**Suggestion**: `scripted_world(Behavior::Wake(64))`; step 1 (wake registered at 65, pairing
live); `SetScriptEnabled{false}`; step past 65; assert either the wake was cancelled
(`wake_tick == None` and event dropped) or that it fires inertly; then `snapshot()` →
`World::restore` must succeed and post-restore N-tick hashes must match the uninterrupted run.
Repeat for the strike-disable path (`FaultEngine` + planted wake).

### [SEVERITY: MEDIUM] Finding 4: Verified C08 class half-unguarded — `RecoverVessel` with a live research project (escrow dropped)
**File**: crates/csp-sim/src/lib.rs:21849 (RecoverData-only gate), 20309 (funds-only recover)
**Problem**: The suite pins that `RecoverData` *refuses* while a lab project holds the key
(`recover_rejects_while_project_holds_key`, L21849 — command at L21883/L21912 is `RecoverData`),
and that `StopResearch` returns the remnant to the pool. But `RecoverVessel` — which removes the
whole vessel — is never run against a vessel with a live `StartResearch` project, so the
similarity/refusal semantics for the whole-vessel door are untested.
**Evidence**: C08 verified: `apply_recover_vessel` (economy.rs:880–1010) banks pooled
`v.science.data` then `remove_vessel` → `drop_vessel_projects` silently destroys escrowed units
and accrued science. No test observes project state after `RecoverVessel`.
**Impact**: The exact verified bug class has no tripwire; a regression re-introducing it (or the
mirror-image: making `RecoverVessel` refuse like `RecoverData` without UI notice) is invisible.
**Suggestion**: Variant of L21849 with `RecoverVessel{site: None}` instead of `RecoverData`:
assert either a typed refusal (parity with the RecoverData gate, `"a live project still holds
the key"`) or full escrow return + key closure + funds credit — whichever the PRD §3.2 semantics
decide — plus snapshot/restore round-trip of the post-recover world.

### [SEVERITY: MEDIUM] Finding 5: §7.5 three-way gaps — five behavior tests restore without hash-comparing, and the notification log (a §7.2 serialized arena, W2.6-grown) is never asserted to survive save/load
**File**: crates/csp-sim/src/lib.rs:18866, 21029, 21061, 21849, 21942; notification reads at 20081/22170
**Problem**: The suite's own convention (e.g. `roster_crew_transfer_reseats_location` L21023,
`lease_is_serialized_and_hashed` L23844, `save_load_mid_transfer` L20744) is snapshot → restore →
`assert_eq!(state_hash)`. Five behavior tests snapshot+restore but stop before the hash:
`latch_absorb_drops_bound_autopilot_and_restores` (L18866), `roster_jettison_marks_lost`
(L21029), `roster_breakup_reseats_fragment_and_marks_lost` (L21061),
`recover_rejects_while_project_holds_key` (L21849), `closed_key_refuses_new_banking` (L21942).
A restore that silently drops a hashed field passes these as long as the asserted field survives.
Separately, the notification log — serialized per §7.2 incl. recorded dormant failures, widened
for W2.6 HUD toasts — is read in tests (`auto_rails_converts_a_coasting_orbiter` L20081,
`dormant_failure_publishes_fires_silently_and_records` L22170) but never asserted present after
`snapshot`/`restore`; `save.rs`'s `save_round_trip_current` round-trips bytes without content
assertions.
**Impact**: §7.5's "each failure is a real bug — usually a cache that is a function of history"
protection is only as wide as the states it is applied to; the crew/economy/pairing states above
are the newest and least covered.
**Suggestion**: Add the one-line `assert_eq!(r.state_hash().unwrap(), w.state_hash().unwrap())`
to the five (matches the file's own convention); add one test asserting notification entries
(dormant failure record + OnRails notice) survive `snapshot`→`World::restore` with content and
order intact.

### [SEVERITY: LOW] Finding 6: Colonies, contracts, manufacture, and tech have zero presence in the integration suite — covered only at module level, and the cross-subsystem seams are untested everywhere
**File**: crates/csp-sim/src/lib.rs:11881–24461 (absence; only mentions are Subsystem-order L12451-area, crafted-save fixtures L14019-area, `Subsystem::Colonies/Contracts/Manufacturing` entries)
**Problem**: Not a "zero coverage" claim: `contracts.rs` drives `AcceptContract` through the
command queue (contracts.rs:1414/1432, incl. `accept_pays_advance_and_arms_deadline` :1424,
`satellite_completes_on_orbit_achieved` :1458, restore+hash :1562), `colonies.rs` drives
`FoundColony` (colonies.rs:2563) and the supply/transfer verbs (incl.
`colony_to_vessel_transfer_credits_at_completion` :4527, `serialization_hash_determinism` :3249),
and `manufacture.rs` covers order-spend/deliver/rejections/restore (:548–642). What no test in
*this* suite (or the modules) covers is the seams between them: e.g. a `ConstructionComplete`
delivery colliding with an occupied berth (only `spawn_physical_refuses_occupied_site` L19984
tests the spawn-side gate), a colony resupply chain riding a landed vessel + `JoinColonyInventory`
+ life support together, or contract completion firing while the completing vessel is mid-recovery.
**Impact**: PRD §3.5.1's found→supply→produce loop and §8.5's "colony supply loop" scenario are
protected only per-module; seam regressions (the shape of most Phase1–4 bugs) have no tripwire.
**Suggestion**: One lib.rs-level seam test per new subsystem pair is enough: manufacture delivery
to a busy pad (retry/queue behavior), and vessel↔colony transfer completing across a snapshot.

### [SEVERITY: LOW] Finding 7 (DRY, borderline): hand-latch and restore-config literals repeated verbatim
**File**: crates/csp-sim/src/lib.rs:20320, 21793, 21873, 21948 (+6 more)
**Problem**: The 9-line `VesselState::SurfaceLatched(SurfacePose { .. })` hand-latch literal
appears 11 times and the `Some(PhysicsConfig { environment: test_env(false), dominant:
BodyId::from("p"), bubble_radius_m: DEFAULT_BUBBLE_RADIUS_M })` restore block 21 times
(`save_load_mid_transfer` L20716 already factors it into a `physics()` closure — the pattern
exists, it just didn't spread).
**Impact**: Minor; a `SurfacePose` field addition touches 11 sites and invites copy-paste drift
in exactly the tests that guard restore invariants.
**Suggestion**: `fn force_latch(w: &mut World, v: VesselId, alt: f64)` and a `restore_physics()`
helper next to `physics_world()`.

### Tests pinning wrong behavior / ignored tests
None found: 0 `#[ignore]` attributes in the range; no test was observed asserting a behavior the
verified-findings list contradicts (the C08/C09/C04 bugs were *absences* of assertion, not
pinned-wrong assertions — the one pinned-wrong case known suite-wide, the inverted comment in
`contracts.rs:1839`, is in C08's module scope, not this range).

---

## PRD coverage table

| PRD flow (§) | lib.rs test? | Evidence |
|---|---|---|
| Command total order / drain (§2.2.1) | yes | L12033–12265 (incl. strict-total-order proof L12265) |
| Warp as command, ×1↔×10⁵ equivalence (§2.6.1/2.6.3) | yes | `warp_command_semantics` L12242; THE milestone test L13135 |
| Event queue, alarms, back-pressure (§2.6.5, M5) | yes | L12475–12615 (`player_alarm_fires_as_clamping_at_its_tick` L12475) |
| Save refusal rules (§7.4) | yes | `SaveBlocking` asserts at L15921, L18671, +4 more |
| Snapshot↔restore↔hash per-commit shape (§7.5) | mostly | 20k-tick dual-run L13042; mid-flight L15953; mid-resource-transfer L20715; mid-research L22029 — but not mid-crew-transfer, not script-wake-disabled, not notifications (Finding 5) |
| Crafted-save hardening (§7.7 refusal) | yes | L13513–14247 (4 large tests) |
| Auto bubble→rails + coast gating (Phase1) | yes | L20046, L20092 — `active_vessel` across the transition unasserted (C19 was view-side staging, not sim state) |
| Re-entry/parachute/landing physics | yes | L14721, L15224, L15403 |
| Maneuver nodes + burn completion (§2.8) | yes | L17124–17617 (+hash/roundtrip) |
| Transfer windows (M5) | yes | L17660–17745 (hashed+restored) |
| Interplanetary reachability (M5) | yes | `duva_reachable_in_sim_end_to_end` L18269 (a genuine e2e chain — proof the suite can do this shape) |
| Docking/joints/merge (§2.4 M6) | yes | L18475–19543 (20 tests incl. bit-identical remerge L19234) |
| Resource transfers vessel↔vessel | yes | L20439–20758 |
| Crew board/transfer/roster reseat | yes | L20803–21160 (happy + loss paths; mid-transfer save/load missing — Finding 2) |
| Science: run/transmit/lab/closed-key | yes | L21345–22074 |
| Science recovery via `RecoverVessel` (§3.2) | **no** | Finding 1/4 |
| Launch→orbit→science→recover chain (§3.2/§8.5) | **no** | Finding 1 |
| Contracts lifecycle (§3.6) | module-only | `contracts.rs:1424/1458/1562`; zero in lib.rs (Finding 6) |
| Colony found/supply/produce (§3.5.1 M8) | module-only | `colonies.rs:2563/4527/3249`; zero in lib.rs (Finding 6) |
| Manufacture-costed launch (§3.5 M8, W2.6) | module-only | `manufacture.rs:548/642`; zero in lib.rs incl. no berth-collision seam test |
| Economy hire/salary/recovery (§3.5) | module-only | `economy.rs:1269–1352` + lib.rs funds-only L20309 |
| Tech spend/unlock (§3.3) | module-only | `tech.rs` (7 tests incl. restore+hash) |
| Scripting wake/lease/strike (§2.10 M9a) | yes | L22674–23900 (disable-with-pending-wake missing — Finding 3) |
| Fatal rollback discipline (§2.13) | yes | L24014–24166 (4 tests) |
| Fresh-world seeding / NewGame (C24 class) | no (view-layer bug) | guard belongs to csp-view/csp-app suites, not reachable here |
| Warp ×2 ladder (C24 class) / stale pre_launch revert (C22) / auto-rails focus kill (C19) | no | view/app-layer classes — guards belong to C21/C24/C19 suites (Follow-ups) |

## Top 3 issues
1. **Finding 1 (HIGH)** — the flagship science→recover loop has no chained test and
   `RecoverVessel` is never tested with science aboard; one composed test guards §3.2's recovery
   edge and would have caught C08.
2. **Finding 2 (MEDIUM)** — C04's poison-save class (in-flight crew transfer + source loss) has
   no guard despite the exact §7.5-shaped harness (`save_load_mid_transfer`) already existing
   for the resource analogue.
3. **Finding 3 (MEDIUM)** — C11's poison-save class (disable/strike a script with a pending
   wake, then save) has no guard; the disable tests all run wake-free scripts.

Follow-ups (out of cluster scope): view/app-layer guard tests for C22 (revert-point invalidation
on load), C24 (NewGame seeding, warp ×2 ladder), C19 (OnRails vessel staging/focus) belong to the
csp-view/csp-app test suites (C21/C24/C19 clusters); C09's net-conservation colony test belongs
to colonies.rs (C09).
