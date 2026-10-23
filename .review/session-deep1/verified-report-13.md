# Verified Report — Cluster C13 (`crates/csp-phys/src/{simple,aero,thermal}.rs`)

Verification of `review-report-13.md` against current source. All line numbers
re-checked against the working tree.

## Headline answers

- **Is the SoftCapture `pull_per_s` convergence hazard real?** **Yes — CONFIRMED.**
  The reel-in is an explicit map `c_{n+1} ≈ (1 − pull·dt)·c_n` implemented by an
  unclamped bilateral PGS velocity target; nothing anywhere bounds `pull_per_s`,
  and the `balance.ron` → `DockingConfig` → `JointKind` path applies it without
  calling `is_valid`. `pull_per_s ≥ 128` produces a permanent bounded limit
  cycle; `[64, 128)` converges with alternating overshoot.
- **Does thermal Euler really diverge to non-finite and poison hashed state?**
  **The divergence mechanism is CONFIRMED; the "poisons hashed state silently"
  part is DISMISSED.** `thermal::step` is unguarded explicit Euler and can write
  ±inf/NaN into the serialized `part_temps_k`. But `StateHasher::f64` rejects
  non-finite values — a `Fatal` fault surfaces at `finish()` (debug builds
  trap), and snapshot restore independently validates `part_temps_k` for
  finiteness. The real failure mode is silent *in-tick* spread (NaN evades the
  lethality check and hops one edge per tick through conduction) followed by a
  hard `SimError::Fatal` at the next `state_hash` — a typed crash, not a silent
  golden-hash corruption as the report claims.

## Per-finding verdicts

### Finding 1 [MEDIUM] — SoftCapture `pull_per_s` has no stability bound: **CONFIRMED**

Evidence re-verified:

- `JointConstraint::new` target `(-pull_per_s * c).clamp(±max_correction_mps)`
  per axis, no bound on `pull_per_s` itself —
  `crates/csp-phys/src/simple.rs:868-881`.
- `solve_joint_rows` applies `dl = (target[row] − vrel·dir)·k` unclamped,
  bilateral, per-row GS re-evaluation — `simple.rs:1706-1740` (row solve at
  :1720-1722). `SOLVER_ITERATIONS = 8` (`simple.rs:227`), cold-started (doc at
  :1703-1705). For a single joint the solve drives `vrel` to `target` to GS
  tolerance, so the anchor separation follows `c_{n+1} ≈ (1 − pull·dt)·c_n` in
  the unsaturated band — the claimed explicit map is correct.
- `PHYSICS_DT = 1/64` exactly (`simple.rs:221`, `csp-core/src/time.rs:9`
  `TICK_HZ = 64`). Stability bound `pull·dt < 2` ⟹ `pull_per_s < 128`;
  non-oscillatory below `pull_per_s < 64`. Math confirmed.
- Validation path confirmed absent where it matters: `DockingConfig::is_valid`
  accepts any finite `soft_pull_per_s ≥ 0` (`csp-sim/src/joints.rs:218-219`),
  and `is_valid` is only invoked at snapshot restore (`csp-sim/src/lib.rs:2306`).
  `World::set_docking_config` (`lib.rs:2159-2161`) installs unchecked, and
  `from_balance` (`joints.rs:194-206`) copies `b.dock_soft_pull_per_s` verbatim
  into `joint_kind()` → `LiveJoint.kind` (`joints.rs:231-236`, :533). The
  `csp-data` loader does file-shape checks only (`load.rs:344-353`); the
  semantic validator does not inspect `BalanceDef` at all (no `dock_*` rules in
  `csp-data/src/validate.rs`). A `dock_soft_pull_per_s: 200.0` really does flow
  into the constraint untouched.
- Limit cycle confirmed: inside `|c| < cap/pull` the map amplifies by
  `|1 − pull·dt| ≥ 1`; outside, the clamp pins `vrel` to `±cap`, pushing `c`
  back toward the band — bounded oscillation with amplitude of order `cap·dt`
  (≈`(pull·dt)·cap/pull`, i.e. ~2–3× the report's `cap/pull` scale estimate;
  same class). A full-strength bilateral impulse is injected every tick.
- Failure narrative holds **for the shipped defaults**: in the limit cycle
  `‖vrel‖ ≈ cap` and `DEFAULT_SOFT_MAX_CORRECTION_MPS = 1.0`
  (`joints.rs:125`) > `DEFAULT_LATCH_REL_SPEED_MPS = 0.5` (`joints.rs:129`),
  so `latch_ready`'s rel-speed gate (`joints.rs:761-762`) fails forever → the
  joint dissolves at `formed_tick + capture_window_ticks`
  (`joints.rs:474-477`) and docking fails. Caveat: a retune with
  `cap ≤ latch_rel_speed` could still latch despite the buzz — the instability
  is real regardless; the "never reads settled" clause is cap-dependent.
- Default `dock_soft_pull_per_s: 8.0` (`joints.rs:123`, `data/balance.ron:29`)
  → `pull·dt = 0.125`, safe — latent, correctly scoped.

### Finding 2 [MEDIUM] — thermal explicit Euler unguarded: **CONFIRMED in mechanism, impact characterization corrected**

Mechanism — all confirmed:

- `step` is raw explicit Euler: `new_t = t + dt·rates[i]/C`,
  `temps.insert(n.node, new_t)` with no clamp, sub-step, or finiteness check —
  `crates/csp-phys/src/thermal.rs:350-357`. Conduction reads pre-update temps
  (:333-342), radiation folded in during integration (:355).
- Conduction eigenvalue `1 − dt·G·(1/C_a + 1/C_b)` — divergence (alternating
  growth) when `G·dt·(1/C_a+1/C_b) > 2`, i.e. `G > 128·C_a·C_b/(C_a+C_b)` at
  `dt = 1/64`. Confirmed.
- Radiation overshoot: `t' = t − dt·εσA·t⁴/C` goes negative when
  `dt·εσA·t³/C > 1`; once `T < 0`, `a4 − t⁴` stays large-negative → monotone
  runaway to −inf. For shipped-scale values (C ≈ 2·10⁴ J/K, ε ≈ 0.8,
  A ≈ 24 m²) the threshold is T ≈ 1–2×10⁴ K — the report's ~2×10⁴ K is the
  right order. Also: lethality *cannot* stop the radiation path in shipped data
  (parts die at `max_temp ≤ 1600` K, far below the overshoot threshold — so the
  path additionally requires either lethality-free operation or a
  `max_temp_k`/capacity combo that reaches ~10⁴ K first).
- `weld` validates `heat_capacity > 0`, `emissivity ∈ (0,1]`,
  `conduction ≥ 0`, `max_temp_k > 0` **individually** — no coupled `G·dt/C`
  bound — `crates/csp-phys/src/weld.rs:425-437`. Confirmed. Note the
  conduction bound is reachable even with *shipped* conductances if a small
  capacity is authored (C ≲ 1 J/K with k = 120 W/K gives `λ ≈ 2+`); shipped
  data itself (C ≥ 2·10⁴, k ≤ 120) is ~4 orders of magnitude inside the
  stable region — latent, as the report says.
- `part_temps_k` is the vessel's serialized, authoritative record
  (`csp-sim/src/bubble.rs:32-47`, serde; write site `csp-sim/src/lib.rs:3399-3406`)
  and is hashed via `h.f64(*t)` (`lib.rs:9033-9041`). Confirmed.
- `t = +inf` → `t2*t2 = +inf`, rate `a4 − inf = −inf`,
  `new_t = inf + (−inf) = NaN`; NaN persists (`NaN + x = NaN`). Confirmed.
  Additional verified detail the report missed: a NaN/inf temperature
  **evades the lethality gate** — `new_t > max_temp_k` is false for NaN —
  and conduction spreads it one edge-hop per tick
  (`q = G·(NaN − ta) = NaN` → the neighbor's `new_t` is NaN), so the whole
  tree is poisoned within graph-diameter ticks.

Impact claim — **corrected**:

- The report's "±inf or NaN propagates silently into the determinism pipeline
  rather than failing typed … hashes fine and corrupts the oracle/hash-golden
  pipeline instead of producing a `SimError`" is **wrong**.
  `StateHasher::f64` → `write_f64` rejects non-finite:
  `Err(SimError::Fatal("non-finite f64 in state hash"))` — debug builds
  `debug_assert!`-trap, release records the fault and `finish()` returns the
  `Fatal` (`csp-core/src/hash.rs:275-305`, `finish` at :426-431). The hash
  does **not** "hash fine"; it fails typed and (per its doc) aborts/rolls back
  the tick.
- Snapshot restore independently gates `part_temps_k` on
  `is_finite() && ≥ 0` → `SimError::Fatal` (`csp-sim/src/lib.rs:8105-8112`).
- **Accurate characterization:** the defect is silent *within* the tick loop —
  no writeback guard, NaN evades lethality, and the poison spreads through the
  conduction tree — but it converts to a hard `Fatal` at the next
  `state_hash`/restore rather than silently corrupting a golden hash. Still a
  real robustness gap (a crash surfaced far from its cause, plus polluted live
  state in the interim), but the report's "silent hash corruption" severity
  framing overstates it. The suggested fixes (weld-side `G·dt·(1/C_a+1/C_b)
  < 2` bound; per-tick `new_t.is_finite()` guard) remain appropriate — the
  writeback guard is what converts this from "crash on next hash" to "clamped
  at source".

### Finding 3 [LOW] — `ref_area_m2` sums total (incl. occluded) area: **CONFIRMED**

- `ref_area = max_d Σ_p area_m2[d]` (`aero.rs:1296-1300`); `area_m2 =
  a_dir + blocked` is explicitly the *total* exposed projected area
  (`aero.rs:1227-1228`, field doc :640-646). Overlapped craft get an inflated
  `A_ref` vs the true silhouette `Σ area·(1−shielded)`. Knots divide by it
  (`aero.rs:1331-1336`).
- Diagnostic-only today confirmed: the only consumers of `ref_area_m2`/knots
  are the table build itself, `bit_eq`, tests (`cm_alpha_table_vs_direct`
  normalizes by `t.ref_area_m2` on both sides, `aero.rs:1946` — self-cancels,
  as the report says), and a `csp-bench` finiteness assertion
  (`scenarios.rs:845`). No live force path reads it.

### Finding 4 [LOW] — zero-area `cop` fallback is vertex centroid mislabelled "CoM": **CONFIRMED**

- `part_com = verts.mean()` (`aero.rs:1216`); pushed as `cop[di]` whenever
  `a_dir == 0` (`aero.rs:1234-1238`). Field doc calls it "the part's
  body-frame CoM" (`aero.rs:652-654`) — vertex centroid ≠ `WeldPart.com` for
  asymmetric vertex sets; the comment is wrong.
- `cop` is interpolated unconditionally in `part_force` whenever the blended
  `a_eff > 0` (`aero.rs:752-758`), so a zero-area vertex drags the blended
  application point toward the centroid — bounded torque-arm bias, exactly as
  described.

### Finding 5 [LOW] — pair contacts reuse ground `env.friction_mu`: **CONFIRMED**

- Pair-manifold Coulomb limit `env.friction_mu * c.lambda_n` —
  `simple.rs:1658` (same expression as ground at :1605). Field doc: "Coulomb μ
  for ground contact" (`simple.rs:294`). Deterministic, bounded; modeling-
  fidelity note stands.

### Finding 6 [LOW] — self-joints not rejected: **CONFIRMED**

- `generate_joint_constraints` filters on bounds only (`simple.rs:913`); the
  documented contract is "joints naming a handle outside `bodies` are skipped"
  (`simple.rs:907-909`). For `body_a == body_b`: `inv_m_a + inv_m_b = 2/m`
  (`simple.rs:843-844`) halves `dl`, and `+j`/`-j` at distinct anchors gives
  zero net linear impulse but `(r_a − r_b)×j ≠ 0` net torque — silent spin
  injection, as analyzed.
- Unreachable from `csp-sim` today: `LiveJoint` is canonically
  `vessel_a < vessel_b` (`joints.rs:266-268`) → distinct bodies. Latent
  API-hardening gap; the one-line filter suggestion is correct.

### Finding 7 [LOW] — latched + `throttle > 0` flicker: **CONFIRMED, with a mechanism correction**

- Latch zeroes `vel`/`ang_vel` and clears contacts but leaves `b.throttle`
  (`simple.rs:1960-1967`); the throttle unlatch fires on the next tick
  (`simple.rs:1995-2003`); module doc cites "a latched vessel's throttle is
  zero" (`simple.rs:1996`, doc :202-207). The invariant is not enforced —
  confirmed.
- **Correction to the claimed impact:** no integration tick is skipped. The
  unlatch happens at the top of the fused pass and falls through to
  `integrate_free` *in the same tick* (`simple.rs:1994-2005`) — the `continue`
  only runs when `throttle ≤ 0`. So the cycle is latch at `settled_ticks =
  128` → unlatch+integrate next tick → re-accumulate 128 settle ticks →
  re-latch ≈ every ~128 ticks (2 s). The real per-cycle discontinuity is the
  velocity-zero + contact-clear at the latch instant (momentum destroyed, pose
  continuous — tiny since `‖v‖ < settle_v`), not the report's "one skipped
  integration tick per ~2 s" / "0.8% duty cycle of skipped integration".
  The flicker itself and the doc-contradiction stand.

### Finding 8 [LOW] — `voxelize_craft` dims-product overflow: **CONFIRMED, with numeric correction**

- `dims[i] = (ceil(span/edge) as usize).max(1)`, product checked in plain
  `usize` (`aero.rs:990-993`); `part_of` allocated with the same expression
  (`aero.rs:1003`). `f64→usize as` saturates; the overflow is in the
  multiply/compare.
- **Correction to the reachability number:** overflow needs the first-iteration
  product > 2⁶⁴ ≈ 1.85×10¹⁹, i.e. span-product > ~6.2×10¹⁶ m³ at edge 0.15 m —
  ≈ **396 km per axis cubic**, not the report's "≳260 km/axis" (260³ km gives
  5.2×10¹⁸ — no overflow). Elongated shapes need more (a flat sheet needs each
  large axis > ~640,000 km). Same class of absurd geometry; order right,
  constant off by ~1.5×.
- Release-mode outcome is twofold, not just "silently wrong tables": a wrapped
  product passing `≤ VOXEL_BUDGET` under-allocates `part_of` while `dims` keeps
  the true values → `index()` (`aero.rs:440-441`) can exceed the vec length →
  **OOB panic** on `part_of[ix]` (`aero.rs:539-542`), or `index()` itself wraps
  to an in-bounds index → silently wrong cell stamps/tables. Debug: multiply
  overflow panics. Unguarded-overflow defect confirmed; crash-vs-silent split
  corrected.

## Verdict-section spot checks

- No `HashMap`/`HashSet` anywhere in `csp-phys/src` (only doc references) —
  confirmed.
- No bare `f64` transcendentals: `.sqrt()/.exp()/.sin()/.cos()/.pow*` appear
  only via `libm::` — confirmed.
- Joint sort key `(body_a, body_b, anchor bits)` — `simple.rs:915-926`,
  confirmed.
- `area_m2` total-area semantics and the `A_eff = area·(1−shielded)` use —
  `aero.rs:1227`, `aero.rs:754`, `thermal.rs:316-320` — confirmed consistent
  (no double-counted shielding).
- Impulse write-back record: ground impulse rotated to world, pair `+J`/`-J`
  split — `simple.rs:2026-2039`, confirmed.
- `update_settle` runs after the position pass at end of `step`
  (`simple.rs:2040-2043`), as the finding-7 analysis assumed.

## Corrections summary (things the report got wrong)

1. **Finding 2 impact**: non-finite temps do **not** "hash fine" —
   `StateHasher::f64` rejects them as `Fatal` (debug trap + release typed
   fault) and restore validates finiteness. Real mode: silent in-tick spread +
   lethality evasion → hard `Fatal` at next hash/restore.
2. **Finding 7 impact**: no integration tick is skipped (unlatch precedes
   `integrate_free` in the same tick); the discontinuity is a per-~2 s
   velocity-zero/contact-clear, not an 0.8% skipped-tick duty cycle.
3. **Finding 8 threshold**: ~396 km/axis cubic (not ~260 km); release can also
   OOB-panic, not only produce silent wrong tables.
4. **Finding 1 nuance**: the "never reads settled → docking fails" clause
   requires `cap > latch_rel_speed` (true for shipped defaults 1.0 > 0.5); the
   limit-cycle amplitude is `O(cap·dt)`, ~2–3× `cap/pull`.
