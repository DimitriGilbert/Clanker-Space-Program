# Verified Report — Cluster C04 (verification of `review-report-04.md`)

**Method:** every cited line/range was re-read against the working tree
(`crates/csp-sim/src/lib.rs` — 17,547 lines, so some cited line numbers
drifted ±10; each anchor was located by content and re-confirmed),
`crates/csp-sim/src/events.rs`, `crates/csp-sim/src/bubble.rs`,
`crates/csp-phys/src/simple.rs`, `crates/csp-phys/src/structural.rs`,
`crates/csp-script/src/lib.rs`, `crates/csp-ai/src/lib.rs`. Call sites and
orderings were traced, not just the cited lines.

**Verdict summary: all 9 findings CONFIRMED.** Line drift noted where the
cited number no longer matches; no factual errors found in the report.

---

## Finding 1 — `surface_pose_restore` vel is origin-not-CoM: **CONFIRMED**

- `bubble.rs:245`: `let vel = env.body_spin.cross(pos - env.body_center);`
  — the co-rotation velocity of the body **origin** `pos`, verbatim.
- `body.vel` is CoM velocity: `simple.rs:26-35` module doc
  ("`vel` is the CoM velocity … `x̄ += v·dt` … `pos = x̄ − q·com_local`"),
  and the integrator applies it at `com_world()`
  (`simple.rs:1331-1342`: `com = b.com_world() + b.vel * PHYSICS_DT;
  b.pos = com - b.rot.rotate(b.com_local)`).
- Call site `lib.rs:3111-3118` welds the body first — `body.com_local`
  is in scope and unused — then `body.vel = vel` verbatim.
- The correct value is `ω × (com_world − c) = ω × (pos − c) +
  ω × (rot·com_local)`; the stored value drops the second term.
- Corroborating convention evidence beyond the report's citations:
  - `SpawnPhysicalVessel` doc: "CoM velocity, bubble frame" (`lib.rs:553`).
  - Telemetry's osculating elements pair `body.com_world()` with
    `body.vel` (`lib.rs:7306-7310`).
  - Jettison corrects `vel` by `ω×Δcom` when `com_local` moves, with a
    comment naming exactly this bug class (`lib.rs:5568-5576`).
  - `structural.rs:345` (`fragment_velocity`): fragment CoM velocity is
    `parent.vel + ω_world×(frag_com − parent.com)` — the codebase's own
    rigid-body convention, which `surface_pose_restore` violates.
  - Even the test helpers encode it: `surface_vel(b.com_world(), env)`
    for the expected co-rotation state (`lib.rs:11060, 11282`).

## Finding 2 — `apply_to_rails_at` conic anchored at origin: **CONFIRMED**

- `lib.rs:4740`: `let (r, v2) = (body.pos - env.body_center, body.vel);`
  → `orbit::cartesian_to_elements(r, v2, …)` — origin `r`, CoM `v`, a
  state belonging to no single material point.
- The atmosphere gate in the *same* function uses `body.com_world()`
  (`lib.rs:4723`); so do the aero/LS altitude checks (`3003, 3345,
  4617`) and telemetry (`7307`).
- Return leg `apply_to_bubble` (`lib.rs:4998-4999`) and the executor's
  node-start materialization (`5231-5232`) stamp `body.pos = r +
  center` — the conic point is treated as the origin's. Round-trip is
  self-consistent (origin → conic → origin); the error is vs. the true
  CoM conic, magnitude `O(|com_local|)`, and the `r_p > radius_m`
  clearance check (~`4746`) evaluates that wrong conic.

## Finding 3 — disabled-script lease lockout: **CONFIRMED**

- Strike-disable (`lib.rs:~6180-6196`): `script.enabled = false` +
  `disabled_reason`; no `control_lease` write.
- `SetScriptEnabled` apply (`lib.rs:4211-4223`): `s.enabled =
  *enabled`; no lease write.
- `apply_unload_script` (`lib.rs:~6331-6353`) *does* vacate to
  `Lease::Player` with the "dead script cannot hold a lease" comment —
  the asymmetry the finding rests on is real and intentional-looking.
- A disabled script can never act: `control_tick` filters `s.enabled`
  (`6102`), `fire_script_wake` requires `s.enabled` (`6375`),
  `run_script_tick` early-returns on `!enabled` (`6140`).
- Who can free it: `apply_take_control` (`~4301-4315`) allows only
  `me == Player` (and `lease_identity` maps only `SourceClass::Ui →
  Player`, `~458`), `me == current`, or vacant self-acquire
  (`current == Player` — false here). `UnloadScript`/`SetScriptEnabled`
  are gated on the bound vessel (`~4033-4040`), so a non-holder cannot
  even unload the dead script to trigger the vacate.
- Net: with no `Ui` producer (headless/External-only), a vessel whose
  lease is `Script(disabled_id)` is permanently uncontrollable by every
  source — confirmed, and it contradicts the invariant the unload path
  states in a comment.

## Finding 4 — `Lease::Script` u64→u32 asymmetry + unvalidated
## `TakeControl` target: **CONFIRMED**

- Producer clamp: `csp-script/src/lib.rs:1030`
  `script.id.0.min(u64::from(u32::MAX)) as u32`.
- Gate truncate: `check_control_lease` `src == (SourceClass::Script,
  id.0 as u32)` (`lib.rs:~4280`) — wraparound `as`, not clamp.
- Identity exact: `lease_identity` zero-extends —
  `Lease::Script(ScriptId(u64::from(source_id)))` (`lib.rs:~458`).
- `apply_take_control` (`~4296-4324`) contains no check that
  `Script(id)`/`Agent(x)` names an existing entity; `Ui` installs any
  `Lease` unconditionally.
- Consequences as stated: `TakeControl{lease: Script(2³²+k)}` hands the
  gate to unrelated script `k` while a hypothetical `2³²+k` script
  (which would stamp `u32::MAX`) can never pass; `TakeControl{lease:
  Script(9999)}` with no such script wedges the vessel exactly like
  Finding 3 (only `Ui` frees it). Confirmed.

## Finding 5 — lease gate excludes destructive/extractive commands:
## **CONFIRMED** (as the contract gap it was filed as)

- `control_gated_vessel` (`lib.rs:1144-1161`) covers exactly the 12
  listed variants; `apply_command` adds only `UnloadScript` /
  `SetScriptEnabled` via bound vessel (`~4033-4040`).
- Verified ungated-against-the-leased-vessel, each re-checked:
  `RecoverVessel` (doc `861-866`: "the vessel leaves the sim",
  `site: None` = remote salvage), `TransferResource`/`TransferCrew`/
  `BoardCrew`/`TransferColonyResource`/`TransferColonyCrew` (all in the
  enum, none in the gate), `TransmitData`, `SetVesselTracked`
  (`4077-4104` — confirmed it does rewrite pending `SoiTransition`
  classes, a real warp-behaviour change), `SetLifeSupportEnabled`,
  `InstallAeroTables`.
- The finding's own caveat stands: this may be deliberate
  (lease = steering authority). The doc text "the drain checks every
  control-affecting command" is broader than the enforced set; flagging
  for contract confirmation is the correct disposition.

## Finding 6 — `ScriptWake` double-ticks a bubble-resident script:
## **CONFIRMED**

- Registration: `request_wake` (`7374-7404`) → `resolve_wake`
  (`7166+`) — `AtTick`/`InTicks` have no residence guard; only
  apsis/SOI/maneuver conditions consult `v.orbit()`/`v.maneuver`.
- Firing: `fire_script_wake` (`~6360-6383`) checks only
  `s.enabled && s.wake_tick == Some(tick)` — no residence check —
  then `run_script_tick(id, event.tick)` → `engine.run_tick(script,
  event.tick, …)`.
- Ordering traced in `step_frame` (`6760+`): `t_next` clamps to the
  event tick `w`; `step_bubble_range(now, w)` covers `t ∈ now..w`
  (excludes `w`); `fire_due_events(w)` runs `on_tick(w)`. Next
  iteration: `step_bubble_range(w, t₂)` → `control_tick(w)` —
  filter `s.enabled && InBubble` only (`6097-6104`) — runs
  `on_tick(w)` **again** with the same tick value.
- Nothing marks the tick consumed for the control pass; the script's
  own `wake_tick` bookkeeping (`Set`/`Cleared`) is unrelated to
  `control_tick`. Confirmed: same nominal tick processed twice for an
  enabled, InBubble, wake-registered script.

## Finding 7 — `TickIo::issue` trusts the engine's `SourceClass` stamp:
## **CONFIRMED**

- `WorldTickIo::issue` (`lib.rs:7367-7372`) pushes `(class, source_id)`
  verbatim into `pending`; `check_control_lease` returns `Ok`
  unconditionally for `SourceClass::Ui` (`~4272-4273`).
- Real engines stamp honestly (`csp-script:1032` → `Script`,
  `csp-ai:201` → `Autopilot`, both with the `min(u32::MAX)` clamp), so
  this is a trust-boundary gap, not a live exploit — exactly the LOW
  severity filed.

## Finding 8 — `AdvanceReport::rejected` keys alias `command_log` keys:
## **CONFIRMED**

- `drain_at` (`lib.rs:~5900-5961`): `key` is built from the pre-increment
  `source_seq`; on non-Fatal failure `cmd_shadow.restore(self)` puts the
  counter back (`TickShadow::restore` → `seq_restore`, `1618-1636`) and
  `rejected.push((key, e))` reports the would-be key.
- The next same-source command re-stamps that seq → the same
  `CommandKey` appears once in `rejected` and once in `command_log`;
  two rejections produce identical `rejected` keys. Confirmed.

## Finding 9 — latched→bubble re-materialization never clears
## `v.maneuver`: **CONFIRMED** (latent)

- `unlatched_body` `SurfaceLatched` arm (`lib.rs:3094-3139`): sets
  `state`, clears `joined_colony`, scrubs every vessel-named event
  (including `ManeuverNodeStart`), restamps reliability — no `maneuver`
  write.
- Both other transitions do clear it: `apply_to_rails_at` (`~4759`)
  and `apply_to_bubble` (`~5004`) — and `apply_delete_maneuver`
  (`~5149-5156`) scrubs the event symmetrically.
- Reachability confirmed as v1-none: `apply_set_maneuver` requires
  `v.orbit()` — `OnRails` only (`5073-5075`) — and every path into
  `SurfaceLatched` passes through `InBubble` (where `maneuver` is
  already `None`) or clears it on entry. Latent zombie-node trap:
  confirmed.

## Spot-check of the "verified clean" section

- `ScheduledEvent::Ord` inverts `(tick, seq)` for the max-heap —
  correct (`events.rs:605-611`).
- `class_of` (`events.rs:444-497`): `SoiTransition` tracked-gated,
  `ScriptWake`/`PlayerAlarm`/`ManeuverNodeStart` `Clamping`,
  `SoiRescan`/`PartFailure` `Silent` — matches the claims.
- `TickShadow::restore` (`lib.rs:1618-1636`) restores log length,
  drained FIFOs in reverse order, `source_seqs` including first-touch
  removal (`Option` arm), heap and `next_event_seq` — as claimed.
- `SetVesselTracked`'s drain/re-heapify class rewrite (`4077-4104`) —
  as claimed.
- No counter-evidence found against any "verified clean" item.

## Notes for the parent

- One citation nit (does not affect any verdict): the report attributes
  the fragment-CoM convention to `structural.rs:345` — correct file and
  line (`fragment_velocity`), and additionally the sim-side jettison
  correction at `lib.rs:5568-5576` makes the same point more directly.
- Findings 1+2 are the same underlying convention confusion
  (origin-vs-CoM in `pos`/`vel` pairing) at two different seams;
  finding 1's fix suggestion is straightforward (`surface_pose_restore`
  could take `com_local`, or `unlatched_body` could add the
  `ω×(rot·com_local)` term it already has in scope).
