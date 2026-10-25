# Verified Report 5 — orbit/transfer/transfers+maneuver cluster
**Verification agent**: 2026-10-25. Verifying reports 5a, 5-transfer, 5-transfers against live tree @ 1e8c6da.
Status: IN PROGRESS — verdicts appended as confirmed.

### [5-transfers] Finding 1: Burning maneuver node silently destroyed on unlatch — CONFIRMED
**Original**: maneuver.rs docs promise unlatch resumes a mid-burn countdown, but unlatched_body drops v.maneuver unconditionally; latch capture preserves it and restore validates the state, so the resume path is unreachable.
**Verification**: All four sites confirmed on live tree (no concurrent edit: last commit touching these files is d03ca0e, 3 commits behind HEAD 1e8c6da; `git status` clean for csp-sim).
- maneuver.rs:60-62 module doc: "A `Burning` node survives the §2.6.4 latch/unlatch round-trip ... unlatching resumes the countdown where it stopped." maneuver.rs:113-118 variant doc repeats it ("the countdown holds while latched and resumes on unlatch").
- lib.rs:3360-3369: unlatched_body's SurfaceLatched arm sets `v.state = InBubble(body)` then `v.maneuver = None;` unconditionally for BOTH statuses, comment: "a `Burning` one has no executor running: leaving it would strand a zombie node".
- The rationale is false: lib.rs:3674 calls `drive_node_burn(&mut vessel.maneuver, body)` for every InBubble body each bubble tick (guarded only by the `InBubble` let-else at 3655), so a preserved Burning node would resume exactly as documented.
- lib.rs:9794-9801 (transition_latched): capture sets `vessel.state = SurfaceLatched(pose)` and never touches `v.maneuver` — preserved on latch. Mid-burn latch is reachable: a Burning vessel is InBubble; ground contact fires transition_latched.
- lib.rs:9091-9096 (restore): `Burning` accepted on `InBubble(_) | SurfaceLatched(_)` — the state class is save-validated but can never fulfill its documented purpose.
- Exit-path completeness: production transitions out of SurfaceLatched go only through unlatched_body (lib.rs:3328; callers 4530 SetThrottle, 4542 SetAttitude, 4970 apply_unlatch). All other SurfaceLatched sites (8727, 9506, 10446) are restore/hash reads; remaining writes are inside the test module (starts lib.rs:10463).
- One minor evidence imprecision in the original report: Undock does NOT share unlatched_body — apply_undock (lib.rs:6275+) requires `InBubble` and rejects latched vessels. Does not affect the verdict; the claim "unlatch is the only way out of the latched state" holds.

### [5a] Finding 1: soi_candidates publishes Exit{root} transitions rebase refuses; root-exit path (incl. §2.9 debris-escape deletion) dead in shipped data — CONFIRMED
**Original**: soi_candidates admits the parent's own boundary without a grandparent check while rebase's Exit arm requires a grandparent; shipped solen soi_m=None makes the whole root-exit path dead code; validator permits the broken config.
**Verification**:
- orbit.rs:1234-1237: parent-exit candidate admitted purely on `soi_m` being `Some` (`soi_candidate(&s.parent, b.soi_m, true)`) — no check that `b.parent.is_some()`.
- orbit.rs:1004-1008: rebase's Exit arm returns `Recoverable("soi exit: parent has no parent")` when the old parent has no grandparent. Publisher/applier asymmetry confirmed.
- orbit.rs:247-249: `soi_candidate` returns `Ok(None)` for `soi_m: None`, and data/systems/home/solen/body.ron:13 has `soi_m: None` — shipped root never publishes an exit candidate.
- Dead-code claim confirmed both paths: event path (lib.rs:7429-7440 debris intercept in apply_soi_transition) requires an Exit{root} event that the scan can never schedule; stamp path (lib.rs:5566-5570 residence walk) gates on `soi_m.is_some_and(|r_soi| rv.norm() > r_soi)`, false for solen. §2.9 debris escape-past-root deletion never executes in the shipped system.
- Data-reachability confirmed: crates/csp-data/src/validate.rs check_graph (1112-1125) pushes `OrbitOnRoot` only when `star.def.orbit.is_some()`; a root with finite `soi_m` and no orbit elements passes validation (soi_m gets only the positivity check at 1206-1207). A validator-legal system with finite root SOI hits the fire-time `Recoverable` churn described in the report.
- Module doc orbit.rs:90-91 states the assumption ("the star's infinite SOI contributes no exit candidate") that is enforced nowhere. Confirmed.

### [5a] Finding 2: Documented "clamp the horizon and retry" fallback does not exist; degraded republish re-anchors past the dropped crossing — CONFIRMED
**Original**: orbit.rs docs (twice) promise a §2.6.5 clamp-retry fallback for scan failures; the shipped caller propagates with `?`, and the degraded path rolls back the applied transition and republishes SoiRescan past the dropped one-shot crossing, stranding the vessel on the old conic.
**Verification**:
- orbit.rs:80-83 and 122-126: both doc passages promise "the caller's fallback is the §2.6.5 re-issue pattern: clamp the horizon and retry". Confirmed verbatim.
- lib.rs:4455-4462 (schedule_next_soi): `next_soi_event_within(...)? ` — plain `?`, no retry, no horizon clamp. The documented fallback does not exist. Confirmed.
- lib.rs:7310-7319 (SoiTransition arm): `apply_soi_transition(...)?` then `schedule_next_soi(...)` inside one fire_event.
- lib.rs:7157-7161: non-Fatal error → `ev_shadow.restore(self)` rolls back the entire event application including the already-applied transition, error pushed to `degraded`.
- lib.rs:7256-7263: republish `SoiRescan` at `event.tick + DEGRADED_RESCAN_TICKS` (K = MAX_TICKS_PER_FRAME, lib.rs:264) — strictly after the crossing tick.
- lib.rs:7288-7293 (SoiRescan handler): scan origin `event.tick.max(clock)` — after the dropped crossing; next_soi_event_within scans only (now, horizon] per its own contract (orbit.rs:7311-7313 comment: "a rescan may never search the past for crossings"). A one-shot Enter{child} flyby crossing is therefore never re-detected; the vessel keeps the pre-transition conic (wrong frame, wrong μ) with no self-heal. Confirmed.
- Likelihood framing in the report is honest (needs a scan Recoverable, e.g. Kepler iteration cap; budget exhaustion itself is pre-clamped by scan_horizon). The false-doc claim stands regardless.

### [5-transfer] Finding 1: A collinear-endpoint cell kills the entire porkchop scan — CONFIRMED
**Original**: `leg_at` propagates per-cell Lambert errors with `?`, so one geometrically degenerate grid cell aborts the whole scan, contradicting its own "infeasible cell, not an error" contract.
**Verification**:
- transfer.rs:785 `if let Some(sol) = lambert(r1, r2, tof_s, mu_p, way)?` and :789 `for sol in lambert_multi(r1, r2, tof_s, mu_p, way, revs)?` — plain `?` propagation confirmed.
- transfer.rs:170-175: `lambert_geom` returns `Err(Recoverable("lambert: collinear endpoints"))` when `1 + cos_dnu <= 1e-14` (Δν within ~1.4e-7 rad of opposition — since 1+cosΔν ≈ (π−Δν)²/2) or cross-product ≤ 1e-14·r1²r2² (Δν≈0). Threshold math checks out.
- transfer.rs:566: `lambert_velocities(&g, mu, z, revs)?` — the velocity-recovery errors ("infeasible solution point"/"degenerate g coefficient") escape the same way.
- transfer.rs:742-745 doc: "`Ok(None)` when nothing solves (an infeasible cell, not an error)" — collinearity is a property of the sampled cell geometry, so the contradiction is real. One degenerate cell → whole `porkchop_scan`/`plan_transfer` `Err(Recoverable)`.
- Note: `lambert`/`lambert_multi` are also `pub` (transfer.rs:546), so direct callers get the same behavior; the finding stands for the scan path.

### [5-transfer] Finding 2: max_revs unvalidated — the "bounded work" contract is only as bounded as the caller — CONFIRMED
**Original**: `PorkchopQuery::max_revs` is a caller-controlled u32 with no upper bound, but porkchop_scan's doc claims bounded work per cell.
**Verification**:
- transfer.rs:627: `pub max_revs: u32` — no documented or enforced upper bound.
- transfer.rs:836-890 (porkchop_scan_inner): validates reversed ranges, degenerate cell counts, tick-lattice fineness, PORKCHOP_MAX_CELLS cap, and parking radii — no `max_revs` check anywhere. Confirmed absent.
- transfer.rs:813-815 doc: "Bounded work: the cell product is capped at PORKCHOP_MAX_CELLS and each cell costs a bounded number of Lambert solves (2·(1 + max_revs))" — the per-cell count is indeed 2·(1+max_revs), but with max_revs = u32::MAX the total work is unbounded; the claim is vacuous without a cap on max_revs.
- Cost claim verified: `multi_rev_z` (transfer.rs:402-441) runs the full 64-iteration golden-section (BRANCH_MIN_ITERS=64, ~130 tof_residual evals) BEFORE the `f_min >= 0` bail at 438-441 — so even rootless branches (branch TOF minimum far above the sampled tof, the common interplanetary case) pay full cost. `for revs in 1..=q.max_revs` at :788 multiplies this per branch. DoS-by-compute framing stands.

### [5-transfer] Finding 3: Long-way Lambert silently unavailable for short TOFs — CONFIRMED (empirically reproduced)
**Original**: For LambertWay::Long with short TOF, the true root lies in the cancellation/overflow regime where tof_residual reads f64::MAX; zero_rev_z grinds ~50 evals and returns None, so `lambert(..., Long)` returns Ok(None) for solvable problems, contradicting "a solution exists for every tof > 0".
**Verification**: Static + EMPIRICAL.
- Static: transfer.rs:225-239 tof_residual None arm — in the overflow region (stumpff cosh overflows for z < −710² ≈ −5.04e5, orbit.rs:309-328 confirmed: `cosh(s)` with s=√−z) y evaluates NaN for A<0 (inf/inf), `NaN <= 0.0` is false → f64::MAX; for A<0, y → +∞ as z → −∞ (leading-order asymptotics: A·(z·c3−1)/√c2 ≈ |A|·e^{w/4}·... > 0), so the y≤0 pocket that guarantees the Short-way flip never exists for Long. Leading-order cancellation x³c3 + A√y → 0 (both ≈ 2^{−1/4}|A|^{3/2}e^{w/4}) confirms the corruption regime. zero_rev_z (transfer.rs:366-381) loops step *= 4.0 to 1e30 (~50 evals) then returns None.
- Empirical: temp integration test (written, run, removed) sweeping r ∈ {1e6,7e6,1e7,1e8} × Δν ∈ {1°..179°} × tof ∈ {1s..1e7s} with mu=Earth: `lambert(Long)` returned `Ok(None)` in 62/256 cells while `lambert(Short)` solved every one — failures concentrated at r=1e7–1e8 m (exactly the report's claimed regime), including e.g. (r=1e8, Δν=120°, tof=100 s) and (r=1e8, Δν=1°, tof=1e6 s). No `Err` cases. The crate's own contract (transfer.rs:49-50 "a solution exists for every `tof > 0`"; :502-505 admits None means the solve failed) is violated in 24% of the sampled grid.
- Scope honesty matches the report: realistic interplanetary scale (r ~ 1e11, tof ~ days) unaffected (verified in first probe run); Short way never failed.

### [5-transfer] Naming verdict: transfer.rs vs transfers.rs — CONFIRMED (naming hazard, no duplication)
**Original**: Both modules live (`pub mod transfer;` lib.rs:75, `pub mod transfers;` lib.rs:93), zero code duplication, near-identical names are an import/grep hazard.
**Verification**: lib.rs:75 `pub mod transfer;` and lib.rs:93 `pub mod transfers;` both present on live tree. transfer.rs = M5 Lambert/porkchop planner (lambert, porkchop_scan, plan_transfer); transfers.rs = M6-D resource/crew transfer process (ResourceTransfer, fire_transfer_complete, tanks). Disjoint function sets, no shared logic — duplication claim of "none" confirmed; naming-hazard assessment stands.

### [5-transfers] Finding 2: ColonyDelivery contract fact credits dead colonies — CONFIRMED
**Original**: fire_transfer_complete records colony_deliveries and fires ContractTrigger::ColonyDelivery with no liveness gate, while credit_end deliberately credits dead colonies' frozen stores — a supply contract completes by dumping into an abandoned colony.
**Verification**:
- transfers.rs:1027-1041: the fact path gates only on `TransferEnd::Colony(c) && landed > 0.0` — no colony_live check anywhere in the path. Confirmed.
- transfers.rs:1082-1099 (credit_end Colony arm): credits the store unconditionally, gates only the recompute on `colony_live` (line 1095); doc comment 1059-1065 explicitly defends crediting dead colonies ("terminal bookkeeping"). The asymmetry (cargo landing defensible, contract fact not) is exactly as reported.
- Liveness gating exists elsewhere, confirming the codebase's own convention: crew landing vessel→colony checks `colony_live` (transfers.rs:1148-1152), joined_colony_gate checks it at apply time (transfers.rs:1338). The cargo fact path is the outlier.
- Colony death mid-flight is real: colonies.rs documents PowerLost→Degraded, Dormant, Abandoned transitions (colonies.rs:55-59) as mid-life events; nothing in the fact path cancels or filters in-flight transfers on death. Confirmed.

### [5-transfers] Finding 3: Duplicate-resource tank specs break capacity/contents/mass contract for programmatic CraftDef — CONFIRMED
**Original**: For a part with two TankSpecs of the same resource, vessel_capacity sums both, spawn_contents keeps only the last spec, debit/credit use only the first — capacity overstates fillable space, spawn understates contents while mass_kg carries full double wet mass; resolver rejects duplicates but SpawnPhysicalVessel accepts programmatic CraftDef.
**Verification**:
- transfers.rs:331-344 (vessel_capacity): `.filter(|t| t.resource == *resource).map(|t| t.capacity).sum()` — sums every matching spec. Confirmed.
- transfers.rs:399-404 (spawn_contents): collects `(resource, capacity)` into BTreeMap<ResourceId, f64> — duplicate keys: last spec silently overwrites the first. Confirmed.
- transfers.rs:493-497 (debit) and :530-534 (credit): `part.tanks.iter().find(|t| t.resource == *resource)` — first spec only; its `cap` bounds the fill/drain while contents key on the resource alone. Confirmed divergence: transfers pass the `vessel_capacity`-based `free < amount` gate, then credit fills to first-spec cap only.
- transfers.rs:415-432 (check_tank_masses): requires `mass_kg ≥ Σ over ALL specs of capacity·mass_per_unit_kg` (line 420-425 sums every spec) but never rejects duplicate resource ids — the spawn gate lets the divergence in. Called from lib.rs:5154 (apply_spawn_physical); spawn_contents applied at lib.rs:5286. Confirmed as the shipped gate.
- csp-phys/src/resolve.rs (~1290-1296): resolver rejects duplicates ("resolve: duplicate tank capacity resource id") — data-driven crafts protected, programmatic `SimCommand::SpawnPhysicalVessel { craft, .. }` (lib.rs:4515-4522, caller-supplied CraftDef) bypasses. Data-reachability confirmed.

---

## Summary
9 findings verified: **7 CONFIRMED, 0 DISMISSED, 0 FIXED-BY-CONCURRENT-EDIT** (no commits or working-tree edits touched csp-sim since d03ca0e, 3 commits behind HEAD; HEAD's later commits are render/app/art only).
- 5-transfers F1 (HIGH, unlatch burn-drop): CONFIRMED — all four sites traced; one minor report imprecision (Undock does not share unlatched_body; it rejects latched vessels outright) that does not affect the verdict.
- 5a F1 (root-exit publisher/applier asymmetry + dead §2.9 path): CONFIRMED.
- 5a F2 (false clamp-retry doc; degraded republish past dropped crossing): CONFIRMED.
- 5-transfer F1 (collinear cell kills scan): CONFIRMED.
- 5-transfer F2 (max_revs unvalidated): CONFIRMED.
- 5-transfer F3 (Long-way short-TOF vanish): CONFIRMED — empirically reproduced (62/256 sweep cells Long=Ok(None) with Short solving; r=1e7-1e8 regime as claimed).
- 5-transfer naming verdict: CONFIRMED (both modules live, no duplication).
- 5-transfers F2 (dead-colony contract credit): CONFIRMED.
- 5-transfers F3 (duplicate tank resource divergence): CONFIRMED.

Cluster verdict: REQUEST CHANGES stands for all three reports.
