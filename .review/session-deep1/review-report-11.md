# Cluster C11 Review — `crates/csp-sim/src/lib.rs` inline test module (L9113–17547)

## Verdict

This is a strong suite (~200 tests). The load-bearing determinism claims are
genuinely pinned: `warp_x1_vs_x1e5_equivalence_10_sim_days` (L10112) pins both
run granularities apart (`sa.calls >= TEN_DAYS/64 - 16`, `sb.max_advance > 64`,
`sb.calls*64 < sa.calls`), exercises a non-empty bubble mask (`in_bubble_ids ==
{2}`), and compares masked + unmasked hashes, command logs, and per-run event
machinery counts — it cannot degrade into a vacuous pass. Restore round-trips
are hash-compared *and* continued (not just structural). Rollback is covered on
both channels: `rollback_restores_source_seqs_and_hash` (L9380, Fatal mid-drain,
hash-verified) and twin-world rejection tests (L9876, L17101) that assert
post-rejection state is bitwise identical to a world that never saw the command.
Committed hash goldens exist at the bench layer
(`data/test-fixtures/{trivial,flight-launch}/expected-hash.txt`, verified by
`csp-bench::replay::tests::golden_trivial_round_trips`; `xtask bless-goldens`
refuses silent hash changes), and `csp-bench` scenario tests exercise the *real*
`RhaiHost`/`AscentAutopilot` (`scenarios.rs:180,951`), so the StubEngine stubs
in `m9a` are bridged by real-engine coverage. **No tautological test found** —
every candidate (`publish_spend`, lab closed-form, `predict_maneuver`
comparison) re-derives its expectation through an independent formula or API.

Findings below are real but modest gaps and drift risks.

### [MEDIUM] Finding 1: `install_aero_tables_accepts_and_binds` never accepts or binds — the Stage-2 install determinism gate is dead code under test

**File:** `crates/csp-sim/src/lib.rs:10802-10846`

**Problem:** The test name and docstring promise a round trip ("build sim-side,
install via the command path"), but the body exercises only the
`graph_hash != gh` rejection leg (`bad.graph_hash = 0xDEAD` at L10828, asserted
rejected at L10842). `SimCommand::InstallAeroTables` appears nowhere else in the
crate's tests (verified by grep). Untested branches of `apply_install_aero`
(L5333-5405): the happy path (cache insert + live-body table hot-swap at
L5393-5404), the `tables.mask != mask` rejection (L5350), unknown-vessel /
no-craft / no-physics rejections (L5340-5344, 5372), the `MAX_CONFIG_MASKS` cap
(L5388-5391), and — most important — the `#[cfg(debug_assertions)]`
worker-vs-sim-recompute byte-compare at L5356-5371 (`Fatal("install-aero: table
differs from sim-side recompute")`), which is the §2.4 determinism seam for
off-tick table builds. That gate can never fire because no successful install is
ever attempted. Separately, `SimCommand::SetAeroConfig` (L642,
`apply_set_aero_config` L5409) — the mask selector with its hamming-nearest
fallback at L5458-5467 — has zero command-path coverage; the only mask mutation
in the suite is a direct field write (`overheat_remaps_aero_mask`, L11943).

**Evidence:** L10819-10845 — `build_aero_tables` is invoked with `gh = 0u64`
(already mismatched), then `bad.graph_hash = 0xDEAD` guarantees rejection
regardless; the only assertion is the Recoverable "graph hash mismatch".

**Impact:** The §2.4 Stage-2 contract — a worker-computed table must bit-match
the sim-side recompute — is a load-bearing determinism claim with no test able
to reach it. A regression in `build_aero_tables` determinism, the cache
invalidation path, or the mask fallback would ship green.

**Suggestion:** Extend the test (or add a sibling) to install a correctly-bound
table and assert it lands in `physics.aero`'s cache and, when `mask ==
aero_mask`, on the live body; add a `mask != cmd mask` rejection leg; and issue
a `SetAeroConfig` through `step_frame` asserting the resolved mask.

### [LOW-MEDIUM] Finding 2: `test_env` diverges from the authored Terrin atmosphere; `home_system` duplicates RON data unbound

**File:** `crates/csp-sim/src/lib.rs:10519-10552` (`test_env`),
`13286-13351` (`home_system` + `MU_DUVA`/`DUVA_*` constants)

**Problem:** Two hand-duplicated fixture layers with no binding assertion to the
real loaders:

- `test_env`'s `temperature_knots` has 2 knots (0 m→288.15, 70 km→200.15); the
  authored Terrin at `data/systems/home/terrin/body.ron` has 4 (12 km→216.65,
  40 km→250.35). At ~10 km the mock ambient is ≈276 K vs ≈229 K real; at 20 km
  ≈263 K vs ≈226 K — ~40-47 K warm. This is load-bearing, not cosmetic:
  `explosion_and_overheat_same_tick_destroys_part` relies on "Ambient at 10 km
  is ~276 K" (L16601) to trip a 250 K `max_temp_k` leaf that would *not* overheat
  under the real profile. All thermal goldens (e.g. the 273.98 K peak at L11702)
  pin the mock's ambient, not the shipped one. Tunables
  (`settle_v/settle_w/settle_ticks/friction_mu/drag_cd`) happen to equal
  `DEFAULT_*` today, but nothing asserts
  `test_env(spin) == Environment::for_body(terrin_def, ZERO, Some(21600))` — the
  real construction path (`csp-phys/src/simple.rs:308`).
- `home_system()` re-types the `data/systems/home` constants. I verified they
  currently match (`MU_DUVA 3.0137e11`, `DUVA_SOI 4.79e7`, `DUVA_A 2.07e10`,
  `DUVA_E 0.051`, `DUVA_I`, `TERRIN_A/SOI/R`, `RASK_*`, `MU_SOLEN 1.1723e18`),
  and `orbit::tests::from_body_defs_matches_authored_home_system` (orbit.rs:2124)
  pins `from_body_defs` to the RON — but nothing pins `home_system` to either,
  so the flagship end-to-end test (`duva_reachable_in_sim_end_to_end`) silently
  drifts if the data changes.

**Impact:** The thermal suite and the Duva reachability proof run on a parallel
universe that currently agrees with shipping data but is not forced to keep
agreeing. A `body.ron`/`DEFAULT_*` edit would leave every test green while the
fixture no longer represents production conditions.

**Suggestion:** One cheap binding test: assert `test_env(spin)` equals
`Environment::for_body` of the loaded Terrin `BodyDef` (or explicitly document +
assert the intended deltas), and assert `home_system() ==
PatchedConics::from_body_defs(&loaded_defs)`.

### [LOW] Finding 3: `second_dock_records_a_second_seam_on_one_body` performs no second dock

**File:** `crates/csp-sim/src/lib.rs:14729-14749`

**Problem:** Name and docstring claim "a second dock adds a second seam — one
vessel, one WeldedBody, two recorded seams." The body performs exactly one
capture-merge, asserts `seams.len() == 1` at L14735, then — after a comment
admitting "the single-port boxes can't re-capture, so … Simpler honest check" —
asserts only `graph.nodes.len() == 2` and `part_index.len() == 2`: the same
facts `settled_capture_merges_into_one_body_with_seam` already proves. A second
seam is never recorded through the live path in this test. Second-seam mechanics
are covered only via crafted `latch_now` calls in
`undock_middle_seam_splits_chain_and_partitions_seams` (L14635) — the *live*
second-capture path (port re-derivation on an already-merged craft, second
seam-record assignment) is unexercised.

**Impact:** Low — the seam-merge code is otherwise well covered, but a reader
auditing coverage against the §2.4 station claim will credit this test for a
case it does not run.

**Suggestion:** Rename to reflect what it asserts (e.g. `merged_body_is_one_weld`),
or build a two-port secondary craft fixture to stage the real second dock.

### [LOW] Finding 4: `theta_survives_snapshot_restore_and_revert` restores against an empty orbit model, then hot-swaps `w2.orbits`

**File:** `crates/csp-sim/src/lib.rs:16458-16460`

**Problem:** The snapshot was taken on `World::with_system(test_system())` with
an `OnRails` vessel parented to `"p"`, but `World::restore` is called with
`PatchedConics::new(BTreeMap::new())` — a model containing no bodies — and the
test then patches `w2.orbits = test_system()` directly. This passes because
restore performs no `vessel.orbit.parent ∈ orbits.bodies` coherence check:
`restore_vessel_state` (L8088) takes no model and the restore walk
(L2298-~2730) never inspects parents. The crafted-garbage suite
(`restore_rejects_crafted_snapshots`, L10341) likewise crafts an OnRails vessel
parented to `"p"` against an empty model (L10416-10419) and never exercises the
gap — its expected `Fatal` fires on `next_vessel_id` instead.

**Impact:** If "OnRails parent ∈ supplied model" is a restore invariant, it is
neither enforced nor pinned — a corrupt save can produce a world whose vessel
references a nonexistent body, surfacing later as an `state_at` failure. If it
is deliberately a caller-error precondition, the test nonetheless codifies a
post-restore model hot-swap production never performs and documents the leniency
only implicitly.

**Suggestion:** Decide the invariant: either add the parent check to restore and
a crafted-snapshot leg pinning it, or give `theta_survives` the real model
(`World::restore(test_system(), …)`) so the test exercises a coherent input.

### [MINOR] Finding 5: stale comment + dead code in m9a

**File:** `crates/csp-sim/src/lib.rs:16950-16952`, `16915-16916`

**Problem:** `script_wake_fires_and_runs_extra_tick` contains contradictory
stale lines — "First run at tick 2 registers the wake → pending `ScriptWake` at
66" immediately followed by the corrected "First run at tick 1 registers the
wake at 65" (assertion uses 65). `script_persist_survives_snapshot` has dead
code: `let orbits = PatchedConics::new(BTreeMap::new()); let _ = orbits;`.

**Impact:** Cosmetic; slightly misleading about which tick stamps the wake.

**Suggestion:** Delete the "at 66" line and the dead binding.

## Checked and cleared

- Equivalence test: non-vacuous granularity + non-empty mask + machinery counts —
  real §2.6.1 pin.
- Restore round-trips: hash equality plus continuation stepping across
  mid-flight, latched, mid-transfer, mid-research, refused-pair, seam-record,
  maneuver-node, script-persist, and θ cases.
- Rejection/rollback: twin-world hash equality on command rejection; crafted
  `Fatal` suite on every heap invariant; `degraded` event path; lease,
  tracking, transfer, science, spawn rejections all typed `Recoverable`.
- Golden coverage: committed `expected-hash.txt` files verified in
  `csp-bench` tests; the `DEFAULT_*`↔`balance.ron` config mirror is pinned by
  `csp-bench::replay::tests` (L1218-1250), so test-vs-production tunable drift
  is covered externally.
- No `#[ignore]`, `todo!`, or `unimplemented!` in the module; `cargo test -p
  csp-sim --lib -- --list` enumerates 283 tests cleanly.
