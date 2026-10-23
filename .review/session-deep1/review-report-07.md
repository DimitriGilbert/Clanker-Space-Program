# Cluster C07 — Vessel Lifecycle Core Deep Review

Scope: `crates/csp-sim/src/lib.rs` L3017–5974 (shadow machinery, `step_bubble_range`,
`finish_burns`, `remove_vessel`, debris decay, `apply_command` + handlers: spawn
physical/rails, maneuver, aero install, jettison, undock, `drain_boundary`/`drain_at`),
plus `crates/csp-sim/src/joints.rs` (soft-capture lifecycle, cap-16, refusal, latch
merge) and `crates/csp-sim/src/transfers.rs` (resource/crew transfer process), with
the referenced helpers (`transition_latched`, `apply_breakup`, `drive_node_burn`,
`surface_pose_*`, `TickShadow`, `event_names_vessel`, restore coherence gates) read
for context.

Findings are ordered by severity.

---

### [HIGH] Finding 1: Latch merge and in-bubble deaths mutate `tracked_vessels` and `transfers` without shadow capture — Fatal rollback diverges hashed state

**File:** `crates/csp-sim/src/lib.rs:3211, 3662-3675, 3695-3701`; `crates/csp-sim/src/joints.rs:1063-1066, 1096`

**Problem:**
`step_bubble_range` captures the vessel arena (and, riding with it, `joints`,
`joint_refused`, `next_vessel_id`, `next_part_instance_id`) once at line 3211 via
`self.shadow_vessels(shadow)`. Inside the borrow-split tick, `joints::latch_pass` →
`apply_latch` also mutates two *other* hashed+serialized arenas:

- `tracked_vessels` — `tracked.remove(&sec)` / `tracked.insert(prim)` (joints.rs:1063-66)
- `transfers` — `remap_transfer_vessels(transfers, sec, prim)` (joints.rs:1096)

Neither `self.shadow_tracked` nor `self.shadow_transfers` is ever called inside the
range. The same gap exists in the post-loop dead-vessels drain
(`self.tracked_vessels.remove(&vid)` at lib.rs:3697 — `shadow_autopilots` is called at
3695 but `shadow_tracked` is not). `remove_vessel` (the only out-of-range removal
path) does call `shadow_tracked` at 3806 — the inline in-bubble removals (impacts at
3586, `max_debris` evictions at 3649, thermal-root removal inside `apply_breakup` at
8817, and `apply_latch`'s absorb) all feed `dead_vessels`, so the gap is reachable on
every in-bubble death path, not just latch.

**Evidence:**
`TickShadow::restore` only restores captured fields (lib.rs:1649-1660: `tracked_vessels`,
`transfers` restore only `if let Some`). Both fields are hashed at `state_hash`
(lib.rs:6948-6951). Fatal-capable code runs after these mutations in the same
iteration: `joints::lifecycle`'s `world_ports`/`body_transforms` (Fatal on corrupt
graph), the next tick's `drain_at`/`control_tick`, `finish_burns`,
`apply_surface_impacts`, `contract_notify`. `shadow.restore(self)` at lib.rs:6819-6820
then rolls back `vessels` but leaves `tracked`/`transfers` mutated.

**Impact:**
Post-restore `state_hash()` is no longer the pre-boundary hash — the §2.13 rollback
contract is broken. Concretely: after a Fatal, `tracked_vessels` can name a vessel
that (restored) never merged/died, and an in-flight `ActiveTransfer` stays retargeted
to the merged id `prim` while the arena still contains `sec` — the completion lands
its cargo on the wrong vessel after the error is handled, and a replay of the same
iteration diverges.

**Suggestion:**
Call `self.shadow_tracked(shadow)` and `self.shadow_transfers(shadow)` in
`step_bubble_range` before `joints::latch_pass` when `!self.joints.is_empty()` (cheap:
lazy no-op when the fields are untouched), and `self.shadow_tracked(shadow)` before
the `dead_vessels` drain at line 3695.

---

### [HIGH] Finding 2: Roster `CrewLocation`/`CrewStatus` desyncs across the physical crew lifecycle — snapshot restore Fatals and silent crew loss

**File:** `crates/csp-sim/src/transfers.rs:1108-1128`; `crates/csp-sim/src/lib.rs:5591, 5781, 8881-8883`; `crates/csp-sim/src/joints.rs:946`; restore gate at `crates/csp-sim/src/lib.rs:7748-7785`

**Problem:**
The roster↔seat coherence gate at restore enforces both directions: a roster member
with `CrewLocation::Vessel(v)` must exist in `v.crew`, and any `vessel.crew` entry
with a roster record must record `Vessel(v)` (else `Fatal`, not `Recoverable`).
Several physical-crew paths move or kill crew records without updating
`economy.crew`:

1. **`land_crew` Vessel→Vessel** (transfers.rs:1116-1128): reseats the member on `t`
   but never sets `m.location = CrewLocation::Vessel(t)` — the Colony arms *do*
   update the roster (1148, 1180). After a routine `TransferCrew`, the member sits
   aboard `t` while the roster says `Vessel(f)` → restore Fatal
   "crew location/seat record disagree".
2. **`apply_jettison_part`** (lib.rs:5591): `v.crew.retain(|c| c.seat != node)` — a
   roster member in the jettisoned seat silently dies; roster keeps
   `Active`@`Vessel(v)` → restore Fatal + the member isn't marked `Lost` (the
   life-support path's precedent at life_support.rs:386-391).
3. **`apply_breakup`** (lib.rs:8881-8883): `v.crew.retain(|c| graph.nodes.contains_key(&c.seat))` — crew on exploded/departed nodes vanish the same way.
4. **`apply_undock`** (lib.rs:5781, `split_vessel_records`): crew riding the fragment
   keep `location = Vessel(old)` — the retained vessel no longer contains them →
   Fatal.
5. **`apply_latch`** (joints.rs:946, `merge_vessel_records`): the absorbed vessel's
   crew pool into `prim`; the roster keeps `Vessel(sec)` and `sec` is removed →
   "crew aboard an unknown vessel" Fatal.
6. **`remove_vessel` + all in-bubble removals** (impact, eviction, thermal-root,
   `DebrisDecay`, SOI escape): roster members aboard a destroyed vessel keep
   `Active`@`Vessel(dead)` forever → same Fatal.

**Evidence:**
Roster members aboard a vessel is reachable: `apply_assign_crew` gives
`location = Colony` (colonies.rs:1644), `TransferColonyCrew{to_colony:false}` lands
them and writes `location = Vessel(t)` (transfers.rs:1180). From there any of the
six paths above applies. `fire_vessel_ls_depleted` (life_support.rs:377-392) shows
the intended bookkeeping — it is the *only* path that maintains the pairing on crew
loss.

**Impact:**
Save/restore breaks with a `Fatal` after ordinary play (a routine crew transfer,
undock, or vessel loss makes every later snapshot unloadable); the roster also lies
in the meantime (member reports aboard a dead/absorbed vessel, `Active` instead of
`Lost` — salary may still accrue).

**Suggestion:**
Mirror the `fire_vessel_ls_depleted` discipline at every site that mutates
`v.crew`: update `economy.crew` (`location = Vessel(new)` when the member travels,
`status = Lost` + `location = Unassigned` when the member dies with its
part/vessel), with `self.shadow_economy(shadow)` first. Minimum fix: `land_crew`'s
V→V arm; then jettison/breakup/undock/latch/removal.

---

### [MEDIUM] Finding 3: `surface_pose_restore` stamps the body *origin's* co-rotation velocity into the CoM-velocity slot — missing `ω×(rot·com_local)`

**File:** `crates/csp-sim/src/bubble.rs:227-247` (line 245); callers `crates/csp-sim/src/lib.rs:3113-3118` (`unlatched_body`), `crates/csp-view/src/lib.rs:1355-1366` (pad spawn), `csp-view/src/flight.rs:426`

**Problem:**
`WeldedBody::vel` is the **CoM** velocity — the integrator advances
`com_world() = pos + rot·com_local` by `vel·dt` (simple.rs:1331-1341, comment
"`vel` is the CoM velocity"), and every split/merge stamp treats it so
(`fragment_velocity`, jettison correction, latch merge). `surface_pose_restore`
returns `vel = env.body_spin × (pos − body_center)` — the co-rotation velocity of
the *origin material point*, not the CoM's. The correct CoM co-rotation velocity is
`ω × (com_world − body_center)` = `ω×(pos−center) + ω×(rot·com_local)`.

**Evidence:**
For a rigidly surface-fixed body, every material point `p` has `v = ω×r(p)`. The
stamped state makes the CoM move at `ω×r(origin)`, so the body slides relative to
the surface at `ω×(rot·com_local)` — the same "w×r" bug class just fixed in the pad
spawn, one level deeper. The function takes only `(env, pose, tick)` — it cannot
know `com_local`, which is why the term is missing at every call site.

**Impact:**
Deterministic but physically wrong: pad spawns and unlatched bodies on rotating
worlds creep across the surface at `‖ω‖·‖com_local‖` (~1.5e-4 m/s per metre of CoM
offset at Earth spin — ~0.5 m/hour for a 1 m offset; much more on fast-spinning
test bodies). Small today, but it is exactly the airborne/stale-velocity class of
spawn bug the recent fix targeted, and `ang_vel` *is* correct so the slide is
silently consistent, not obviously wrong.

**Suggestion:**
Give `surface_pose_restore` the body's `com_local` (or the `WeldedBody`) and return
`vel = ω × (pos + rot·com_local − body_center)`; update `unlatched_body` and the
csp-view pad spawn to pass it.

---

### [LOW] Finding 4: `latched_total` accumulates duplicates and non-transitions — `SurfaceLatched` contract trigger fires spuriously per tick

**File:** `crates/csp-sim/src/lib.rs:3223, 3458-3479, 3484-3485, 3707-3712`

**Problem:**
A `settle.latched` body held `InBubble` as a static supporter (a moving body rests
on it) re-pushes its vid into `latched_now` on *every* tick it stays in contact
(line 3471's `else if body.settle.latched` — it stays latched while `InBubble`).
`latched_total.extend_from_slice` accumulates the duplicates, and vids whose
transition was *skipped* this tick (still supporting a neighbor) are added too —
they haven't latched. The comment at 3708 claims "dedup'd by BTreeSet order" but
`latched_total` is a `Vec`. Each entry then drives one
`contract_notify(SurfaceLatched(vid))`.

**Evidence:**
`transition_latched` (8460-8511) skips vids still in `bubble_vessel_contact` — those
vids are already in `latched_total`, and re-enter `latched_now` on subsequent ticks.

**Impact:**
Benign today: the trigger inserts into a set (`bodies_visited`) and `objective_met`
re-checks `v.state == SurfaceLatched`, so no contract completes falsely — but it is
wasted trigger evaluation per supporter-tick, the "dedup'd" claim is wrong, and any
future non-idempotent trigger effect would multiply.

**Suggestion:**
Collect `latched_total` in a `BTreeSet`, or push vids only when `transition_latched`
actually transitions them (the current state check inside `contract_notify` makes a
post-loop `v.state == SurfaceLatched` filter equivalent).

---

### [LOW] Finding 5: `script_log` survives Fatal rollback — aborted-iteration log lines leak into the next `AdvanceReport`

**File:** `crates/csp-sim/src/lib.rs:1942, 6182-6212, 6869`

**Problem:**
`self.script_log` accumulates strike/disable/poll lines during `control_tick` but is
neither shadowed nor cleared on `shadow.restore`. It is drained into the report only
on success (`std::mem::take` at 6869). A Fatal mid-range returns `Err` with the
buffer intact, so the next successful `step_frame` emits lines describing effects
that were rolled back.

**Impact:**
User-facing only: misleading log entries (a "script disabled" line for a disable
that never persisted) plus unbounded accumulation across repeated Fatals. Minor.

**Suggestion:**
Clear `script_log` on the Fatal-restore path in `step_frame`, or capture it in
`TickShadow` like `command_log`'s `log_len`.

---

### [LOW] Finding 6: Burn executor reads thrust sites before `apply_thrust_factors` refreshes them — one-tick-stale factor in Δv accounting; zero-thrust burns never terminate

**File:** `crates/csp-sim/src/lib.rs:3304-3313, 7045-7065`

**Problem:**
Each tick: `drive_node_burn` (3304) reads `maneuver::net_thrust_axis(body)` — the
thrust sites carrying *last* tick's `thrust_factor` — to compute
`full = f_max·dt/m`, then `apply_thrust_factors` (3311) re-scales the sites for this
tick's step. `dv_remaining` decrements by the stale value while physics integrates
with the fresh one — a misfire/degradation landing this tick still counts full Δv.
Worse: if the burn's last engine departs (breakup/jettison), `net_thrust_axis` →
`f_max = 0` → `drive_node_burn` returns `false` forever → the node is permanently
`Burning`: `SetManeuver` rejects "burn in progress — delete it first", and only a
manual `DeleteManeuver`/`ToRails` recovers the node slot.

**Impact:**
One-tick Δv bookkeeping error at factor-change ticks (self-correcting, small); a
permanently wedged maneuver slot after engine loss — user-visible stall requiring
manual intervention.

**Suggestion:**
Run `apply_thrust_factors` before `drive_node_burn` (or move the factor read inside
it); consider auto-completing/cancelling a `Burning` node whose available thrust is
zero for N ticks (the "honest ballistic outcome" convention `finish_burns` already
documents).

---

### [LOW] Finding 7: `TransferResource`/`TransferCrew`/`InstallAeroTables`/`SetVesselTracked` bypass the control lease while `Undock`/`JettisonPart`/`SetThrottle` are gated

**File:** `crates/csp-sim/src/lib.rs:1144-1159` (`control_gated_vessel`)

**Problem:**
The lease gate covers control-affecting commands but omits the transfer family and
aero/tracking ops. A non-leaseholder automation (`External` source under a `Script`
lease, etc.) can drain a leased vessel's tanks or move its crew — mutations that
change the vessel's mass and therefore its control physics — while `Undock`,
`JettisonPart`, and even `SetAeroConfig` are gated. Policy inconsistency worth
confirming; if transfers are intentionally ungated, the asymmetry should be
documented.

**Suggestion:**
Confirm intent; if transfers are control-affecting, add the `Transfer*`/`BoardCrew`
variants (gate on `from_vessel`/`vessel`) to `control_gated_vessel`.

---

### [LOW] Finding 8: `ensure_aero` runs before `shadow_vessels` in `step_bubble_range` — unshadowed `body.aero` attach survives a Fatal

**File:** `crates/csp-sim/src/lib.rs:3202-3211`

**Problem:**
The stale-table pre-pass attaches `body.aero` via `ensure_aero` at 3208-3210, then
`shadow_vessels` captures the arena at 3211. Any aero attach therefore survives a
Fatal restore — technically a write before the first capture. Impact is vacuous
today (`body.aero` is derived from craft+mask, unhashed at 9075-9095, and
self-healing via the staleness check), but it breaks the "capture before first
mutation" discipline every other writer follows — if the attached table ever
becomes hashed or serialized-relevant, this silently diverges.

**Suggestion:**
Move `self.shadow_vessels(shadow)` above the `needing` loop.

---

## Verified clean (no finding)

- **Drain semantics** (`drain_at`/`drain_boundary`): two-phase pop/apply is correct —
  `seq_restore` rolls back rejected commands' `source_seq`, per-command `TickShadow`
  isolates partial effects, `last_src` grouping preserves `(class, source_id)` order,
  and Fatal re-queues drained commands in exact FIFO order. One-drain-per-tick-value
  holds (`now` drains the emptied queue; `t_next` exclusive).
- **Breakup partitioning** (`apply_breakup`, lib.rs:8540-8968): ancestor-first
  depth sort + `removed`/`assigned` guards prevent double-claiming nested failures;
  thermal-root → whole-vessel destruction path is total; mass/momentum stamps match
  the jettison/fragment conventions; the `≥64` `ConfigMask` edge is handled.
- **Undock split** (lib.rs:5631-5855): seam partition drops straddling seams on both
  sides; ids preserved by `fragment_graph_of`; CoM/`ω×Δcom` velocity corrections
  present; contents/crew/science/reliability partition by node (roster caveat in
  Finding 2).
- **Soft-capture lifecycle** (joints.rs): cap-16 degrade→refuse+`Recoverable` matches
  §2.4 verbatim; refusal pruning prevents re-capture storms; per-pair one-joint
  limit; ascending-pair detection order; anchors re-posed each tick (re-weld safe);
  latch merge conserves `p` and `L` about merged CoM; index-restart discipline in
  `latch_pass` is sound.
- **Spawn paths**: `apply_spawn_physical` validation (finiteness, tank-mass,
  part_costs parallelism, reliability table, funds gate) is ordered
  validate→shadow→allocate→charge→insert; the csp-view pad spawn now uses
  `surface_pose_restore` + `surface_upright` correctly (modulo Finding 3's CoM term);
  `SpawnVessel`'s elements are validated against `state_at` before allocation.
- **`finish_burns`**: epoch-`t_next` stamping is correct for mid-range completion;
  post-stamp failure republishes `SoiRescan` (the `fire_due_events` fallback);
  pre-stamp refusals skip the republish correctly.
- **Transfer atomicity** (`transfers.rs`): debit-at-apply + reservation-net capacity
  is consistent in both vessel and colony directions; completion's
  destination→source→drop totality is total and deterministic; `TransferComplete`
  survives vessel removal by design (restore gate verified); quantum-bucketed mass
  deltas are idempotent.
- **`debris_since`/`DebrisDecay`**: epoch-vs-clock scheduling is handled correctly
  (the doc'd `epoch` parameter); `max_debris` eviction is deterministic by
  `(debris_since, id)`.
- **`resolve_soi_residence`**: reverse-transition suppression bounds the walk;
  `body_orbit` on the root is safe (`soi_m: None` → no exit).
- **In-bubble removal event invariant**: `event_names_vessel`'s deliberate
  `VesselLifeSupportDepleted` exclusion is consistent with the tolerated-no-op
  firing path; `debug_assert`s document the rails-only invariant.
- `physics.aero` is explicitly derived/never hashed/never shadowed — the unshadowed
  cache inserts in spawn/undock/latch are fine.
