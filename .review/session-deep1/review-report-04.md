# Deep Review — Cluster C04: Command/Event Ordering Contract, Lease Gating, Bubble Residence

**Scope:** `crates/csp-sim/src/lib.rs` L1–1168 (plus the drain/apply/fire/lease machinery it references), `crates/csp-sim/src/events.rs` (full), `crates/csp-sim/src/bubble.rs` (full).
**Focus:** API contracts + determinism — lease-gating completeness, event-class assignment, hash/serialized discriminant stability, CommandKey ordering edges, co-rotation frame math.

All files/ranges were read in full. Determinism-critical machinery (BTreeMap drain order, `(tick, seq)` heap order, canonical heap enumeration in `state_hash`, per-command `TickShadow` rollback including `source_seq` restore, `#[repr(u16)]` + `StableDiscriminant` coverage, SUBSYSTEM_ORDER verbatim) was traced end-to-end and is clean unless noted below.

---

### [MEDIUM] Finding 1: `surface_pose_restore` assigns the body *origin's* co-rotation velocity to `body.vel` (which is CoM velocity)

- **File:** `crates/csp-sim/src/bubble.rs:245` (call site `crates/csp-sim/src/lib.rs:3113-3118`)

- **Problem:** `surface_pose_restore` computes `vel = env.body_spin.cross(pos - env.body_center)` — the surface co-rotation velocity of the **body origin** (`pos`) — and `unlatched_body` writes it into `body.vel`. But `WeldedBody.vel` is the **CoM** velocity: the backend integrates `x̄ += vel·dt` over `com_world()` (`crates/csp-phys/src/simple.rs:26-35`, `pos = x̄ − q·com_local`), the `SpawnPhysicalVessel` doc names `vel` "CoM velocity" (lib.rs:553), telemetry pairs `body.vel` with `com_world()` (lib.rs:7307-7309), and the breakup path computes fragment CoM velocities the same way (`structural.rs:345`).

- **Evidence:** For a rigid body co-rotating at ω, the material point at `p` moves at `ω × (p − c)`. The CoM's velocity is `ω × (com_world − c)`, not `ω × (pos − c)`. The stored quantity is off by `ω × (rot·com_local)` — exactly the origin-vs-CoM `ω×(q·com_local)` confusion `simple.rs:34-35` documents as a bug class it fixed.

- **Impact:** Any unlatched vessel with `com_local ≠ 0` re-enters physics with a systematic tangential velocity error ~`ω·|com_local|`, contradicting the doc's "the state a body-fixed object actually has … no impulse". Deterministic (no replay break), but the craft slides slowly relative to the surface until contact/settle absorbs it; worse for large/asymmetric craft and fast-spinning bodies.

- **Suggestion:** Compute the CoM co-rotation velocity: `vel = env.body_spin.cross(pos + rot.rotate(com_local) - env.body_center)`. `surface_pose_restore` has no `com_local` in scope — either take it as a parameter (the caller welds the body first, lib.rs:3111) or fix `body.vel` in `unlatched_body` after assignment.

---

### [MEDIUM] Finding 2: `apply_to_rails_at` computes osculating elements at `body.pos` (origin), not `com_world()`

- **File:** `crates/csp-sim/src/lib.rs:4740` (and the paired write at `lib.rs:4998`)

- **Problem:** The bubble→rails conversion builds the conic from `(body.pos - env.body_center, body.vel)` — origin position paired with CoM velocity. Every other consumer of the same state uses `com_world()`: the atmosphere gate *inside the same function* (lib.rs:4723), the aero/LS altitude checks (3003, 3345, 4617), and telemetry's osculating-elements call (7307-7309). `apply_to_bubble` round-trips the same convention (`body.pos = r + center`), so the legs are self-consistent — but the conic is anchored at the wrong point.

- **Evidence:** Gravity integrates the CoM (`x̄`), so the physically meaningful osculating state is `(com_world − c, vel)`. For `com_local ≠ 0`, `(pos − c, vel)` mixes two different points' states — it is not the osculating orbit of *any* material point, and for a rotating body the origin isn't even ballistic. `cartesian_to_elements` receives a state whose r and v do not belong together.

- **Impact:** Stamped elements differ from the true CoM conic by O(|com_local|) in position and inherit a mismatched velocity — the rails trajectory diverges from where physics would have taken the CoM, and the periapsis clearance check (`r_p > radius_m`, line 4746) is evaluated on the slightly-wrong conic. Deterministic, but systematically wrong for every non-centred craft.

- **Suggestion:** Use `(body.com_world() - env.body_center, body.vel)` at 4740, and on the return leg set `body.pos = r + center - com_local` (rot is weld-default identity) and `body.vel = vel` so the CoM lands on the conic point.

---

### [MEDIUM] Finding 3: A disabled script keeps its control lease — permanent lockout for all non-Ui sources

- **File:** `crates/csp-sim/src/lib.rs:6184-6196` (strike auto-disable), `lib.rs:4211-4223` (`SetScriptEnabled{enabled:false}`), vs the vacate at `lib.rs:6341-6353`

- **Problem:** `apply_unload_script` releases `control_lease` back to `Player` with the explicit rationale "a dead script cannot hold a lease — release the bound vessel back to `Player` so the craft is never stranded under a controller that no longer exists." Neither disable path does the same. A disabled script can never exercise the lease again — `control_tick` filters `s.enabled` (6102), `fire_script_wake` requires `s.enabled` (6375), `run_script_tick` early-returns on `!enabled` (6140) — yet the lease check still demands `src == (Script, id)` or `Ui` (4280).

- **Evidence:** Strike-disable is reachable through ordinary faults (`strikes >= STRIKE_DISABLE_LIMIT` at 6184). Self-disable via `SetScriptEnabled` is also reachable — the gate resolves the script's bound vessel and the holder passes it. Once disabled, only `TakeControl` from `Ui` can free the vessel: a non-holder `TakeControl` rejects (4311-4315), and `SetScriptEnabled{enabled:true}` itself is gated on the bound vessel — which rejects every non-holder, non-Ui source (4033-4040 + 4278-4281).

- **Impact:** In a session with a `Ui` producer this is an annoying manual step. In headless/External-only deployments (csp-server — the class the `Lease::Agent` variant exists for) it is a *permanent* control lockout on that vessel: no script or client can ever command it again. Contradicts the stated "dead script cannot hold a lease" invariant.

- **Suggestion:** On the disable paths (strike-disable in `run_script_tick`, and `SetScriptEnabled{enabled:false}`), release `control_lease` to `Player` when it equals `Lease::Script(id)` — the same vacate `apply_unload_script` performs.

---

### [MEDIUM] Finding 4: `Lease::Script` u64→u32 asymmetry + `TakeControl` never validates the target holder

- **File:** `crates/csp-sim/src/lib.rs:4280` (`id.0 as u32`), `lib.rs:4301-4315`, `lib.rs:456-463`; producer clamp at `crates/csp-script/src/lib.rs:1030`

- **Problem:** Three different `ScriptId → u32` mappings coexist:
  - producers stamp `source_id = script.id.0.min(u32::MAX) as u32` (clamp),
  - `check_control_lease` compares `src == (Script, id.0 as u32)` (wraparound truncate),
  - `apply_take_control`'s `me == current` compares full `u64` via `lease_identity` (exact).
  They agree only for `id ≤ u32::MAX`. Separately, `TakeControl` accepts any `Lease` value — `Ui` "may set any lease", and a holder "transfers at will" — with no check that `Script(id)`/`Agent(x)` names an existing entity.

- **Evidence:** `TakeControl{lease: Script(2³²+k)}` (from Ui or a holder) installs a lease whose gate check is `src == (Script, k)` — the nominal holder (which would stamp `u32::MAX`) can never pass, while an unrelated *script k* wrongly does. `TakeControl{lease: Script(9999)}` with no script 9999 loaded wedges the vessel against every non-Ui source exactly like Finding 3. `lease_identity` also manufactures `ScriptId(source_id)` from the stamp (459), so the stamp-side and check-side disagree on the identity of any `id ≥ 2³²`.

- **Impact:** Reachable with a single bad or malicious `TakeControl` — wrong-holder pass (script `id mod 2³²` gains authority) or phantom-holder wedge. The ≥2³²-script scenario itself is unreachable, but the *lease assignment* path requires no such count.

- **Suggestion:** Validate in `apply_take_control` that `Lease::Script(id)` names a loaded script (and that `id.0` fits `u32`, matching the producer clamp), or normalize `check_control_lease` to the same `min(u32::MAX)` rule producers use. At minimum, make the check and the stamp use one shared conversion so they cannot diverge.

---

### [MEDIUM] Finding 5: Lease-gating carve-out lets a non-holder *destroy or drain* the vessel the lease protects

- **File:** `crates/csp-sim/src/lib.rs:1144-1161` (`control_gated_vessel`), gate at `lib.rs:4033-4040`

- **Problem:** The gate covers the flight-control surface and control-program lifecycle (15 variants + `UnloadScript`/`SetScriptEnabled` via bound vessel — that part is correctly exhaustive for its stated list). But the "everything else is ungated: it does not steer the craft" rationale (1131-1134) doesn't cover destructive/extractive effects on the vessel itself.

- **Evidence:** Under a held lease (`Script`/`Agent`), a non-holder source can still issue, against the leased vessel: `RecoverVessel` (**deletes the vessel outright** — `site: None` is remote abandon salvage, 851-866), `TransferResource`/`TransferColonyResource` (drains its tanks), `TransmitData` (drains its `ElectricCharge`), `TransferCrew`/`BoardCrew`/`TransferColonyCrew` (moves crew), `SetVesselTracked` (flips its SOI events `Silent↔Clamping` — a warp-behaviour change), `SetLifeSupportEnabled` (toggles the crew-killing subsystem globally), `InstallAeroTables` (rewrites its aero cache). In an `External`-held lease, `SourceClass::Script` sources also do all of the above unchecked.

- **Impact:** If the lease is meant only as a flight-stick authority this is per-contract; but "the drain checks every control-affecting command" (355-359) reads broader than what is enforced, and `RecoverVessel` lets any non-leaseholder delete the very craft the lease exists to arbitrate. That reads as a contract gap, not a calibration choice.

- **Suggestion:** Either document the threat model explicitly (lease = steering authority only; destructive economy/science actions are world-level), or extend `control_gated_vessel` to cover at least `RecoverVessel` and the vessel-endpoint transfer/science commands. This may be a deliberate carve-out — flagging for contract confirmation rather than asserting a bug.

---

### [MEDIUM] Finding 6: `ScriptWake` double-ticks a bubble-resident script at the wake tick

- **File:** `crates/csp-sim/src/lib.rs:6365-6383` (`fire_script_wake` — no residence check), `lib.rs:6088-6116` (`control_tick` — ticks all enabled InBubble scripts), `lib.rs:7166-7219` (`resolve_wake` — `AtTick`/`InTicks` have no residence guard)

- **Problem:** An `InBubble` script can register `WakeCondition::AtTick`/`InTicks` (only the apsis/SOI/maneuver conditions require rails). The `ScriptWake` event fires at `w` and runs `on_tick(w)` via `fire_script_wake`. Then the next `step_bubble_range` covers `t ∈ w..t_next` and `control_tick(w)` runs `on_tick(w)` **again** — the same nominal tick processed twice.

- **Evidence:** `fire_script_wake` checks only `s.enabled && s.wake_tick == Some(tick)` (6372-6375); it does not check the bound vessel's residence and does not mark the tick consumed for the control pass. `control_tick`'s filter is `s.enabled && InBubble` (6097-6104) — nothing exempts a script whose wake already fired at this tick.

- **Impact:** A script relying on "each tick arrives once" (side-effectful wake logic, `persist` writes keyed by tick, op-budget assumptions) runs its body twice at the wake tick. Deterministic, but semantically a double application. Only hits `InBubble` scripts that register wakes — but nothing prevents or documents that combination.

- **Suggestion:** Either refuse `request_wake` while the bound vessel is `InBubble` (their tick already arrives every tick via the control pass), or have `fire_script_wake` skip bubble-resident scripts (stale wake → tolerated no-op, the `TransferComplete` precedent).

---

### [LOW] Finding 7: `TickIo::issue` lets the engine stamp any `SourceClass` — including `Ui`, which preempts every lease

- **File:** `crates/csp-sim/src/lib.rs:7367-7372` (io impl), `lib.rs:4272-4273` (Ui early-return)

- **Problem:** `WorldTickIo::issue(&mut self, class, source_id, command)` takes the stamp verbatim from the engine. `check_control_lease` returns `Ok` unconditionally for `SourceClass::Ui`. So an installed engine that stamps `Ui` bypasses every lease — the sim trusts the engine to stamp `Script`/`Autopilot` honestly. Real engines do (csp-script:1030, csp-ai:201), and engines are construction-config trusted code — but the io is the enforcement seam and it doesn't enforce.

- **Evidence:** `issue` pushes `(class, source_id)` straight into `pending` with no relation to `bound_script`/`bound_vessel`. Nothing else validates it downstream.

- **Impact:** None today (trusted engines behave); it widens the blast radius of a buggy engine or a future producer and quietly defeats M9c's authority model from inside the trust boundary.

- **Suggestion:** Constrain `issue` to the producer's identity — for a script io, force `SourceClass::Script` + `bound_script` id; for an autopilot io, `SourceClass::Autopilot` + bound vessel id — or at least reject `SourceClass::Ui`/`External` stamps from this path.

---

### [LOW] Finding 8: `AdvanceReport::rejected` keys alias live `command_log` keys

- **File:** `crates/csp-sim/src/lib.rs:5941-5961`, contract at `lib.rs:1189-1195`

- **Problem:** A rejected command rolls its `source_seq` consumption back and reports the "would-be key". The next same-source command reuses that seq — so a rejected `SetThrottle` reporting `(t, Script, 0, 5)` can be followed by an applied `(t, Script, 0, 5)` that *is* in the log. Two consecutive rejections from one source produce identical `rejected` keys as well.

- **Evidence:** `let seq = self.source_seqs.entry(src).or_insert(0)` + push `(key, e)` on reject, `cmd_shadow.restore` puts the counter back (5959-5961); the next staged command re-stamps the same value.

- **Impact:** Any consumer joining `rejected` and `command_log` on `CommandKey` equality gets a false collision — a key that is simultaneously "rejected" and "applied". Inherent to the "rejected spends nothing" design, but worth documenting as non-unique on the `rejected` field doc, or tagging entries with a flag/index.

- **Suggestion:** Document on `AdvanceReport::rejected` that keys are would-be keys and may alias log keys (and may repeat); alternatively include a per-drain ordinal in the rejected tuple.

---

### [LOW] Finding 9: `unlatched_body` scrubs vessel events but never clears `v.maneuver` — latent zombie-node asymmetry

- **File:** `crates/csp-sim/src/lib.rs:3094-3139` vs `lib.rs:4760` (`to_rails`) and `lib.rs:5004` (`to_bubble`)

- **Problem:** Both other residence transitions clear `vessel.maneuver` (the "manual transition discards the scheduled intent" rule). The `SurfaceLatched → InBubble` re-materialization scrubs every event naming the vessel (3132-3137) — including `ManeuverNodeStart` — but leaves `v.maneuver` untouched.

- **Evidence:** 3119-3138 sets `state`, clears `joined_colony`, filters the heap, restamps reliability — no `maneuver` write. If a `Scheduled` node ever reaches this arm, its pending start event is scrubbed while the node record survives — the exact "zombie node that never fires" `fire_due_events` is careful to avoid (6436-6445) and `apply_delete_maneuver` scrubs symmetrically (5149-5156).

- **Impact:** Unreachable in v1 — no path produces a latched vessel holding a node (SetManeuver requires `OnRails`; `ToBubble`/construction clear or never set it). It is a latent trap: the first feature that creates latched-with-node state inherits a silent zombie.

- **Suggestion:** Add `v.maneuver = None` in the latched arm for symmetry with the other transitions, or `debug_assert!(v.maneuver.is_none())` documenting the invariant.

---

## Verified clean (traced, no findings)

- **CommandKey total order:** `pending` is `BTreeMap<(SourceClass,u32), VecDeque>` — map order IS §2.2.1 drain order; phase-1 pops preserve per-source FIFO; `source_seq` stamped at dequeue; rejected commands roll the stamp back via `seq_restore` (including removing first-touch entries); `CommandKey`s are strictly increasing by construction (one drain per tick value, enforced by the `flushing` skip + the `t_next == now` empty-range guard + `schedule_event`'s strictly-ahead invariant).
- **In-range drains:** `step_bubble_range` drains `t` before `control_tick(t)` (one-tick latency), `self.clock` is set to `t` for stamp consistency and restored to `now` at 3692; `drain_at(now)` re-runs harmlessly on an empty queue.
- **Rollback correctness:** per-command `TickShadow` restores only that command's writes + seq; Fatal restores the whole iteration including `drained` commands pushed back in exact FIFO order.
- **Event ordering:** `ScheduledEvent::Ord` inverts `(tick, seq)` correctly for the max-heap; `event_seq` is serialized monotonic state; `fire_due_events` pops `tick <= t_next` (covers the deferred-tail state), bounded by `MAX_EVENTS_PER_BATCH`, per-event shadow drops only the failed event; degraded fallback republishes `SoiRescan` correctly.
- **`class_of`:** exhaustive over all 25 kinds; assignments match every variant doc (Silent: rescan/decay/periodics/colony boundaries/reliability; Clamping: player-facing transitions/completions; tracked-gated `SoiTransition` only). `SetVesselTracked` correctly rewrites pending `SoiTransition` classes through a drain/re-heapify (class is not part of `Ord`).
- **Discriminant stability:** `SourceClass`, `Lease`, `EventKind`, `EventClass`, `Subsystem`, `VesselState` all carry explicit sequential `#[repr(u16)]` discriminants matching serde variant indices; `hash_into` covers every payload field; `SimCommand`'s index-stability is documented append-only policy.
- **Co-rotation math (other than Finding 1):** `surface_pose_capture`/`restore` phase-quat convention is correct (`rel = R⁻¹(pos−c)`, `pos = c + R·p_fix`, `orientation = R⁻¹·rot`, `rot = R·stored`), pole/zero-spin degenerates handled, `DQuat::from_axis_angle` normalizes internally so `surface_upright`'s non-unit hinge axis is safe; the ENU convention was verified at `lat=lon=0` and both poles.
- **`TakeControl` rules:** `me == current` holder transfer, vacant self-acquire, `Player` preempt — all match the doc table; `Autopilot` correctly holds no identity.
