# Verification — review-report-11.md (Cluster C11: `csp-sim/src/lib.rs` test module)

Verifier notes: every claim was checked against `crates/csp-sim/src/lib.rs`
(17 547 lines), `crates/csp-sim/src/orbit.rs`, `crates/csp-phys/src/simple.rs`,
`crates/csp-phys/src/aero.rs`, `data/systems/home/*/body.ron`,
`crates/csp-bench/src/{replay,scenarios}.rs`, `crates/csp-server/src/backend.rs`,
and `crates/csp-sim/src/save.rs`. All five findings hold; two carry small
nuances noted below.

## Finding 1 — `install_aero_tables_accepts_and_binds` only tests the reject path: **CONFIRMED**

- `install_aero_tables_accepts_and_binds` at `crates/csp-sim/src/lib.rs:10802-10846`
  builds the table with `gh = 0u64` (L10819-10827, already mismatched), then
  overwrites `bad.graph_hash = 0xDEAD` (L10828), and the single assertion is
  `Recoverable("install-aero: graph hash mismatch")` (L10839-10845). No
  successful install is ever attempted.
- `SimCommand::InstallAeroTables` appears nowhere else in the repository —
  `grep -rn "InstallAeroTables\|SetAeroConfig" --include="*.rs" .` outside
  `csp-sim/src/lib.rs` returns zero hits. Inside lib.rs it exists only at the
  enum (L626), dispatch (L4106), doc comment (L5330), and the test (L10832).
- `apply_install_aero` (L5333-5405) is reachable only via the command dispatch
  at L4110 (verified: `apply_install_aero` has exactly 2 occurrences —
  definition + dispatch). The untested branches are exactly as listed:
  unknown-vessel (L5340-5341), no-craft (L5343-5344), `mask != mask` field
  check (L5350-5352), **the `#[cfg(debug_assertions)]` worker-vs-sim byte-compare
  `Fatal` at L5356-5371**, no-physics (L5372-5374), `MAX_CONFIG_MASKS` cap
  (L5388-5391), and the happy path cache-insert + `aero_mask == mask` hot-swap
  (L5393-5404). `grep` for each error string in test code: only
  "graph hash mismatch" is asserted (L10842).
- **Yes — the byte-compare determinism gate is dead under test.** It sits
  behind two reject gates that the only test deliberately trips, and it is the
  sole `bit_eq` comparison on the install path. A `build_aero_tables`
  cross-context divergence would pass every test.
- `SetAeroConfig` (enum L642, dispatch L4111, `apply_set_aero_config` L5409)
  has zero command-path coverage; its hamming-nearest fallback is at
  L5458-5467 as stated. The only `aero_mask` mutation in the suite is the
  direct field write at L11943 in `overheat_remaps_aero_mask` (other
  `aero_mask:` occurrences at L10412, 12333, 14357, 14496, 15425, 16361, 16670
  are `ConfigMask::ALL` in `Vessel` literals — construction, not mutation).

**Nuance (mitigating, does not dismiss):**
- `csp-phys/src/aero.rs:1828-1830` contains a same-context determinism test
  (`a.bit_eq(&b)` on two `build_aero_tables` calls), so the builder's
  within-thread determinism is pinned — what is dead is specifically the
  *cross-context* seam (worker-produced table vs sim-side recompute through
  the command path).
- The lazy `ensure_aero` path (L5268-5328) does exercise cache insert +
  live-body attach in tests (e.g. `overheat_remaps_aero_mask` reads
  `body.aero.as_ref().unwrap()` at L11963), so aero tables function under
  test generally; the *install command* seam specifically does not.
- Minor wording: the test docstring (L10799-10800) does say "reject a
  mismatched graph hash", so it documents the reject leg — it is the test
  *name* (`..._accepts_and_binds`) that over-promises.

## Finding 2 — `test_env` / `home_system` diverge unbound from authored data: **CONFIRMED**

- `test_env` (L10519-10552) has 2 `temperature_knots` (0 m→288.15,
  70 km→200.15); `data/systems/home/terrin/body.ron` has 4 (adds
  12 km→216.65, 40 km→250.35). Interpolation is piecewise-linear
  (`csp-phys/src/simple.rs:344-372`, lerp at L365), so the report's numbers
  reproduce exactly: mock at 10 km ≈ 275.6 K vs authored ≈ 228.6 K; mock at
  20 km ≈ 263.0 K vs authored ≈ 226.3 K.
- **The divergence is load-bearing:** `explosion_and_overheat_same_tick_destroys_part`
  (L16595-16648) spawns at 10 km (`spawn_falling(&mut w, craft, 10_000.0, 2_000.0)`,
  L16616) and relies on the L16601 comment "Ambient at 10 km is ~276 K" to push
  a 250 K `max_temp_k` leaf over limit on the first `thermal_step`. Under the
  authored profile ambient ≈ 229 K — the leaf would *not* overheat from ambient
  alone and the test would fail. The 273.98 K ±0.5 K golden at L11701-11704
  likewise pins the mock's ~263 K ambient at 20 km (comment L11698).
- `test_env`'s tunables (`settle_v 0.05, settle_w 0.05, settle_ticks 128,
  friction_mu 0.5, drag_cd 1.0`, L10546-10550) match `DEFAULT_*` at
  `csp-phys/src/simple.rs:242-257` today; `Environment::for_body`
  (simple.rs:308-328) is the real construction path and nothing asserts
  `test_env == for_body(terrin_def, ZERO, Some(21600))`. Other scalars
  (radius 6e5, μ 3.5316e12, spin 21 600 s, pressure/scale-height/top/molar/
  γ) all match the RON — *only* `temperature_knots` diverges.
- `home_system()` (L13300-13351) constants verified one-for-one against the
  RON files: `MU_DUVA 3.0137e11`, `DUVA_SOI 4.79e7`, `DUVA_A 2.07e10`,
  `DUVA_R 3.2e5`, `RASK_R 2.0e5`, `DUVA_E 0.051`, `DUVA_I
  0.0010471975511965976` all match `duva/body.ron`/`rask/body.ron`; the shared
  constants at L9120-9126 (`MU_SOLEN 1.1723e18`, `MU_TERRIN 3.5316e12`,
  `MU_RASK 6.5138e10`, `TERRIN_SOI 8.4e7`, `TERRIN_A 1.36e10`, `RASK_A 1.2e7`,
  `RASK_SOI 2.43e6`) and `TERRIN_R` (L10638) match too.
- `home_system()` has exactly one call site (L13705,
  `duva_reachable_in_sim_end_to_end`). `from_body_defs_matches_authored_home_system`
  exists at `orbit.rs:2124` and pins `from_body_defs` to the RON (μ, SOI,
  parent, elements) — but nothing compares `home_system` against either, so a
  RON edit leaves the flagship test green on stale constants.

**Nuances (mitigating, do not dismiss):**
- `home_system` uses `period_s: f64::INFINITY` for all three orbiting bodies
  (via `circular_elements`, L9136, and literal L13342) while `from_body_defs`
  carries the authored `period_s` — so the report's suggested literal
  `home_system() == PatchedConics::from_body_defs(&defs)` would *fail today*;
  the binding must be field-wise excluding `period_s` (which the model marks
  "unused") or via `state_at` agreement. Same caveat applies verbatim to the
  `test_env == for_body` suggestion — the report does hedge with "or
  explicitly document + assert the intended deltas", which covers this.
- The authored atmosphere is not wholly untested end-to-end:
  `csp-bench/src/scenarios.rs:154` builds `Environment::for_body(terrin, …,
  terrin.rotation_period_s)` on the real RON. So shipping-atmosphere coverage
  exists at the bench layer; the gap is confined to the csp-sim unit suite
  (whose thermal semantics, e.g. the 250 K leaf, silently depend on the mock).

## Finding 3 — `second_dock_records_a_second_seam_on_one_body` performs no second dock: **CONFIRMED**

- `crates/csp-sim/src/lib.rs:14729-14749`: one `spawn_port_pair` +
  `step_until` capture-merge (identical fixture to
  `settled_capture_merges_into_one_body_with_seam`, L14209), asserts
  `seams.len() == 1` (L14735), then only `graph.nodes.len() == 2` and
  `part_index.len() == 2` (L14747-14748). The comment at L14736-14745 itself
  concedes "the single-port boxes can't re-capture … Simpler honest check".
  No second dock, no second seam record.
- Second-seam mechanics are covered only via crafted `latch_now` calls —
  direct `joints::apply_latch` invocations bypassing joint formation and
  settle (`latch_now` at L14527, used at L14665/L14670 in
  `undock_middle_seam_splits_chain_and_partitions_seams`). The live
  second-capture path (port re-derivation on an already-merged craft, second
  seam-record assignment through the joint→settle→latch pipeline) is
  unexercised, exactly as reported.

## Finding 4 — `theta_survives_snapshot_restore_and_revert` restores against an empty orbit model: **CONFIRMED**

- L16458: `World::restore(PatchedConics::new(BTreeMap::new()), None, snap)`
  with the snapshotted world containing `dormant_vessel` — `OnRails` with
  `parent: BodyId::from("p")` (L16365-16369). L16459 then hot-swaps
  `w2.orbits = test_system()`.
- `World::restore` (L2298-2790+) never inspects `orbits` against vessel
  parents: `grep` of the function body shows `orbits` referenced only at the
  signature (L2299), the struct assignment (L2726), and a doc comment
  (L2765). `restore_vessel_state` (L8088) takes `(vessel, has_physics)` — no
  model. So an `OnRails` vessel whose parent is absent from the supplied
  model restores cleanly; the failure would surface later as
  `Recoverable("orbit model: unknown body")` from `state_at`/`body_state`
  (orbit.rs:882).
- `restore_rejects_crafted_snapshots` (L10341) does craft an OnRails vessel
  parented to `"p"` against an empty model (L10416-10420) but its expected
  `Fatal` fires on `next_vessel_id` (L10428: `s.next_vessel_id = 3` < live
  id 3 + 1), never on the parent gap. Confirmed unexercised.
- Production never hot-swaps: `csp-server/src/backend.rs:365` passes
  `self.orbits.clone()` into restore; no `.orbits =` assignment exists
  outside the test module.
- **Corroborating detail:** two other restore call sites also use empty
  models — `rollback_restores_source_seqs_and_hash` (L9386) and
  `save.rs:783` (`migrated_save_restores_identical_world`) — but both
  snapshots contain zero vessels, so the gap is benign there; only
  `theta_survives` exercises an OnRails vessel against an empty model and
  then papers over it with the hot-swap.

## Finding 5 — stale comment + dead code in m9a: **CONFIRMED**

- L16950-16952 in `script_wake_fires_and_runs_extra_tick`: the stale
  "First run at tick 2 registers the wake → pending `ScriptWake` at 66" line
  sits directly above the corrected "First run at tick 1 registers the wake
  at 65" (assertion uses 65 at L16955-16957). Contradictory lines present
  verbatim.
- L16915-16916 in `script_persist_survives_snapshot`:
  `let orbits = PatchedConics::new(BTreeMap::new()); let _ = orbits;` — dead;
  the restore at L16919-16927 uses `test_system()`.

## "Checked and cleared" claims — spot-verified

- Equivalence test `warp_x1_vs_x1e5_equivalence_10_sim_days` (L10112):
  non-vacuous pins present verbatim — `in_bubble_ids == {2}` (L10202),
  `sa.calls >= TEN_DAYS/64 - 16` (L10207), `sa.max_advance <= 64` (L10208),
  `sb.max_advance > 64` (L10209), `sb.calls*64 < sa.calls` (L10214).
- `rollback_restores_source_seqs_and_hash` at L9380 exists (Fatal mid-drain,
  restore crafted via snapshot). Twin-world rejection tests exist at
  L9876 (`spawn_vessel_rejects_unknown_parent`) and L17101
  (`external_control_rejects_without_lease`).
- Goldens: `data/test-fixtures/{trivial,flight-launch}/` exist with
  `expected-hash.txt`; `golden_trivial_round_trips` at
  `csp-bench/src/replay.rs:954`; `xtask bless-goldens` implemented in
  `xtask/src/main.rs`. Real `RhaiHost`/`AscentAutopilot` at
  `scenarios.rs:180-181` confirmed.
- Test counts: `grep -c '#\[test\]'` across `crates/csp-sim/src/*.rs` totals
  **283** — matches the report's `--list` claim exactly (142 in lib.rs, rest
  in orbit/colonies/transfer/etc.). No `#[ignore]`, `todo!`, or
  `unimplemented!` anywhere in `lib.rs`.

## Verdict

All five findings **CONFIRMED**. Finding 1's central claim is exact: the
Stage-2 install determinism byte-compare is unreachable under test.
Finding 2's load-bearing claim is exact: the 10 km overheat test fails under
the authored atmosphere, not merely "pins different goldens". Corrections are
editorial only — the suggested literal-equality bindings in Finding 2 would
need `period_s`/knot-count carve-outs to compile green, and the authored
atmosphere does see bench-layer coverage via `scenarios.rs`.
