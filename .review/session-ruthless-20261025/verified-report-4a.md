# Verified Report 4a — csp-sim lib.rs (production half)

Verified against current HEAD `1e8c6da` (2026-10-25). All original line
numbers still resolve identically — the concurrent fix agent's commits
(render/app/playability) did not touch any cited code. Nothing is
FIXED-BY-CONCURRENT-EDIT. Finding 1 was additionally verified by an
empirical reproduction (temporary in-crate test, run green, then
reverted; `lib.rs` md5-matches its pre-edit state).

### Finding 1: Debris SOI-escape deletion Fatal-loops the world — CONFIRMED (empirically reproduced)
**Original**: `fire_event` removes the vessel via `apply_soi_transition`'s §2.9 escape arm, then unconditionally calls `schedule_next_soi`, which Fatals on the now-unknown vessel; rollback re-inserts the event → permanent Err loop.
**Verification**:
- `crates/csp-sim/src/lib.rs:7305-7319` — the `SoiTransition` arm calls `apply_soi_transition(...)?` then `schedule_next_soi(*vessel, ...)` with **no existence guard between** (current HEAD, unchanged).
- `crates/csp-sim/src/lib.rs:7432-7441` — removal path: `Exit && orbit.parent == *body && body_orbit(body)?.parent.is_none() && debris_since.is_some()` → `remove_vessel` → `return Ok(())`.
- `crates/csp-sim/src/lib.rs:4442-4445` — `schedule_next_soi`: `.ok_or(SimError::Fatal("soi schedule: unknown vessel"))`.
- `crates/csp-sim/src/lib.rs:7112-7165` — `fire_due_events` shadows the heap into the iteration shadow **before** the pop (`shadow_events(shadow)`, line 7117) and returns `Err(Fatal)` unchanged; `step_frame` (`crates/csp-sim/src/lib.rs:7579-7586`) then runs `shadow.restore(self)` — `TickShadow::restore` writes the captured heap back (`crates/csp-sim/src/lib.rs:1870-1871`), re-inserting the popped event at its tick.
- `crates/csp-sim/src/orbit.rs:244-247,1234-1237` — `soi_candidate` admits a parent boundary only when `soi_m` is `Some`; a finite root SOI therefore publishes `Exit{root}` for escape conics (test fixtures at orbit.rs:1872/1996-2002 assert such Exits against finite-SOI roots).
- `crates/csp-data/src/validate.rs:1206-1208` — validation only checks `soi_m > 0` when present; nothing requires a root's `soi_m` to be `None`. Authored `.ron` systems with a finite root SOI are legal.
- `data/systems/home/solen/body.ron:13` — root `solen` ships `soi_m: None` → the escape-deletion arm is dead code in the shipped system (and in the sim's own `test_system`/`test_system_crossing` fixtures, lib.rs:10492/10560 — both roots are `soi_m: None`; the path has zero test coverage).
- Command-side twin contrast confirmed: `apply_to_rails_at` (`crates/csp-sim/src/lib.rs:5408-5414`) re-checks `self.vessels.get(&vessel)` after `resolve_soi_residence` precisely because "the residence walk can consume the vessel outright" — the guard `fire_event` lacks.
- **Empirical reproduction** (temporary test, reverted): single root body `p` with `soi_m: Some(8.4e7)`, debris vessel on the `crossing_vessel_elements()` escape hyperbola. Observed: (a) `SoiTransition { vessel: VesselId(0), body: "p", direction: Exit }` published at spawn for tick 3,192,078 — publication path live; (b) on firing: `Fatal("soi schedule: unknown vessel")` — exact predicted message; (c) the rolled-back Exit event re-appears in the heap and the clock can never pass tick 3,192,078 — three consecutive re-advance rounds each Fatal again. **Additional nuance discovered**: from the second firing on, the Fatal comes from `apply_soi_transition` itself (`"soi event for unknown vessel"`, lib.rs:7421) because the deletion *stands* — the per-event `ev_shadow`'s captures die on Fatal (only `Ok` merges via `merge_committed`), and in a physics-less world the iteration shadow never captured the arena. Two distinct permanent Fatal sources; neither is escapable by `step_frame` alone (an out-of-band command deleting the debris would fire the same second Fatal on the stale event, since the drain precedes the event tick but `apply_soi_transition` Fatals on the missing vessel before any arm runs).

### Finding 2: Aero tables refreshed once per step_frame iteration, not per tick — CONFIRMED
**Original**: The staleness repair is a pre-pass before the `for t in now..t_next` loop; fragments created mid-range (breakup, in-range undock) fly the parent's tables for the remainder of the range — partition-dependent physics (§2.6.1 violation).
**Verification**:
- `crates/csp-sim/src/lib.rs:3453-3469` — the only range-level refresh (`aero_tables_stale` filter + `ensure_aero`) runs **before** the tick loop at `lib.rs:3497`.
- Exhaustive call-site check: `ensure_aero` appears at 3467 (pre-pass), 5636 (`apply_to_bubble`), 5877 (`fire_maneuver_start`), 6257 (`apply_jettison_part`), 6503 (`apply_undock`) — **none inside the tick loop**; `aero_tables_stale` is invoked only at 3456. Nothing rechecks staleness mid-range.
- Fragment inheritance confirmed: `apply_breakup` `fb.aero = aero.clone()` (`crates/csp-sim/src/lib.rs:10031`) and `apply_undock` `fb.aero = parent_body.aero.clone()` (`crates/csp-sim/src/lib.rs:6319`), both commented "§2.4: inherit until Stage-2 recompute". `apply_breakup` is called from inside the tick loop at `lib.rs:4005` (tick `t + 1`); undock fragments arise mid-range via `drain_at` at `lib.rs:3512` (script/autopilot-issued commands). `apply_undock` refreshes only the retained vessel (`lib.rs:6503`).
- Backend consumption confirmed: `crates/csp-phys/src/simple.rs:1165-1178` — per-tick force integration reads `body.aero` (per-part force tables, parent node ids/CoPs) every atmospheric tick.
- The code's own doc admits the dependence: `lib.rs:9713-9720` explains the hash check exists so a fragment doesn't "fly the parent's geometry — phantom drag and the wrong CoP arm — **forever**" — i.e. the repair lands at the *next* pre-pass, exactly the partition dependence claimed. Boundary-drain undocks are covered (pre-pass runs after `drain_boundary`), which is why only mid-range creations escape. Severity HIGH stands: breakup-under-heating mid-range is the core M4c scenario, and divergence propagates to impact ticks, colony damage, and the hashed state.

### Finding 3: EngageAutopilot accepts programs the installed driver cannot fly — CONFIRMED (severity nuance: suggest LOW–MEDIUM)
**Original**: Apply validates only `is_valid()` + vessel existence; `run_autopilot_tick` silently `return Ok(())` on driver-absent/discriminant-mismatch — an inert control record logged as applied.
**Verification**:
- `crates/csp-sim/src/lib.rs:4709-4725` — apply checks exactly two things; no `autopilot_driver` presence or program check. Confirmed unchanged.
- `crates/csp-sim/src/lib.rs:6924-6935` — silent skip on `autopilot_driver == None` (6924-6926) and on discriminant mismatch (6930-6934); nothing is pushed to `degraded` on either path (only `run_tick` faults surface, 6954-6957). The applied command lands in `command_log` (drain_at, `lib.rs:6621`).
- `crates/csp-sim/src/scripts.rs:467-472` — the skip is documented design ("the world runs `run_tick` only when `state.program` matches … forward-compat seam").
- **Nuance**: `AutopilotProgram` has exactly ONE variant today (`Ascent`, `scripts.rs:239-249`), and every production caller installs the matching driver before engaging (`csp-mcp.rs:92`, `csp-view/src/lib.rs:839`, `csp-bench/src/replay.rs:758`, `scenarios.rs:181`). So the "different valid variant" half is currently unreachable; the live silent-inert path is only "no driver installed" (direct library use — `autopilot_driver: None` at construction, `lib.rs:2298`, and post-restore, `lib.rs:3017`). Real API-contract gap with zero observable failure, but practical exposure is narrow; recommend downgrading to LOW unless the multi-variant future is near. The suggested fix (reject at apply when no/mismatched driver) remains correct.

### Finding 4: aero_tables_stale recomputes the full blake3 craft hash per InBubble vessel per iteration — CONFIRMED
**Original**: The staleness predicate freshly computes `craft_graph_hash(craft)` — a complete `hash_craft` blake3 walk — for every InBubble vessel at the top of every `step_bubble_range`, mostly to conclude "not stale".
**Verification**:
- `crates/csp-sim/src/lib.rs:9722-9732` — in the steady state (`body.aero` attached, mask equal) the `||` short-circuit falls through to `t.graph_hash != craft_graph_hash(c)`, computing the full hash. Confirmed.
- `crates/csp-sim/src/lib.rs:9704-9711` — `craft_graph_hash` = full `hash_craft` through `StateHasher`; `crates/csp-core/src/hash.rs:228` — `StateHasher` wraps `blake3::Hasher`. `hash_craft` (`lib.rs:9561+`) walks node transforms, collider shapes, force sites, dock ports, tanks, seats, aero cd/cl curves, thermal, structural, science, reliability — the whole craft.
- `crates/csp-sim/src/lib.rs:3453-3458` — invoked per InBubble vessel per `step_bubble_range` call = per `step_frame` loop iteration (each event boundary/budget slice re-enters). Severity MEDIUM (warp-time CPU/alloc churn scaling with vessels × parts) is fair; deterministic and correct, purely a cost issue.

### Finding 5: Rollback deep-clones whole sub-states per command/event — CONFIRMED
**Original**: Per staged command a fresh `TickShadow` whose first vessel touch deep-clones the entire vessel arena; per fired event a fresh shadow whose first schedule clones the whole event heap; every drained command cloned into `shadow.drained`.
**Verification**:
- `crates/csp-sim/src/lib.rs:6600` — `let mut cmd_shadow = TickShadow::capture(self);` per staged command (`capture` starts all-None, `lib.rs:1672-1686`, so each command's first vessel touch re-captures).
- `crates/csp-sim/src/lib.rs:3251-3266` — `shadow_vessels`: `shadow.vessels = Some(self.vessels.clone())` — full `BTreeMap<VesselId, Vessel>` deep clone (craft graphs, parts, tanks, science, reliability) plus joints; per shadow, i.e. per command.
- `crates/csp-sim/src/lib.rs:7136` — `let mut ev_shadow = TickShadow::capture(self);` per popped event; `crates/csp-sim/src/lib.rs:8515-8524` — `shadow_events_split`: `Some(heap.clone())` — full `BinaryHeap<ScheduledEvent>` clone on the shadow's first heap touch (every SOI scheduling).
- `crates/csp-sim/src/lib.rs:6570` — `shadow.drained.push((src, entry.clone()))` — every drained command cloned, including `CraftDef`-carrying `SpawnPhysicalVessel`/`ManufactureCraft`.
- `merge_committed` (`lib.rs:1746-1747`) only folds captures into the *iteration* shadow; it does not dedupe subsequent items' captures — N vessel-touching commands ⇒ N full-arena clones per boundary, as claimed. Correctness unaffected; MEDIUM as allocation churn at 64 Hz is reasonable.

## Out-of-scope observations — all CONFIRMED as written
- **Spawn SOI residence**: `crates/csp-sim/src/lib.rs:5036-5039` — self-documented: "Latent edge (M5-A backlog): residence is NOT resolved at spawn — an authored spawn inside a child's SOI stays mis-parented". Live gap, acknowledged in-code.
- **prepare-before-shadow**: `crates/csp-sim/src/lib.rs:6974-6978` — `engine.prepare` runs before `shadow_scripts`; the engine's construction state is never shadowed, so a later Fatal in the same apply leaves phantom prepared state (only the id-overflow Fatal at 6982-6984 can trigger it; self-healing on id reuse). Asymmetry real, window tiny.
- **Qualifications clone per tick**: `crates/csp-sim/src/lib.rs:3551` — `let quals = self.tech.qualifications.clone();` inside the `for t` tick loop, deliberately (comment: partition-identity for mid-range burn credits). Per-tick allocation churn confirmed.

## Summary
5 confirmed, 0 dismissed, 0 fixed-by-concurrent-edit. All original line numbers still resolve at HEAD `1e8c6da`.
Severity notes: Finding 1 HIGH stands (empirically bricked; plus a second permanent Fatal source discovered); Finding 2 HIGH stands; Finding 3 confirmed but suggest LOW–MEDIUM (single program variant, all production callers install the driver — only the driver-absent path is live); Findings 4-5 MEDIUM stand.
