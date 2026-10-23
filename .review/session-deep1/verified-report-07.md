# Verification — Cluster C07 (Vessel Lifecycle Core)

Report verified: `.review/session-deep1/review-report-07.md`
Sources checked: `crates/csp-sim/src/lib.rs`, `crates/csp-sim/src/joints.rs`,
`crates/csp-sim/src/transfers.rs`, `crates/csp-sim/src/bubble.rs`,
`crates/csp-sim/src/life_support.rs`, `crates/csp-sim/src/economy.rs`,
`crates/csp-sim/src/colonies.rs`, `crates/csp-sim/src/contracts.rs`,
`crates/csp-sim/src/reliability.rs`, `crates/csp-sim/src/maneuver.rs`,
`crates/csp-phys/src/{weld,simple,structural}.rs`, `crates/csp-view/src/{lib,flight}.rs`,
`crates/csp-view/tests/pad_spawn.rs`.

Verdicts: **8 of 8 findings CONFIRMED.** Findings 1 and 2 are real HIGH-severity
defects; Finding 3 is a real (small) physics error — and a cross-report duplicate
of review-report-04's Finding 1, which corroborates it.

---

### Finding 1 — CONFIRMED (HIGH): unshadowed `tracked_vessels`/`transfers` mutations inside `step_bubble_range` break post-Fatal hash identity

Every link in the chain checks out:

- `shadow_vessels` (lib.rs:3017-3030) captures `vessels`, `next_vessel_id`,
  `joints`, `joint_refused`, `next_part_instance_id` — **not** `tracked_vessels`,
  **not** `transfers`. Called once at lib.rs:3211.
- `joints::latch_pass` (lib.rs:3662-3675) receives `&mut self.tracked_vessels`
  (3667) and `&mut self.transfers` (3669). `apply_latch` mutates both:
  `tracked.remove(&sec)` / `tracked.insert(prim)` at joints.rs:1063-1066 and
  `remap_transfer_vessels(transfers, sec, prim)` at joints.rs:1096
  (transfers.rs:1598-1611 — rewrites `ActiveTransfer::from`/`to`). `apply_latch`
  shadows only `science` (joints.rs:967).
- The dead-vessels drain calls `shadow_autopilots` at lib.rs:3695 but mutates
  `self.tracked_vessels.remove(&vid)` at 3697 **without** `shadow_tracked`.
  Every in-bubble death funnels here: impact (vessels.remove at 3586,
  dead_vessels.push 3588), `max_debris` eviction (3649-3651), thermal-root
  removal inside `apply_breakup` (8817-8819). Only the out-of-range
  `remove_vessel` calls `shadow_tracked` (3806).
- Grep over all `shadow_tracked`/`shadow_transfers` call sites confirms none is
  unconditional inside the range: lib.rs:3806 (`remove_vessel`), 4081
  (`SetVesselTracked`), 6451 (command handler), transfers.rs:841/931/984/1014/
  1426/1530 (transfer applies), economy.rs:754 — all reachable *only* through a
  drained command (`drain_at` at 3246, `drain_boundary` at 6784). **Nuance:**
  if a transfer/tracked command applied earlier in the same iteration, the lazy
  capture already fired and the latch mutation *would* roll back — the bug is
  conditional on no such command in the iteration, which is the common case for
  a latch merge.
- `TickShadow::restore` restores both fields only `if let Some`
  (lib.rs:1649-1660); `TickShadow` spans the whole iteration (created at
  lib.rs:6775, restored on any `Err` at 6791/6819/6830/6837/6848).
- Both fields are hashed: `transfers` at lib.rs:6948-6950 via
  `transfers::hash_transfer` — which *does* hash `from`/`to`
  (transfers.rs:701-712) — and `tracked_vessels` at 6951-6954.
- Fatal-capable code runs after the mutations inside the same shadow:
  `joints::lifecycle(...)?` at lib.rs:3681-3688 (Fatal via
  `world_ports`/`body_transforms`, joints.rs:321-322; `pair_capture` `?` at 508),
  every later tick's `drain_at`/`control_tick`/`apply_breakup`/`latch_pass`,
  and the post-loop `apply_surface_impacts` (colonies.rs:1800 `apply_damage_module?`),
  `contract_notify` (contracts.rs:859 `complete_contract?` → `schedule_event`,
  Fatal on seq overflow), `finish_burns` → `apply_to_rails_at` (Fatal propagates
  at lib.rs:3769).

Consequence as claimed: a post-Fatal `state_hash()` differs from the
pre-boundary hash; `tracked_vessels` can name a merged-away/dead vessel that the
restored arena still holds, and an in-flight transfer stays retargeted to `prim`
while `sec` is back in the arena. The report's suggestion (shadow both arenas
before `latch_pass`, shadow tracked before the dead-vessels drain) is correct
and cheap — both helpers are lazy no-ops.

**Verdict: CONFIRMED — real Fatal-restore divergence, HIGH severity.**

### Finding 2 — CONFIRMED (HIGH): roster `CrewLocation`/`CrewStatus` desyncs across the physical crew lifecycle

The restore gate is real and bi-directional, enforced as `Fatal`:

- lib.rs:7753-7761 — roster `CrewLocation::Vessel(v)` requires the vessel to
  exist AND `vessel.crew` to contain the member ("crew aboard an unknown
  vessel", "crew location/seat record disagree").
- lib.rs:7779-7788 — mirror: any `vessel.crew` entry *with* a roster record
  must record `location == Vessel(vid)` ("seated crew's location is not its
  vessel"). The `if let Some(m)` tolerance is correct for id-only `BoardCrew`
  members (transfers.rs:966-995 creates no roster record — verified, consistent
  with report-06's note).

Each claimed desync site verified:

1. **`land_crew` V→V** (transfers.rs:1116-1128): reseats via
   `dst.crew.push(VesselCrew { id, seat })`, never writes `m.location`. The
   colony arms do update the roster (V→C writes `Colony(c)` at 1148, C→V writes
   `Vessel(t)` at 1180). `apply_transfer_crew` (transfers.rs:886-957) admits any
   aboard crew id — roster members qualify. **One routine `TransferCrew` of a
   roster-tracked member makes every later snapshot unloadable** (Fatal at
   7784). CONFIRMED.
2. **`apply_jettison_part`** (lib.rs:5591): `v.crew.retain(|c| c.seat != node)`
   — a roster member in the jettisoned seat vanishes silently; roster keeps
   `Active`@`Vessel(v)` → Fatal at 7757, and no `Lost` marking (the
   `fire_vessel_ls_depleted` precedent at life_support.rs:377-392, which IS the
   only crew-loss path maintaining the pairing: `status = Lost`,
   `location = Unassigned`). CONFIRMED.
3. **`apply_breakup`** (lib.rs:8883): `v.crew.retain(...)` on the retained side;
   crew on exploded nodes vanish; crew on detached subtrees ride to fragment
   vessels (8775-8779) while the roster keeps `Vessel(old)` → both directions
   of the gate trip. CONFIRMED.
4. **`apply_undock`** (lib.rs:5781 → `split_vessel_records`,
   transfers.rs:1616-1638): fragment crew keep `location = Vessel(old)`; the
   retained vessel no longer carries them → Fatal. CONFIRMED.
5. **`apply_latch`** (joints.rs:946 → `merge_vessel_records`,
   transfers.rs:1571-1592): absorbed crew pool into `prim` with remapped seats;
   roster keeps `Vessel(sec)`; `sec` is removed at joints.rs:1084 →
   "crew aboard an unknown vessel". CONFIRMED.
6. **`remove_vessel` + all in-bubble removals** (lib.rs:3801-3839, plus DebrisDecay/
   SOI-escape/impact paths): none touches `self.economy.crew` — roster members
   aboard a destroyed vessel stay `Active`@`Vessel(dead)` → Fatal, plus salary/
   status lying in the meantime. CONFIRMED. (The recovery path at
   economy.rs:898-904 shows the intended unassign on removal — recovery only.)

Reachability confirmed: `TransferColonyCrew{to_colony:false}` → `land_crew`
C→V arm writes `location = Vessel(t)` (transfers.rs:1180); from there any of
the six paths applies.

**Verdict: CONFIRMED — HIGH. The report's "minimum fix: land_crew V→V, then
jettison/breakup/undock/latch/removal" is the right ordering.**

### Finding 3 — CONFIRMED (MEDIUM): `surface_pose_restore` stamps the origin's `ω×r` into the CoM-velocity slot

- bubble.rs:245: `let vel = env.body_spin.cross(pos - env.body_center);` — `pos`
  is the body *origin* (`SurfacePose` records origin lat/lon/alt;
  `surface_pose_capture` uses `body.pos`, bubble.rs:192).
- `WeldedBody::vel` is CoM velocity: documented at simple.rs:26-35 (which names
  this exact `ω×(q·com_local)` confusion as a bug class it fixed), the
  integrator advances `com = b.com_world() + b.vel * PHYSICS_DT` (simple.rs:1331),
  `com_world() = pos + rot·com_local` (weld.rs:177-178), and
  `SimCommand::SpawnPhysicalVessel.vel` is documented "CoM velocity"
  (lib.rs:553). Every split/merge stamp treats it as CoM (`fragment_velocity`,
  structural.rs:345; jettison correction, lib.rs:5576).
- Correct value: `ω × (com_world − body_center) = ω×(pos−center) +
  ω×(rot·com_local)`. The term is missing, and the signature `(env, pose, tick)`
  cannot supply `com_local` — the report's structural explanation is exact.
- Call sites: `unlatched_body` (lib.rs:3113-3118 → `body.vel`), csp-view pad
  spawn (csp-view/src/lib.rs:1355-1366 → `SpawnPhysicalVessel.vel`), and
  csp-view/src/flight.rs:426 (render-only `FlightBody.vel` — harmless).
- Magnitude: ‖ω‖·‖com_local‖ ≈ 7.3e-5 m/s per metre at Earth spin (the report's
  "1.5e-4" is within 2× — order-of-magnitude right, impact description
  accurate: deterministic, small, silently consistent since `ang_vel` is
  correct). The pad-spawn regression test's `< 1.0 m/s` surface-speed bound
  (pad_spawn.rs:54-58, using the *CoM* `ω×r` as reference — the correct value)
  cannot catch a 0.07 mm/s error, so nothing guards it.
- Also filed as review-report-04 Finding 1 — an independent derivation agreeing
  verbatim. Corroborating, not padding.

**Verdict: CONFIRMED — real physics error, honestly MEDIUM/LOW impact; fix per
the suggestion (pass `com_local` or post-fix `body.vel` at each consumer).**

### Finding 4 — CONFIRMED (LOW): `latched_total` accumulates duplicates and non-transitions

- `latched_total: Vec<VesselId>` (lib.rs:3223); `latched_total.extend_from_slice`
  (3485); comment at 3708 claims "dedup'd by BTreeSet order" — false, it's a Vec.
- `settle.latched` persists while `InBubble` (set at simple.rs:1963; cleared only
  by `throttle > 0`, simple.rs:1998-2000) → a latched supporter held InBubble
  re-pushes into `latched_now` every tick via lib.rs:3471's `else if`.
- `transition_latched` skips (no transition) when a still-moving body rests on
  the vid (lib.rs:8474-8475, `bubble_vessel_contact(..., moving_only=true)`) —
  those vids enter `latched_total` having never latched, then re-enter as
  duplicates.
- Benign today: `contract_notify(SurfaceLatched)` re-checks
  `v.state == SurfaceLatched` before inserting the fact (contracts.rs:840-848)
  and `objective_met` re-verifies — wasted trigger evaluation + a wrong comment
  only. CONFIRMED as described (LOW).

### Finding 5 — CONFIRMED (LOW): `script_log` survives Fatal rollback

- `TickShadow` has no `script_log` field (struct ends at `notifications`,
  lib.rs:1573). `self.script_log.push` at lib.rs:6182-6219 during
  `control_tick`; drained only on success via `std::mem::take` at 6869. The
  Fatal path (`shadow.restore(self); return Err(e)` at 6819-6820 et al.) leaves
  the buffer intact → rolled-back effects (strikes, disables, poll demotions)
  leak into the next successful `AdvanceReport`; unbounded accumulation across
  repeated Fatals. CONFIRMED (LOW, user-facing only).

### Finding 6 — CONFIRMED (LOW): stale thrust factor in Δv accounting; zero-thrust burns wedge

- `drive_node_burn` (lib.rs:3304) reads `maneuver::net_thrust_axis(body)`
  (maneuver.rs:212-220 — sums the *current* `site.rated_n`, i.e. last tick's
  `thrust_factor`-scaled values) to compute `full = f_max·dt/m` and decrement
  `dv_remaining_mps` (lib.rs:7056-7063); `apply_thrust_factors` re-scales for
  the tick only at lib.rs:3311-3313 (reliability.rs:1460-1489). One-tick-stale
  Δv bookkeeping — real, self-correcting, small.
- Zero-thrust wedge real: `f_max = 0` → `throttle = 0; return false` forever
  (lib.rs:7050-7053) → `burns_done` never collects the vid → node stays
  `Burning` → `SetManeuver` rejects "burn in progress" (lib.rs:5065-5071) →
  only manual `DeleteManeuver`/`ToRails` recovers the slot. CONFIRMED.

### Finding 7 — CONFIRMED (LOW, policy question): transfer/aero/tracking ops bypass the control lease

- `control_gated_vessel` (lib.rs:1144-1161) covers
  SetThrottle/SetAttitude/Unlatch/ToRails/ToBubble/SetAeroConfig/JettisonPart/
  SetManeuver/DeleteManeuver/Undock/LoadScript/EngageAutopilot/DisengageAutopilot
  — everything else returns `None`, including `TransferResource`,
  `TransferCrew`, `TransferColonyResource`, `TransferColonyCrew`, `BoardCrew`,
  `InstallAeroTables`, `SetVesselTracked`, `SetLifeSupportEnabled`,
  `RecoverVessel`.
- The doc *does* state intent ("resource/crew moves ... is ungated: it does not
  steer the craft", lib.rs:1131-1133), so this is documented asymmetry rather
  than an omission bug — but the inconsistency the report names is real:
  resource transfers change mass (the burn executor and contact physics read
  it), `InstallAeroTables` rewrites a force model `SetAeroConfig` is gated on,
  and `RecoverVessel` deletes the vessel outright. Same observation as
  review-report-04. CONFIRMED as "worth confirming intent" — correctly framed
  as policy, not defect.

### Finding 8 — CONFIRMED but vacuous (LOW): `ensure_aero` precedes `shadow_vessels`

- lib.rs:3202-3210 mutates `body.aero` via `ensure_aero` before `shadow_vessels`
  at 3211 — a real capture-order violation.
- `body.aero` is unhashed (`hash_vessel`, lib.rs:9075-9095, walks only
  pos/rot/vel/ang_vel/throttle/settle), never serialized-relevant, and
  self-healing through `aero_tables_stale` (lib.rs:8410). Vacuous today; the
  suggested one-line move is still right hygiene. CONFIRMED.

---

## "Verified clean" section — spot-checked, consistent

- Drain semantics: per-command `TickShadow` + `seq_restore` rollback verified
  (lib.rs:5873-5917); two-phase pop/apply and `drained` FIFO rebuild in
  `TickShadow::restore` (1624-1626) as described.
- `apply_breakup`: ancestor-first `(depth, id)` sort at 8626-8629,
  `removed`/`assigned` guards, `≥64` mask edge — all as claimed.
- `apply_undock`: seam partition (5738-5758), `fragment_velocity` + jettison
  ω×Δcom corrections (5669-5684) present.
- Soft-capture: cap→refuse+Recoverable (joints.rs:512-523), refusal pruning,
  one-joint-per-pair, index-restart in `latch_pass` (1137-1155), p/L
  conservation about merged CoM (joints.rs:902-922) — all verified.
- `remove_vessel` shadow order (3805-3833) and `physics.aero`'s
  derived-never-hashed status confirmed.

## Notes for the synthesis pass

- Findings 1+2 compound: `apply_latch` is the single site that produces *both*
  bugs at once (unshadowed `tracked`/`transfers` AND roster `Vessel(sec)` left
  dangling). A latch-merge + any later Fatal or snapshot is a real-world
  trigger for both.
- Finding 2's `land_crew` V→V arm is the cheapest trigger: an ordinary
  `TransferCrew` command (no mods, no edge cases) permanently corrupts the
  next save — arguably the highest user-visible severity in the cluster.
- Finding 3 duplicates review-report-04 Finding 1 verbatim — deduplicate in the
  synthesis, keep one entry citing both.
