# Ruthless Review — Cluster 4a: crates/csp-sim/src/lib.rs lines 1–10,742

Scope: first half of csp-sim/src/lib.rs (types, TickShadow, World construction/restore/snapshot, command drain, `step_frame`/`step_bubble_range`, control/autopilot/script plumbing, fuel/burn helpers, restore coherence checks, `apply_breakup`, hash walks). Static review only. All findings verified against the live code; cross-references into csp-phys/csp-core/csp-data checked.

### [SEVERITY: HIGH] Finding 1: Debris SOI-escape deletion Fatal-loops the world — `fire_event` schedules a scan for a vessel `apply_soi_transition` just deleted
**File**: crates/csp-sim/src/lib.rs:7305
**Problem**: The `SoiTransition` firing arm removes the vessel (§2.9 debris escape) and then unconditionally publishes the next scan for that now-deleted vessel. `schedule_next_soi` treats an unknown vessel as an invariant violation and returns `Fatal`, which rolls the whole iteration back — including the popped event — so the identical event fires again on the next `step_frame` call. The world is permanently bricked: every future `step_frame` returns `Err(Fatal)`.
**Evidence**:
```rust
// lib.rs:7305-7318 (fire_event)
EventKind::SoiTransition { vessel, body, direction } => {
    self.apply_soi_transition(*vessel, body, *direction, event.tick, shadow)?;
    // ...
    self.schedule_next_soi(
        *vessel,
        SimTime::from_ticks(event.tick.max(self.clock.ticks())),
        shadow,
        Some((body.clone(), *direction)),
    )
}
// lib.rs:7432-7441 (apply_soi_transition — the removal path just taken)
if direction == orbit::SoiDirection::Exit
    && orbit.parent == *body
    && self.orbits.body_orbit(body)?.parent.is_none()
    && self.vessels.get(&vessel_id).is_some_and(|v| v.debris_since.is_some())
{
    self.remove_vessel(vessel_id, shadow);
    return Ok(());
}
// lib.rs:4442-4445 (schedule_next_soi)
let Some(orbit) = self
    .vessels
    .get(&vessel)
    .ok_or(SimError::Fatal("soi schedule: unknown vessel"))?
```
**Impact**: In any system whose root body carries a finite `soi_m` (legal in a hand-built `PatchedConics` — the channel `World::restore`/`WorldConfig` accepts and csp-bench replays carry), a debris vessel put on rails (`ToRails`) on an escape conic gets an `Exit{root}` event published (orbit.rs:1235 exits only against a parent with `soi_m: Some`). When it fires: removal succeeds → `schedule_next_soi` Fatals → `fire_due_events` returns `Err` → `step_frame` restores the shadow (heap rollback re-inserts the popped event at its tick) → next call fires it again. Unrecoverable save-destroying loop on a plan-specified path (§2.9 "deleted outright if it leaves the outermost SOI"). The command-side twin (`apply_to_rails_at` → `resolve_soi_residence` deletion) handles exactly this case correctly (lib.rs:5409-5414 re-checks existence), proving the hazard was known and missed on the event side. Corollary: in the shipped home system the root `solen` has `soi_m: None` (data/systems/home/solen/body.ron:13), so the Exit{root} event can never publish there — the §2.9 escape-deletion rule is dead code in production and escaping debris is never removed.
**Suggestion**: In `fire_event`'s `SoiTransition` arm, mirror `apply_to_rails_at`'s guard: only call `schedule_next_soi` when the vessel still exists and is on rails after `apply_soi_transition`:
```rust
self.apply_soi_transition(*vessel, body, *direction, event.tick, shadow)?;
if self.vessels.get(vessel).and_then(|v| v.orbit()).is_some() {
    self.schedule_next_soi(*vessel, /* ... */, shadow, Some((body.clone(), *direction)))
} else { Ok(()) }
```
Separately, decide what §2.9 escape deletion means when the root SOI is infinite (today: never) and document/test it.

### [SEVERITY: HIGH] Finding 2: Aero tables are refreshed once per `step_frame` iteration, not per tick — mid-range breakup/undock fragments fly partition-dependent physics (§2.6.1 violation)
**File**: crates/csp-sim/src/lib.rs:3453
**Problem**: `step_bubble_range` re-derives stale aero tables only in a pre-pass that runs once, before the `for t in now..t_next` loop. Fragments created *inside* the range — breakup debris (thermal/structural failure at any in-range tick) and undock fragments from an in-range (script/autopilot-issued) drain — enter the arena wearing the *parent's* aero tables and keep them for every remaining tick of the range, because nothing inside the tick loop rechecks staleness. `SimpleBackend`'s force integration reads `body.aero` per tick (csp-phys/src/simple.rs:1165-1178), so drag and aero heating for those ticks are computed against the wrong geometry.
**Evidence**:
```rust
// lib.rs:3453-3458 — the ONLY refresh point, before the tick loop
let needing: Vec<VesselId> = self
    .vessels
    .iter()
    .filter(|(_, v)| aero_tables_stale(v))
    .map(|(id, _)| *id)
    .collect();
// lib.rs:10031 (apply_breakup) and lib.rs:6319 (apply_undock) — fragments inherit:
fb.aero = aero.clone(); // §2.4: inherit until Stage-2 recompute
// lib.rs:6503 — undock refreshes ONLY the retained vessel:
self.ensure_aero(vessel)
```
**Impact**: The refresh point depends on where `step_frame` call boundaries fall. A breakup at tick t inside a 1000-tick range (e.g. reentry heating under warp — the core M4c scenario) integrates the fragments with the parent's tables for the rest of the range; the same advance partitioned more finely (event boundaries, smaller targets) rebuilds correct tables at t+1. Different forces → divergent trajectories → divergent impact ticks → divergent *hashed* state (debris impact → colony damage via `apply_surface_impacts`, `SurfaceLatched` contract triggers). That is precisely the §2.6.1 "identical state regardless of partition" guarantee this milestone exists to enforce, broken for any atmospheric breakup mid-range. (Boundary-drain undocks are covered — the pre-pass runs after them — which is exactly why the bug hides: only mid-range creations escape.)
**Suggestion**: Move the staleness filter + `ensure_aero` inside the tick loop (run it per tick before the batch build; the filter is cheap once Finding 4's hash cost is fixed), or build fresh tables for fragments inline in `apply_breakup`/`apply_undock` the way the retained body already does at lib.rs:10202-10215.

### [SEVERITY: MEDIUM] Finding 3: `EngageAutopilot` accepts programs the installed driver cannot fly — a silently inert control record
**File**: crates/csp-sim/src/lib.rs:4709
**Problem**: The apply validates only `program.is_valid()` and vessel existence. It never checks the installed `AutopilotDriver` actually flies that program. `run_autopilot_tick` silently skips on discriminant mismatch (`return Ok(())`), so the engagement applies, is logged as an applied command, and then never ticks — no rejection, no `degraded` entry, no surfaced state.
**Evidence**:
```rust
// lib.rs:4709-4724 — apply: no driver-program check
SimCommand::EngageAutopilot { vessel, program } => {
    if !program.is_valid() { return Err(SimError::Recoverable("autopilot: non-finite program")); }
    if !self.vessels.contains_key(vessel) { return Err(SimError::Recoverable("autopilot: unknown vessel")); }
    self.shadow_autopilots(shadow);
    self.autopilots.insert(*vessel, AutopilotState { program: program.clone(), enabled: true, persist: PersistMap::new() });
}
// lib.rs:6930-6935 — silent skip at tick time
if !state.enabled
    || StableDiscriminant::discriminant(&driver.program())
        != StableDiscriminant::discriminant(&state.program)
{ return Ok(()); }
```
**Impact**: With one driver installed (`set_autopilot_driver`, scripts.rs:468-472: "the world runs `run_tick` only when `state.program` matches"), any `EngageAutopilot` carrying a different valid variant of `AutopilotProgram` produces a dead record: the player is told the command applied (it is in `command_log`), the vessel flies uncontrolled, and nothing in `AdvanceReport` hints at it. The skip exists as a "forward-compat seam", but the apply should be the place that refuses what cannot run — this is a fake-feature path.
**Suggestion**: Reject at apply time when the installed driver's program doesn't match: `if self.autopilot_driver.as_ref().map(|d| d.program()) != Some(program.clone()) { return Err(SimError::Recoverable("autopilot: no driver for program")); }` (with `None` driver rejecting too, or documenting the no-engine `LoadScript` precedent if parking is intended).

### [SEVERITY: MEDIUM] Finding 4: `aero_tables_stale` recomputes the full blake3 craft hash for every InBubble vessel on every `step_frame` iteration
**File**: crates/csp-sim/src/lib.rs:9722
**Problem**: The staleness predicate compares the attached table's `graph_hash` against a *freshly computed* `craft_graph_hash(craft)` — a complete `hash_craft` walk (every node transform, every part's curves/sites/tanks/engine spec) through blake3 — and the pre-pass calls it for every InBubble vessel at the top of every `step_bubble_range`, i.e. once per `step_frame` loop iteration. In the steady state (tables fresh, mask matching) the short-circuit order guarantees the expensive arm is the one evaluated: the hash is recomputed only to conclude "not stale".
**Evidence**:
```rust
// lib.rs:9726-9731 — mask check passes in steady state, so the hash is computed:
body.aero.as_ref().is_none_or(|t| {
    t.mask != v.aero_mask
        || v.craft
            .as_ref()
            .is_some_and(|c| t.graph_hash != craft_graph_hash(c))
})
// lib.rs:3453-3458 — invoked per vessel per iteration (see Finding 2 quote)
// lib.rs:9704-9711 — craft_graph_hash = full StateHasher (blake3) walk of the craft
```
**Impact**: During flight this runs every frame: O(InBubble vessels × craft bytes) of hashing per `step_frame` call that proves nothing changed. Under warp it is worse: each loop iteration (every event boundary re-enters `step_bubble_range`) repeats the full sweep — an event-peppered ×10⁵ advance over a populated bubble re-hashes megabytes of craft data per call. Deterministic, but a pure warp-time CPU/alloc stall that scales with vessel count and part count.
**Suggestion**: Cache the craft's graph hash on the vessel (or inside the weld output) and update it at the known craft-mutation sites (spawn, jettison, undock, breakup, latch-merge, reweld); `aero_tables_stale` then compares two u64s. Alternatively store the hash in `CraftDef` at construction.

### [SEVERITY: MEDIUM] Finding 5: Rollback machinery deep-clones whole sub-states per item — full vessel arena per command, full event heap per scheduling event
**File**: crates/csp-sim/src/lib.rs:6600
**Problem**: The per-command/per-event rollback design clones entire collections into each item's throwaway `TickShadow`: (a) `drain_at` creates a fresh `TickShadow` per staged command, and the first vessel-touching apply in each one calls `shadow_vessels`, deep-cloning the *entire* `BTreeMap<VesselId, Vessel>` — every craft graph, parts vec, tank maps, science, reliability records; (b) `fire_due_events` creates a fresh `TickShadow` per popped event, and any apply that schedules (every SOI event does) clones the entire event heap into it; (c) every drained command is cloned into `shadow.drained` — including `SpawnPhysicalVessel`/`ManufactureCraft` commands carrying full `CraftDef`s.
**Evidence**:
```rust
// lib.rs:6600 — fresh shadow per command:
let mut cmd_shadow = TickShadow::capture(self);
// lib.rs:3252-3262 — what the first vessel touch in that shadow costs:
if shadow.vessels.is_none() {
    shadow.vessels = Some(self.vessels.clone());   // full arena deep clone
    shadow.next_vessel_id = Some(self.next_vessel_id);
    shadow.joints = Some(self.joints.clone());
    ...
}
// lib.rs:6570 — every drained command cloned for rollback:
shadow.drained.push((src, entry.clone()));
// lib.rs:7136 — fresh shadow per fired event (heap clone rides its first schedule):
let mut ev_shadow = TickShadow::capture(self);
```
**Impact**: A drain of N vessel-touching commands performs N full-arena deep clones (O(N × arena bytes) per boundary); script-heavy ticks — the exact workload the poll-detection machinery anticipates — issue many commands per tick through `TickIo::issue`. Same-tick event batches (§2.6.5's ~500-object pile-up, up to `MAX_EVENTS_PER_BATCH` = 1024 pops per flush iteration) clone the heap up to once per event. Correctness is unaffected and clones are freed immediately, but at 64 Hz this is measurable allocation churn and stutter that grows with world size.
**Suggestion**: Capture per-item only what the item can touch (per-vessel undo entries keyed by id, heap as a persistent/log structure), or promote the iteration shadow's "first touch" capture to cover subsequent items and record per-item undo logs instead of full snapshots; at minimum skip the arena clone for commands that provably touch no vessel (`SetWarp`, `AdvanceTo`).

## Out-of-scope observations
- `apply_spawn_vessel` (lib.rs:5036-5039) does not resolve SOI residence at spawn — a spawn authored inside a child's SOI stays mis-parented. Self-documented as M5-A backlog, but it is a live correctness gap in the shipped command surface.
- `apply_load_script` (lib.rs:6974-6977) runs `engine.prepare` before any shadow capture; a Fatal rollback of the command leaves phantom prepared state in the engine (construction config is never shadowed). Self-healing on id reuse, but asymmetric with the world-side rollback.
- `step_bubble_range` clones `self.tech.qualifications` every integer tick (lib.rs:3551) — per-tick allocation churn during warp proportional to the qualification table size.

## Verdict
The determinism architecture (epoch anchoring, tick shadows, command-log discipline, restore coherence gates) is genuinely rigorous and mostly correctly implemented — the restore validation and rollback code are among the most careful I have reviewed. But the rigor is uneven: the debris SOI-escape path was never executed end-to-end (Finding 1 bricks worlds and is simultaneously dead code in the shipped data), and the aero staleness repair point quietly reintroduces exactly the partition dependence the design forbids (Finding 2). The remaining findings are cost asymmetries (whole-state clones, per-iteration re-hashing) that will surface as warp stalls at scale. Professional core, non-professional edges: the features at the boundary of tested scenarios are where the real bugs live.
