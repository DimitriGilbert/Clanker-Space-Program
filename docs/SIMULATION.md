# SIMULATION — the authoritative sim layer (csp-sim + csp-phys + csp-core)

The canonical reference for the deterministic simulation core. Audience: a competent
Rust dev with zero domain knowledge. Everything here was verified against the working
tree; where the code diverges from `PLAN.md` (the PRD), **both** readings are stated.
Cross-references: [ARCHITECTURE.md](ARCHITECTURE.md) (crate graph, frame loop),
[DATA-AND-SCRIPTING.md](DATA-AND-SCRIPTING.md) (RON schemas, Rhai host),
[BUILD-AND-DEV.md](BUILD-AND-DEV.md) (build, CI, goldens workflow),
[GAMEPLAY-GUIDE.md](GAMEPLAY-GUIDE.md) (player-facing flows).

The one-sentence model: **all state lives in plain Rust structs inside
`csp_sim::World`; the only way it changes is `World::step_frame(target_tick)`, driven
by stamped `SimCommand`s and a scheduled event heap; every subsystem that must survive
timewarp advances by closed form over an interval, never by per-tick integration**
(`crates/csp-sim/src/lib.rs` crate docs, `World::step_frame`).

Three crates own the layer:

| Crate | Role | Anchor |
|---|---|---|
| `csp-core` | `SimTime`/`TICK_HZ=64`, `DVec3`/`DQuat`, `StateHasher` (BLAKE3 canonical walk), ChaCha8 RNG KDF, `SimError` classes | `crates/csp-core/src/{time,vec3,quat,hash,rng,error}.rs` |
| `csp-phys` | weld, effective-part resolution, `SimpleBackend` integrator + contact, aero/thermal/structural, editor `CraftSession` | `crates/csp-phys/src/lib.rs` |
| `csp-sim` | the authoritative headless `World`: commands, tick machine, event heap, all progression subsystems, save codec | `crates/csp-sim/src/lib.rs` |

Hard rules baked into the crates (PLAN §2.1/§2.7, enforced by `clippy.toml`
disallowed-methods/types): `#![forbid(unsafe_code)]`; no `HashMap`/`HashSet`/`std::time`
(BTree\* only); no `rand` facade — draws only through `csp_core::rng`; `libm`
transcendentals only; iteration that affects state is ascending-ID.

---

## 1. How to read csp-sim

### 1.1 `src/lib.rs` — 24,461 lines, four internal sections

| Section | Lines (approx.) | Contents |
|---|---|---|
| **A. Command contract + World shell** | 1–3,985 | Crate docs (the §2.6.1/§2.6.2 design essays — read these first); module decls (L58–165); re-exports + the four load-bearing constants (below); `VesselId`/`Vessel` (L281/292 — a vessel is: craft weld inputs, residence `VesselState`, aero mask, thermal record, debris flag, maneuver slot, dock seams, tank contents, crew, science, colony join, reliability, control lease, parachute-deploy record); `SourceClass` (L421: Script=0 < Autopilot=1 < Ui=2 < External=3), `Lease` (L460), `CommandKey` (L505); **`enum SimCommand`** (L534–1216, 53 variants — the whole game-verb surface) + `SimCommand::control_gated_vessel` (L1243, which commands the control lease gates); `AdvanceReport` (L1317); `WorldSnapshot` (L1390); `WorldConfig` (L1560); `TickShadow` (L1703, one-tick rollback capture/restore: `capture` L1827, `merge_committed` L1882, `restore` L2015); `struct World` (L2183); `impl World` shell: constructors `new`/`with_system`/`with_config` (L2480–2495), config installers `set_*` (L2573–2832), **`World::restore`** (L2926), **`check_serializable`** (L3497), **`snapshot`** (L3556), `enqueue` (L3635), `telemetry` (L3670), `rails_warp_refusal` (L3707), shadow helpers (L3759–3831) |
| **B. The tick machine** | 3,986–8,753 | **`step_bubble_range`** (L3987, the per-integer-tick physics pass); `finish_burns` L4929; `remove_vessel` L4997; `schedule_debris_decay` L5064; `schedule_event` L5119; `schedule_next_soi` L5171; **`apply_command` dispatch** (L5223) + all `apply_*` handlers (L5527–7547: `check_control_lease`, `apply_take_control`, `apply_terminate_vessel`, `apply_transfer_command`, `apply_set_alarm`, `apply_watch_window`, `apply_unlatch`, `apply_spawn_vessel`, `apply_spawn_physical`, `apply_to_rails`/`apply_to_rails_at`, `resolve_soi_residence`, `apply_to_bubble`, `apply_set_maneuver`, `apply_delete_maneuver`, `fire_maneuver_start`, `ensure_aero`, `apply_install_aero`, `apply_set_aero_config`, `apply_jettison_part`, `apply_deploy_part`, `apply_undock`); **`drain_boundary`/`drain_at`** (L7548/7570 — CommandKey stamping); **`advance_interval_subsystems`** (L7668) + `advance_orbits` (L7740); `control_tick` (L7781) → `run_script_tick`/`run_autopilot_tick` (L7819/7937); script applies + `fire_script_wake` (L7985–8188); **`fire_due_events`** (L8189) / `fire_event` (L8354) / `apply_soi_transition` (L8504); **`World::step_frame`** (L8620); **`state_hash`/`state_hash_masked`** (L8754/8767) |
| **C. Fuel, tick-IO, restore/breakup/hash walk** | 8,754–11,880 | `Default for World` L8896; `refresh_decel_sites` L8911; the `FuelPlan` propellant machinery (L8950–9268: `propellant_mpu`, `propellant_plans`, `apply_fuel_scales`, `engine_can_draw`, `planned_burn_throttle`, `drive_node_burn` — the maneuver executor); `WakeAction` L9269; **`WorldTickIo`** (L9287, the `TickIo` impl scripts/autopilots see: telemetry, `issue`, `request_wake`, `log_line`); shadow split helpers L9777–9834; the `restore_*` validation family (L9900–10914: transfer/build/science/economy/contract/vessel-contents/colony/reliability/vessel state — `World::restore`'s coherence gates); `hash_craft` L10915; **`transition_latched`** (L11154, bubble→surface-latch); **`apply_breakup`** (L11252, structural/thermal split with momentum conservation); `hash_vessel` L11731 |
| **D. Inline tests** | 11,881–24,461 | `mod tests` — ~12,600 lines, **199 `#[test]`s**, organized by milestone banners: M0 command-log set (L12027), M1-B event queue + ×1-vs-×10⁵ equivalence (L12445), M2-C physics bubble (L14309), M4b thermal (L16041), M5 maneuver + Duva end-to-end (L17071/17771), M6-B/C/D docking + transfers (L18375/18815/19557), M7-A science (L21246), M9c control-lease `mod m9c` (L23163), `mod fatal_rollback` TickShadow coverage (L23894) |

Constants at the top of section A (each mirrors a `data/balance.ron` key for
data-less worlds; a `csp-bench` test pins the mirror):

- `MAX_TICKS_PER_FRAME = 69_120` (L214) — 18 sim-minutes at 64 Hz; one `step_frame`
  call's tick budget → `AdvanceReport::partial`.
- `DEFAULT_MAX_EVENTS_PER_FRAME = 2_000` (L224) — §2.6.5's event budget.
- `DEFAULT_BUBBLE_RADIUS_M = 20_000.0` (L233).
- `MAX_EVENTS_PER_BATCH = 1_024` (L252) — one tick's due batch fires atomically in
  ≤1024-event chunks; the tail defers to the next loop iteration (zero-advance flush).
- `DEGRADED_RESCAN_TICKS = 69_120` (L273) — a `Recoverable` event failure re-publishes
  `SoiRescan` this far out.

### 1.2 The 17 feature modules

| Module | Owns | Anchor |
|---|---|---|
| `orbit` | `PatchedConics` `OrbitModel`: universal-variable Kepler solver (Laguerre–Conway n=5, Stumpff series/closed form, elliptic Δt mod period), elements↔cartesian, §2.6.6 SOI scan (stride `h = 0.1·r_soi/v_rel_max`, sign-change bracketing, Brent to ≤1 tick) | `crates/csp-sim/src/orbit.rs` |
| `events` | `EventKind` (25 kinds), `EventClass` (Clamping/Silent), `class_of` (the tracked-vessel opt-in), `ScheduledEvent` heap entry (inverted `Ord` on `(tick, seq)`), `SUBSYSTEM_ORDER` (the 12-slot constant array) | `crates/csp-sim/src/events.rs` |
| `maneuver` | `ManeuverNode` sim state, `Scheduled → Burning` lifecycle, post-burn conic prediction, executor attitude/thrust helpers | `crates/csp-sim/src/maneuver.rs` |
| `transfer` | Transfer planner: `lambert`/`lambert_multi`, `porkchop_scan`, `plan_transfer`, `predict_encounter`, `departure_node` (plan → `SetManeuver` bridge). Pure queries over `PatchedConics` — no state, no clock | `crates/csp-sim/src/transfer.rs` |
| `bubble` | Residence types: `VesselState` (OnRails/InBubble/SurfaceLatched), `CraftDef` (weld inputs), `SurfacePose` (body-fixed latch record), `VesselThermal`, `PhysicsConfig` | `crates/csp-sim/src/bubble.rs` |
| `joints` | `LiveJoint` soft-capture transients, `DockingConfig` tunables, per-tick capture detection + lifecycle, `DockSeam` records, `joint_refused` set | `crates/csp-sim/src/joints.rs` |
| `transfers` | `ResourceTransfer`/crew-transfer processes (`ActiveTransfer`, debit-at-apply), the docked-or-proximity link precondition, per-vessel tank/crew bookkeeping + §2.4 quantized-mass retally | `crates/csp-sim/src/transfers.rs` |
| `research` | §3.2 science: keyed `VesselScience` data inventory, diminishing-repeat soft cap, transmit/recover, lab two-regime closed form with `data_epsilon` exhaustion | `crates/csp-sim/src/research.rs` |
| `tech` | `TechState` (researched nodes, purchased `UpgradeTrack` levels, family qualifications), `frozen_upgrades`/`build_part` (the manufacture-time `BuiltPart` freeze) | `crates/csp-sim/src/tech.rs` |
| `economy` | Integer-micro funds + bounded ledger, crew roster + `SalaryDue`, recovery-site inventories (`SiteState`), `Insolvent` path | `crates/csp-sim/src/economy.rs` |
| `contracts` | Deterministic offer generator (`{CONTRACTS, 0, offer_index}` KDF), fact-based prerequisites, event-driven completion predicates, `ContractDeadline` | `crates/csp-sim/src/contracts.rs` |
| `colonies` | §3.5.1 flow network: Day/Night insolation step + closed-form `TerminatorCrossing`s, algebraic `T_eq`, deterministic allocation pass (least/greatest fixpoint), epoch-anchored linear stores, `ColonyState` machine | `crates/csp-sim/src/colonies.rs` |
| `manufacture` | `ManufactureCraft` build orders (BOM + funds spent at apply), `build_ticks = ceil(mass/rate·TICK_HZ)`, `ConstructionComplete` delivery as surface-latched vessel | `crates/csp-sim/src/manufacture.rs` |
| `life_support` | Per-save toggle, periodic `LifeSupportCheck` debit on the absolute grid, `VesselLifeSupportDepleted` crew-loss transition | `crates/csp-sim/src/life_support.rs` |
| `reliability` | §3.4: `PartReliability` records (pre-drawn θ, `PartUsage`/Λ accrual), dormant hazard, `PartFailure` closed-form publishing, ignition Bernoulli draws, notification log, qualification/bench writers | `crates/csp-sim/src/reliability.rs` |
| `scripts` | Script/autopilot sim-side surface: `ScriptState`/`persist` map, `TickIo` trait, `VesselTelemetry`, `ScriptEngine`/`AutopilotDriver` traits, closed-form wake resolution, poll-detection constants | `crates/csp-sim/src/scripts.rs` |
| `save` | `CSPSAVE\0` container codec (JSON header + DEFLATE-over-CBOR), frozen save schemas + `migrate_vN_to_vN+1` machinery, bug-report command-log codec | `crates/csp-sim/src/save.rs` |

The Rhai guest host is **not** here (it is `csp-script`, installed as construction
config via `World::set_script_engine`); neither is the ascent autopilot (`csp-ai`,
installed via `set_autopilot_driver`). Keeping engines out of `csp-sim` keeps `rhai`
out of the determinism crate's tree (`crates/csp-sim/src/scripts.rs` crate-split note).

---

## 2. The tick lifecycle

One frame of the app calls `World::step_frame(target_tick)` exactly once
(PLAN §2.5 `SimStep` schedule; `crates/csp-app/src/lib.rs` frame loop). What happens
inside (`crates/csp-sim/src/lib.rs`, `World::step_frame` L8620):

```
step_frame(target):
  loop {
    1. break unless clock < effective_target && tick_budget > 0
    2. shadow = TickShadow::capture(self)                 // §2.13 rollback anchor
    3. drain_boundary(...)          (L7548 → drain_at L7570)
         - pops staged commands per (source_class, source_id) FIFO in BTreeMap order
           (that iteration IS the §2.2.1 CommandKey order)
         - stamps CommandKey { tick, source_class, source_id, source_seq }
         - apply_command(src, cmd)  (L5223)
              * control-lease gate first (check_control_lease L5527)
              * per-command shadow: a non-Fatal reject rolls back ALL of its effects
                (partial writes, seq consumption, log entry) → AdvanceReport::rejected
              * Ok ⇒ push (key, cmd) to command_log (the §7.6 replay artifact)
         - skipped entirely while a deferred over-ceiling event batch is flushing
           (heap head sits at the clock — one drain per tick value, ever)
    4. t_next = min(effective_target, event_heap.peek_tick(), now + tick_budget)
              .max(now)
    5. range_end = step_bubble_range(now, t_next, ...)    (L3987)
         per integer tick t in now..t_next, for every InBubble vessel:
           clock = t; backend.set_sim_time(t)
           drain_at(t)                  // a bubble tick IS a tick boundary
           control_tick(t)              (L7781) // scripts (asc ScriptId) then
                                                // autopilots (asc VesselId), BEFORE
                                                // force integration; their commands
                                                // drain next tick (one-tick latency)
           ensure_aero                  (L6846) // per-tick table attach (see §5)
           physics backend step         // forces → semi-implicit Euler → PGS contact
           fuel plan (FuelPlan, L8950+) // propellant debit, §2.4 1/1024-quantum retally
           thermal step, structural check → apply_breakup (L11252) on overload
           joints lifecycle (joints::lifecycle — dissolve expired, scan captures)
           settle latch → transition_latched (L11154)   // §2.6.4
           maneuver executor (drive_node_burn L9230)    // §6
           debris impact bookkeeping
           auto bubble→rails: the FIRST tick a coasting vessel's conic clears
             surface + atmosphere top forces range_end to that tick (the
             "rail_break" edge, `step_bubble_range` doc at L4035) → apply_to_rails_at
    6. advance_interval_subsystems(now, range_end)       (L7668)
         iterates SUBSYSTEM_ORDER verbatim; real arms today:
           Orbits      → advance_orbits (L7740): re-solve each rails conic at t_next
                         (solvability proof only — rails elements are epoch-anchored
                         and mutate at events/commands, never per interval)
           Colonies    → publish_colonies (repair missing network deadline)
           LifeSupport → publish_life_support (keep one check on the absolute grid)
           Reliability → publish_reliability (lazily publish closed-form dormant
                         PartFailure crossings)
           Contracts   → publish_economy (SalaryDue cadence) + publish_contracts
                         (lazy offer-board refill)
    7. clock.try_advance(range_end - now); tick_budget -= advance
    8. fire_due_events(range_end, ...)                    (L8189)
         - pops while heap.tick <= t_next && batch < 1024
         - per-event shadow; clock is swapped to event.tick for the firing
           (partition-independence: firing runs at the event's own tick)
         - fire_event (L8354) applies the effect; may schedule new events
         - Recoverable failure ⇒ rollback + drop + zombie-drop of paired records
           (node/transfer/project/contract) + SoiRescan republish
         - Fatal ⇒ whole-iteration shadow.restore(self), step_frame returns Err
    9. break on reached_target || tick_budget==0 || events_processed >= event_budget
  }
  return AdvanceReport { reached_tick, commands_applied, events_processed,
                         fired_events, rejected, degraded, partial, ... }
```

Key properties, each stated in code:

- **A zero-advance call is not a tick boundary** — it drains nothing
  (`step_frame_at_current_tick_drains_nothing`, lib.rs tests L12092).
- **Commands are stamped at dequeue**, never at production (`CommandKey` doc L494).
- **Rejected commands apply nothing** — the reject is total rollback
  (`AdvanceReport::rejected` doc L1334).
- **`Clamping` events in `fired_events` are what `csp-app`'s `WarpController`
  demotes on** (§4 below).
- The **event budget is checked between iterations, never mid-batch**; a batch in
  (1024, 2000] defers its tail without tripping `partial` (`MAX_EVENTS_PER_BATCH`
  doc L235).

The control lease (§2.2.1): every command in `SimCommand::control_gated_vessel`
(L1243) is checked against the vessel's `Lease`; non-leaseholders get a
`Recoverable` reject with zero writes. `Player` preempts unconditionally;
`External` clients must `TakeControl` first (`Lease` doc L438).

---

## 3. The SimCommand surface and who can issue it

53 variants (`enum SimCommand`, lib.rs L534–1216). Variant order is append-only and
stable (the command log's serde stream encodes the index). Grouped by gameplay verb,
with the **reachability split** — verified by grepping live (non-test) issuance code
in `csp-view`/`csp-app` (player path) vs `csp-server`'s generic `sim.command` MCP tool
(`crates/csp-server/src/mcp.rs:532`, which deserializes *any* variant) and
`csp-bench`. Player-path anchor is the translation table
`program_command` (`crates/csp-view/src/lib.rs:1001`) plus the map planning panel
(`crates/csp-view/src/map.rs`) and app input (`crates/csp-app/src/lib.rs`).

**Player-reachable: 44 of 53. MCP/bench-only: 9.**

### Movement / attitude (player: keys + HUD)
| Command | Player path | Notes |
|---|---|---|
| `SetThrottle`, `SetAttitude` | `csp-app/src/lib.rs:2190/2123` (held-axis integration happens app-side; commands are absolute) | Lease-gated; a positive throttle on propellant-starved engines clamps to 0 in the apply (L5262) |
| `JettisonPart` | staging (`csp-view/src/lib.rs:3276`) | Splits the graph; partitions thermal/tanks/science/reliability with it |
| `DeployPart` | Program window (`csp-view/src/lib.rs:1190`) | Parachute deploy; `data/parts/csp.chute.s125.ron` exists |

### Rails / bubble / tracking
| Command | Player path | Notes |
|---|---|---|
| `ToBubble` | "recover control" pin (`csp-view/src/lib.rs:2664`) | Materializes an on-rails vessel in the bubble |
| `SpawnPhysicalVessel` | sandbox fallback only (`csp-view/src/lib.rs:3037`) — worlds with no authored launch site; funds still debit | The normal launch path is `ManufactureCraft` |
| `SpawnVessel`, `ToRails`, `Unlatch`, `SetVesselTracked` | **MCP/bench only** | `ToRails` is also what the *auto* bubble→rails edge executes internally (`apply_to_rails_at`); `Unlatch` behavior also fires implicitly on throttle (`unlatched_body`); `SetVesselTracked` promotes that vessel's `SoiTransition` Silent→Clamping |

### Warp / clock / session
`SetWarp`, `AdvanceTo` — emitted only by `csp-app`'s `WarpController`
(`crates/csp-app/src/warp.rs`); warp is a logged command, never a measurement.
`SetAlarm` — map planning panel (`map.rs:1133–1220`) + Program window.
`SetLifeSupportEnabled` — issued **once at app startup** with `enabled: true`
(`csp-app/src/lib.rs:608`); there is no player toggle (the module default is OFF,
`life_support.rs` module doc).

### Science / research
`RunExperiment`, `TransmitData`, `RecoverData`, `StartResearch`, `StopResearch` —
all Program-window buttons (`csp-view/src/lib.rs:1015–1231`). `RecoverData` requires
`SurfaceLatched` (the parachute part makes that reachable on Terrin).

### Tech
`ResearchTech`, `ResearchUpgradeLevel` — Program window (`csp-view/src/lib.rs:1008`).

### Economy / crew / recovery
`RecoverVessel` — Program window + auto-recovery of a pad-occupying parked build at
launch (`csp-view/src/lib.rs:2976`); credits resource contents at
`recovery_efficiency` (0.9; 1.0 at launch site per `data/balance.ron`).
`HireCrew`, `BoardCrew`, `TerminateVessel` — Program window / app key.
`ContractReward` — **MCP-only** (a raw funds credit; the sim's own contract
completion pays through the event path, not this command).

### Contracts
`AcceptContract` — Program window (`csp-view/src/lib.rs:1014`).

### Colonies / manufacture / logistics
`FoundColony`, `AbandonColony`, `AssignCrewToColony`, `EvacuateColony`,
`ScheduleColonyInspection`, `JoinColonyInventory`, `LeaveColonyInventory`,
`TransferColonyResource`, `TransferColonyCrew` — all Program window
(`csp-view/src/lib.rs:1121–1188`). `ManufactureCraft` — the **Launch button**
(`csp-view/src/lib.rs:3020`): pays BOM + funds at apply, delivers a latched craft at
`ConstructionComplete`. `DamageColonyModule` — **MCP-only** (the sim's own
vessel-impact detection applies damage internally, `colonies.rs:3924`).

### Maneuver / transfer planning
`SetManeuver`, `DeleteManeuver` — map node editor (`map.rs:1244`,
`csp-view/src/lib.rs:905/912`). `WatchTransferWindow` — map + Program window
(`map.rs:1202`).

### Docking
`Undock` — **MCP-only** today (splits the graph at a `DockSeam`); `data/parts/
csp.dockport.s125.ron` exists, and the Program window has no undock button (verified:
no `SimCommand::Undock` in live view/app code).

### Resource / crew transfers between vessels
`TransferResource`, `TransferCrew` — Program window (`csp-view/src/lib.rs:1095–1119`).

### Scripting / autopilot
`LoadScript`, `UnloadScript`, `SetScriptEnabled`, `EngageAutopilot`,
`DisengageAutopilot` — Program window (`csp-view/src/lib.rs:1236–1268`); the Rhai
host and ascent autopilot are installed at startup (`csp-view/src/lib.rs` RhaiHost
install). `RunBenchTest` — Program window (`csp-view/src/lib.rs:1274`).

### Control / aero install
`TakeControl` — issued by the view when a recover-control pin converts
(`csp-view/src/lib.rs:2275`), and by MCP `vessel.take_control`. `InstallAeroTables`,
`SetAeroConfig` — **MCP-only** (see §5: the sim self-computes tables, so the
install-by-command worker path is never exercised by the game).

Also player-reachable but not SimCommands: save/load/revert ride the **Game menu**
(session-side, `crates/csp-ui/src/game.rs` → `View::save_game`/`load_game`,
`csp-view/src/lib.rs:3064/3079`, revert restores the `pre_launch` snapshot taken at
`csp-view/src/lib.rs:2997` with the §3.4 "revert is not a reroll" disclosure).

What scripts may issue (`ControlApi`, `crates/csp-script/src/lib.rs:169–299`):
`SetThrottle`, `SetAttitude`, `JettisonPart`, `SetManeuver`, `DeleteManeuver`,
`TransferResource`, `SetAlarm`, `DeployPart`, `EngageAutopilot`, `DisengageAutopilot`
— everything goes through the same queue, stamped `SourceClass::Script`.

---

## 4. The event system

One `BinaryHeap<ScheduledEvent>` on `World`, keyed by `(tick, seq)`; `seq` is the
serialized monotonic `event_seq` assigned at insertion (insertion order follows
`SUBSYSTEM_ORDER`, so the counter is hash-relevant state). `Ord` is inverted so the
max-heap pops the earliest (`ScheduledEvent`, events.rs L610).

`EventClass` is assigned **at schedule time** by `class_of` (events.rs L449) and
carried on the entry — the policy lives in one place, not per-subsystem:

- **Clamping** (auto-demotes warp via `AdvanceReport::fired_events` →
  `WarpController`): `PlayerAlarm`, `ManeuverNodeStart`, `TransferWindow`,
  `TransferComplete`, `ResearchDataExhausted`, `ResearchProjectComplete`, `Insolvent`,
  `ContractDeadline`, `PowerLost`, `PowerRestored`, `LifeSupportDepleted`,
  `VesselLifeSupportDepleted`, `ScriptWake`, tracked-vessel `SoiTransition`.
- **Silent** (never touches warp): `SoiRescan`, `DebrisDecay`, `SalaryDue`,
  `TerminatorCrossing`, `ColonyStoreFull`, `ColonyStoreEmpty`, `ColonyInspection`,
  `LifeSupportCheck`, `PartFailure`, `BenchTestComplete`, untracked `SoiTransition`.

PLAN §2.6.5's table names `ColonyStoreFull`/`Empty` Silent and a `Clamping`
`ConstructionComplete`; the code keeps `ConstructionComplete` Clamping (the member
the plan's table actually lists) and adds kinds the plan didn't name: `SoiRescan`
(the Orbits publish deadline — always Silent), `TransferWindow`,
`PowerLost`/`PowerRestored`, `LifeSupportCheck`, `VesselLifeSupportDepleted`,
`BenchTestComplete`. All are additive.

Who publishes what (producer → kind):

| Producer | Kind(s) |
|---|---|
| `schedule_next_soi` (lib.rs L5171) | `SoiTransition`, `SoiRescan` |
| `apply_set_alarm` / `apply_watch_window` | `PlayerAlarm` / `TransferWindow` |
| `apply_set_maneuver` / `apply_delete_maneuver` | `ManeuverNodeStart` (paired with the node slot) |
| `apply_transfer_command` (transfers.rs) | `TransferComplete` (paired with `ActiveTransfer`) |
| `StartResearch` apply (research.rs) | `ResearchDataExhausted` + same-tick `ResearchProjectComplete` |
| `publish_economy` (Contracts slot) | `SalaryDue`; the funds charge path schedules `Insolvent` |
| `AcceptContract` apply | `ContractDeadline` (dropped on completion — strict pairing) |
| colony recompute (`solve_network`) | `TerminatorCrossing`, `ColonyStoreFull/Empty`, `PowerLost/Restored`, `LifeSupportDepleted` |
| `ManufactureCraft` apply (manufacture.rs) | `ConstructionComplete` |
| `publish_life_support` / a check firing | `LifeSupportCheck` / `VesselLifeSupportDepleted` |
| `request_wake` (WorldTickIo) | `ScriptWake` (paired with `ScriptState::wake_tick`) |
| `publish_reliability` / `RunBenchTest` | `PartFailure` / `BenchTestComplete` |
| `schedule_debris_decay` / `ToRails` | `DebrisDecay` |

Stale firings are tolerated no-ops by design (`TransferComplete` precedent): events
name ids that removal paths never scrub. Failed (Recoverable) firings roll back,
drop, and re-publish `SoiRescan` at `event.tick + DEGRADED_RESCAN_TICKS`
(`fire_due_events` L8337).

---

## 5. Physics — csp-phys

**Weld, two stages** (`crates/csp-phys/src/weld.rs`):

- **Stage 1** — `weld(part_graph, &[WeldPart]) -> WeldedBody`: pure, synchronous,
  bit-deterministic (fixed ascending-`PartNodeId` summation). Produces total mass,
  exact CoM, full inertia tensor (`R·I·Rᵀ` + parallel-axis per part), per-part
  `ForceSite`s, and the merged convex-hull compound collider. Per PLAN §2.4 there is
  **no time trigger** — callers re-weld only when the 1/1024-quantized resource mass
  changes.
- **Stage 2** — aero tables (`crates/csp-phys/src/aero.rs`): isotropic 0.15 m
  voxels (budget-capped, edge doubles on overflow), voxelized **once in body frame**,
  swept over a 66-direction geodesic; barycentric interpolation; one table per
  `ConfigMask` (max 8, nearest-by-Hamming fallback — `MAX_CONFIG_MASKS`, aero.rs:121).
  - **PLAN-vs-code divergence #1:** PLAN §2.4.1 says the 66 directions are an
    "(icosahedron, 1 subdivision)" — arithmetically wrong (that yields 42). The code
    keeps the pinned count 66 via a **frequency-4 octahedron geodesic**
    (aero.rs module doc, first bullet).
  - **PLAN-vs-code divergence #2:** PLAN §2.4 makes Stage 2 a worker-computed
    `InstallAeroTables` command and says the sim "refuses to launch" without
    installed tables. In the code the sim **self-computes tables lazily inside the
    bubble tick** (`World::ensure_aero`, lib.rs L6846: builds/caches per
    `(vessel, graph_hash, mask)` and attaches `body.aero`), nothing refuses a launch,
    and `InstallAeroTables`/`SetAeroConfig` are never issued by the game (MCP-only).
    The install path still exists and debug byte-compares against a sim recompute.

**EffectivePart** (`crates/csp-phys/src/resolve.rs`): PLAN §3.1 defines
`EffectivePart = resolve(BuiltPart) ∘ wear(PartUsage)` — "two functions". The code
keeps that shape with a third **live** input: family qualification
(`resolve_built_part` frozen half, `apply_wear` live half, `resolve_part` the
composition). Frozen-at-manufacture fields (`part_id`, `scale`, `variant`,
`upgrade_levels_at_build`, `data_set_hash`) are serialized per part instance; a
`BuiltPart` never reads live tech state on re-derivation (an in-flight craft cannot
gain upgrades across a reload). The weld consumes the embedded
`EffectivePart::weld_input` (`WeldPart`) — one resolver, one place
(resolve.rs naming-reconciliation note).

**SimpleBackend** (`crates/csp-phys/src/simple.rs`) — the only backend:
semi-implicit (symplectic) Euler, dt pinned at `1/64` (RK4 explicitly rejected);
phase order force → velocity → position → contact, positions advancing the **CoM**
(`pos = x̄ − q·com_local`); gyroscopic term included (torque-free precession is an
oracle case). Contact is a projected Gauss–Seidel impulse solve: 8 iterations in
fixed ascending `(body_id, contact_index)` order, Baumgarte β = 0.2, restitution 0,
two-direction Coulomb friction, against facet-plane hulls derived in-crate
(`hull.rs`) — **no parry/rapier materialization anywhere in the solve**. Body-vs-body
contact (M6) runs the same PGS machinery over pair manifolds; latched bodies act as
static colliders. Terminal state is the §2.6.4 settle latch (`settle_v = 0.05` m/s,
plus per-tick pose-delta limits).

- **PLAN-vs-code divergence #3 (documented in code):** PLAN §2.9 specifies a
  `RapierBackend` (`rapier3d-f64`) for jointed islands. **No RapierBackend exists**;
  `rapier3d-f64`/`parry3d-f64` are workspace-declared but unused by any crate
  (`crates/csp-phys/src/backend.rs:29-31`). Soft-capture joints are solved inside
  `SimpleBackend`'s PGS velocity pass — the deviation is documented at
  `crates/csp-sim/src/joints.rs:14-18`.

**Thermal** (`thermal.rs`): one lumped node per part; conduction across attach edges,
convective reentry heating `½ρ|v|³·A_eff` off the aero tables (shielded parts take no
convective input — a fairing is a heatshield), Stefan–Boltzmann ambient exchange.
The per-part record (`VesselThermal`, `bubble.rs`) is authoritative state on the
vessel; held fixed on rails/latched (documented v1 approximation).

**Structural breakup** (`structural.rs` + `World::apply_breakup` lib.rs L11252):
joint reaction `F_J = M_S·a(c_S) − ΣF_ext` per non-root node decomposed against the
attach axis; overload splits the graph, children inherit momentum (conservation
test with the plan's defined denominators) and the parent's aero tables.

**Docking** (`merge.rs` + `joints.rs`): approach = live `LiveJoint` transient
(cap 16; over-cap degrades to clean separation); latch = pure graph merge
(secondary ids remapped `base = max(primary)+1`) + one Stage-1 weld + recorded
`DockSeam`; undock = split at the seam. Live joints are **hashed but never
serialized** (§7.4 refuses saves while any exist); `joint_refused` is hashed and
serialized.

**CraftSession** (`session.rs`) — the editor/VAB working-craft model: a rooted
`SessionNode` tree keyed by `PartNodeId`, attach edges, staging, scale/variants,
validation, `to_part_graph`. It is §2.2-category-C **session state** (never hashed,
never in the command log); the type lives in `csp-phys` so headless consumers
(bench, MCP `editor.*`, design AI) don't depend on a UI crate. `readout.rs` is the
per-stage Δv/TWR/burn-time analysis; `profile.rs` is the §3.4 `MissionProfile`
(stub; confidence is a plain comparison at this tier).

---

## 6. Orbits, time, and warp

**Clock**: `SimTime(u64 ticks)`, `TICK_HZ = 64`, epoch 0 = game start, UT seconds =
ticks/64 (`crates/csp-core/src/time.rs`). The only mutator is the fallible
`try_advance`. The physics dt is `1/64` s forever — warp never changes it (the
single most important rule in PLAN §2.6).

**Patched conics** (`orbit.rs`): `OrbitState { parent, elements, epoch }` is
**epoch-anchored** — the stored elements never change as the clock advances;
`state_at` evaluates the closed form at any tick. This is what makes the ×1-vs-×10⁵
equivalence test pass: re-stamping `M += n·Δt` per interval would make hashed state
partition-dependent (lib.rs module doc §"Partition independence"). Element
conventions (including the parabolic `a := −p` encoding and degenerate-angle rules)
are documented in the orbit.rs module doc — they ARE the contract.

**Universal solver**: elliptic Δt reduced mod period; Stumpff C/S closed-form vs
series at `|z| < ε`; Laguerre–Conway n=5 with a maintained sign bracket; cap 64
iterations; non-convergence is `Recoverable` (keep last valid state), never garbage.

**SOI transitions** (§2.6.6): coarse scan at stride `h = 0.1·r_soi/v_rel_max` (ceil
to ticks, min 1), sign-change bracketing, Brent to ≤1 tick. Honest guarantee:
residences shorter than `h` may be missed; `h` is reportable (`stride_ticks`).
Transitions re-base elements through `cartesian_to_elements` at the crossing tick
(`apply_soi_transition`, lib.rs L8504).

**Maneuver nodes** (`maneuver.rs`): one node per vessel (v1); `SetManeuver` requires
the vessel on rails around the dominant body with a positive-rated thrust site. The
burn is **thrust-integrated, never impulsive**: `ManeuverNodeStart` materializes the
body at the node point, and the executor (`drive_node_burn`, lib.rs L9230) owns
throttle/attitude per tick until delivered Δv hits zero, then returns the vessel to
rails with epoch-stamped elements.

**Transfer planner** (`transfer.rs`): Lambert (universal-variable form), porkchop
grid + strict local minima, `plan_transfer`, `predict_encounter` (SOI scan over a
hypothetical conic). Honest limitation, stated in the module: `departure_node`'s
single-impulse ejection does **not** compose to arrival on its own — the
predict-then-re-plan correction loop is required.

**Bubble ↔ rails rules**:
- A vessel is `InBubble` iff physics-resident; `OnRails` conic otherwise;
  `SurfaceLatched` = body-fixed pose record (lat/lon/alt/orientation), no contacts,
  no drift, doesn't block warp (`bubble.rs`).
- Settle latch: contact + `‖v‖ < 0.05` m/s + pose deltas under the same limits for
  the settle window → latch (`transition_latched`).
- **Auto bubble→rails** (the edge PLAN §2.6.2 left implicit): the first tick a
  coasting bubble vessel's conic clears surface + atmosphere top, the range ends
  there and the vessel converts — a pure function of the trajectory, so every
  partition converts at the same tick (`step_bubble_range`, lib.rs L4035 doc).
- Manual: `ToRails` (MCP), `ToBubble` (recover-control pin), `Unlatch` (MCP; also
  implicit on throttle).

**The warp ladder** (`crates/csp-app/src/warp.rs:31`):
`WARP_LADDER = [1, 2, 10, 100, 1_000, 10_000, 100_000]` — ×1 real time, ×2 physics
warp (128 bubble ticks/s, the v1 cap), rails ×10…×10⁵. What advances at which warp:
**at every rung the same `step_frame` code path runs** — at ×1/×2 the bubble ticks
execute; under rails the bubble is empty of thrust/contact/atmosphere vessels
(`World::rails_warp_refusal`, lib.rs L3707 — checked every frame by the app, demotes
to rung 0 while it holds) and the loop is an analytic jump. The controller emits
`SetWarp`/`AdvanceTo` as logged commands; promotion needs the §2.6.3 inequality with
1.5× margin for 30 consecutive frames. `MAX_TICKS_PER_FRAME = 69_120` bounds one
call (= 18 sim-min, the ×10⁵ frame coverage); `partial` reports tell the caller to
re-issue.

---

## 7. Progression subsystems

All are sim-side arenas on `World`, hashed + serialized unless noted. Reachability is
stated honestly: **sim-complete-but-no-UI counts as unreachable** — as of this
writing the Program window exposes nearly everything; the residual MCP-only list is
§3's nine.

**Science** (`research.rs`): experiments yield typed `DataRecord`s keyed
`(experiment, body, biome, situation)`, pooled per producing node (partitions with
the graph). Diminishing repeats are the plan's rule applied as its exact integral —
`C' = C + (cap − C)(1 − exp(−k·u/cap))`, reproducing the plan's 63/87/95% sequence
(module doc). `TransmitData` banks `ratio` and drains EC; `RecoverData` banks the
remainder and closes the key (requires `SurfaceLatched`). Labs: `rate = R·min(1,
D/T)`; abundant regime linear, scarce regime exponential decay; exhaustion defined
at `data_epsilon`; both landing events are Clamping. **Player-reachable**: all five
verbs.

**Tech** (`tech.rs`): `TechState { researched, track_levels, qualifications }`.
Nodes gate parts/facilities; upgrade levels are bought individually up to the
researched cap; `frozen_upgrades`/`build_part` implement the manufacture-time freeze
(`BuiltPart.upgrade_levels_at_build`). Only banked science is spendable. Tree
content: `data/techtree/home.ron`. **Player-reachable**: both buy verbs.

**Economy** (`economy.rs`): funds as `u64` micros (1e6/fund) — exact integer
arithmetic, one deterministic f64→micro rounding at the edge; negative funds never
exist (debit clamps, shortfall → `Insolvent`, Clamping). Bounded ledger (recorded
history: serialized, **not hashed**). Salaries: `SalaryDue` (Silent) on a 30-sim-day
absolute grid, lazily published by the Contracts slot. Recovery sites
(`SiteState`) hold inventories; `RecoverVessel` credits contents at
`recovery_efficiency`. Crew roster: `CrewMember { name, kind, skill, location,
salary_per_s, status }`, hire/board/transfer/lost. **PLAN-vs-code**: PLAN §3.5 also
names `StaffAttrition` and `FacilityDormant` events on unpaid salaries — the code
has only `SalaryDue` + `Insolvent` (economy.rs; no attrition event exists).
**Player-reachable**: RecoverVessel, HireCrew, BoardCrew, TransferCrew;
`ContractReward` is MCP-only.

**Contracts** (`contracts.rs`): offers draw from the fixed-arity KDF key
`{CONTRACTS, 0, offer_index}` — deterministic, not re-rollable by reload. Board
lazily refilled (pure function of `(templates, facts, offer_index, seed)`).
Prerequisites are concrete facts (researched nodes, completed-by-kind counts,
bodies visited) — no reputation currency. Completion is evaluated **by the sim on
relevant events** (orbit achieved, recovery, transfer complete…), never polled.
Deadlines fail with an authored penalty through the funds clamp. Templates:
`data/contracts/*.ron` (6). **Player-reachable**: AcceptContract.

**Colonies** (`colonies.rs`, 5,172 lines): the §3.5.1 flow network, five rules
verbatim — two-state Day/Night power with closed-form terminator crossings; algebraic
`T_eq`; deterministic allocation (resources ascend `ResourceId`, consumers
`(priority, ColonyId, ModuleIndex)`, full-rate grants then pro-rata remainder, with a
least-fixpoint grounding pass so cyclic converters pin at 0); next event = min over
store bounds + terminator + inspection; explicit `ColonyState`
(Nominal/Degraded/Dormant/Abandoned) with transition events. Stores are
epoch-anchored linear (`level(t) = anchor + net·(t−anchor)/TICK_HZ`) — partition-
independent like rails elements. `Dormant`/`Abandoned` schedule nothing (the event-
budget protection). **PLAN-vs-code**: PLAN rule 1 wants an *authored* per-latitude
day-average insolation table; the code uses the tilt-0 closed form `(2/π)·S·cos φ`
(no table authored to date; colonies.rs rule 1). Authored content: one Rask colony
in `data/systems/home/colonies.ron`, founded at boot. **Player-reachable**: all
colony verbs incl. found/abandon/evacuate/inspect/join/leave + colony↔vessel
transfers.

**Manufacture** (`manufacture.rs`): `ManufactureCraft` is a reservation — checks and
atomically spends the launch site's inventory rows + funds at apply, schedules
`ConstructionComplete` at `now + ceil(mass/rate·TICK_HZ)`; the craft materializes
only at the event, surface-latched at the site. Every order reserves a future vessel
slot. **Player-reachable**: this IS the Launch button (`csp-view/src/lib.rs:3020`);
the old free-instant spawn survives only as the site-less sandbox fallback.
`manufacture_rate_kg_per_s = 50.0` (`data/balance.ron`).

**Life support** (`life_support.rs`): per-save toggle (default OFF;
`csp-app` enables at startup), `LifeSupportCheck` (Silent) per
`check_period_ticks` debits `rate·period` per crew member from tank contents;
shortfalls schedule `VesselLifeSupportDepleted` (Clamping) one tick out, which marks
crew `Lost` after re-checking. A `tank.life.s` part exists.
**Player-reachable**: behavior only (no toggle UI).

**Reliability** (`reliability.rs`): `λ(t) = λ_base·Π(stress)·qual_factor` operating,
`λ_base·dormant_stress·scale` dormant; Λ accrues; failure at pre-drawn
`θ₀ = −ln U₀` from `{RELIABILITY, part_instance_id, 0}` — drawn once at creation,
renewed `θₖ = θₖ₋₁ + (−ln Uₖ)` after each failure; revert restores the record, so
revert is a retry, not a reroll (§3.4). Dormant failures draw only non-fatal modes.
Rails/latched parts are epoch-anchored (`Λ(t) = integral + λ·(t−epoch)`); in-bubble
parts accrue per tick. `PartFailure` is Silent, surfaced via the notification log +
`fired_events`. Qualification: `RunBenchTest` spends funds + site inventory, delivers
`bench_units` at `BenchTestComplete`; flight units credit on burn completion.
**Player-reachable**: RunBenchTest + the failure/revert experience.

**Crew/resource transfer processes** (`transfers.rs`): `TransferResource`/
`TransferCrew` validate + **debit at apply** (reservation by construction), land at
a rate-computed `TransferComplete` (Clamping). Link precondition (checked once, at
apply): a live soft-capture joint, or both InBubble within `transfer_radius_m`
(100 m), or both SurfaceLatched on the same body within the radius.
**Player-reachable**: both.

**Scripting/autopilot** (`scripts.rs` + `csp-script`/`csp-ai`): scripts are
tick-entry-point programs — `on_tick(ctx)` once per tick, op budget
`DEFAULT_SCRIPT_OP_BUDGET = 20_000`, budget-exceed aborts + strike, 5 strikes
disable. Cross-tick state is the `persist` map (serialized + hashed). Wakes:
`WakeCondition` resolves closed-form (absolute tick, time-to-Ap/Pe, SOI entry, node
start, altitude crossing); the host publishes `ScriptWake` (Clamping). Poll
detection demotes warp. **Player-reachable**: load/unload/enable/engage via Program
window (the script source is typed into the panel; `csp-view/src/lib.rs:1236`).

---

## 8. Save / replay / state hash

**Snapshot/restore round-trip**: `World::snapshot()` (lib.rs L3556) first runs
`check_serializable` (L3497), then serializes the §7.2 set: clock, warp, source-seq
counters, all arenas + next-id counters, event heap (sorted `(tick, seq)`) +
`next_event_seq`, configs, science/economy/contract/colony/reliability/script
state, vessel craft + residence payloads + latch records. `World::restore`
(L2926) re-validates everything (config validity, id < next-id, node↔event pairing,
transfer/project/contract pairings, craft shape, refused-pairs coherence) — a
crafted or corrupt snapshot is refused with `Fatal`, never half-loaded
(see tests `restore_rejects_crafted_*`, lib.rs L13513+). Re-derived on load, never
serialized: physics-backend internals, `WeldedBody`s, aero tables, joints (empty at
restore by §7.4), the orbit model (data-derived).

**Save refusal rules** (`check_serializable`, lib.rs L3497 — PLAN §7.4 verbatim):
(1) session save-blocked from a loader/install error; (2) commands pending at the
boundary; (3) any live joint island; (4) any bubble body with live contacts and not
surface-latched. Landed play saves normally (a settled vessel latches).

**CSPSAVE codec** (`save.rs`): `CSPSAVE\0 | u32 header_len | JSON header |
DEFLATE(CBOR body)`. Header records `save_version`, `game_version`,
`hash_schema_version`, `kdf_version`, `data_hash_version`, `data_set_hash`, label,
codecs, and `body_len` (truncation guard). `save_version` selects a frozen schema;
v1 bodies are **not loadable** (the v1 schema drifted with live types — documented
in the module table); v2 is current. Pre-1.0 policy: a bump may invalidate saves;
migration chains begin at the first public build. Replays stay bincode and are never
migrated.

**State hash** (`csp-core/src/hash.rs` + `World::state_hash` lib.rs L8754):
BLAKE3-256 over a hand-written canonical walk — never serde, never derive order.
`HASH_SCHEMA_VERSION = 21` (hash.rs:207; the version history log in the same file is
the changelog). Rules: version word first; little-endian fixed-width ints; f64
`to_bits()` with `-0.0 → 0.0`; NaN/±inf forbidden (Fatal); arenas in ascending-ID
order with `len` prefixes; enums via declared stable `u16` discriminants
(`StableDiscriminant`). Recorded history (command log, ledger, notification log) is
serialized but **not hashed** — the hash is the state's forward-looking content.
`state_hash_masked(in_bubble)` excludes bubble-resident vessels for the equivalence
test (a named function, not a test hack).

**Replays & goldens** (`csp-bench/src/replay.rs`): artifact
`{ fingerprint, save_seed, data_fixture_hash, initial snapshot, final_tick,
Vec<(CommandKey, SimCommand)> }`, bincode-canonical. Golden layout:
`data/test-fixtures/<scenario>/` = frozen data files + `replay.bin` +
`expected-hash.txt` + `fingerprint.json` (the artifacts are excluded from the
fixture hash). Scenarios today: `trivial`, `flight-launch`.

**Running the sim tests** (see [BUILD-AND-DEV.md](BUILD-AND-DEV.md) for full CI):
- `cargo test -p csp-sim` — the 199 inline tests (§9 patterns).
- `cargo run -p csp-bench --bin csp-replay -- list | record <scenario> <dir>
  [--force] | verify <dir>` — golden record/verify; exit 1 = hash mismatch,
  2 = harness error.
- `cargo xtask bless-goldens --reason "…"` — the gated regeneration path; refuses a
  dirty tree and a hash change without a `HASH_SCHEMA_VERSION` bump
  (`xtask/src/main.rs`).
- Do **not** run these from this doc; the numbers above are the interfaces.

---

## 9. Invariants a dev must never break

1. **Determinism is a property of the command log.** Same binary + platform + save +
   log ⇒ identical `state_hash`. Everything below serves this.
2. **dt is 1/64 s forever.** Warp changes how many ticks run, never the step.
3. **Total command order**: `CommandKey { tick, source_class, source_id, source_seq }`,
   stamped at dequeue. Reordering `SourceClass` or `SimCommand` variants is a
   golden-breaking change requiring a `HASH_SCHEMA_VERSION` bump.
4. **One integrator per subsystem, and it is the interval/closed-form one.** The tick
   loop drives only the bubble. Mutation of rails/subsystem state happens only at
   absolute-tick events and commands — never per interval (the `fl(fl(x−r·dt₁)−r·dt₂)`
   hazard). The enforcement test is
   `warp_x1_vs_x1e5_equivalence_10_sim_days` (lib.rs tests L13135): 10 sim-days at
   ×1 vs ×10⁵ must hash identically under the bubble mask.
5. **Every derived cache is a pure function of serialized state.** No time-triggered
   caches (the §2.4 quantized-mass rule). Corollary: save → load → N ticks must hash
   like uninterrupted N ticks (`save_load_*` tests, e.g. L20715).
6. **Ascending-ID iteration for anything that affects state**; BTree containers only.
7. **Epoch anchoring**: rails elements, Λ accrual, colony stores — stored values are
   anchors, evaluated in closed form, mutated only at events/commands.
8. **Hash coverage**: any new state field must join the hand-written walk in
   `state_hash`/`hash_*` in the same commit, and any non-hashed field must be
   recorded history by the documented classes. Bump `HASH_SCHEMA_VERSION` in the
   same commit that reblesses goldens.
9. **Event budget discipline**: ≤2000 events/frame (checked between iterations),
   ≤1024 per same-tick batch chunk, subsystems publish **coarse** next-event times
   (a per-cycle publisher is a review rejection). Any thresholded/nonlinear
   subsystem must publish its next event time — a subsystem that can only be
   computed by stepping is a bug under rails warp.
10. **`step_frame` never panics in release**; `Fatal` rolls the whole iteration back
    via `TickShadow` (coverage is itself tested — `mod fatal_rollback`, lib.rs
    L23894). Rejected commands and degraded events roll back completely and report.
11. **Commands are absolute, never relative** (`SetThrottle(f64)`, never
    `AdjustThrottle(Δ)`) — rate-based input is integrated app-side against
    `ticks_consumed` first.
12. **The control lease gates every control-affecting command**;
    `control_gated_vessel` is the checklist (adding a control command without adding
    it there is a bug).

**Test patterns** (all in `csp-sim/src/lib.rs` `mod tests` + `csp-bench`):
- Equivalence/property: the ×1-vs-×10⁵ hash test; weld permutation invariance
  (`csp-phys/src/weld.rs` tests); heap total order under ties (`events.rs` tests);
  solver round-trips across `e ∈ {0…3}` and Δt decades (`orbit.rs` tests).
- Round-trips: snapshot→restore→hash at mid-flight states
  (`maneuver_node_snapshot_round_trip` L17335, `save_load_mid_transfer` L20715,
  `refused_pair_survives_save_load` L18728).
- End-to-end mission shapes: `duva_reachable_in_sim_end_to_end` (L18269: spawn →
  SetManeuver → executor → Exit{terrin} → Enter{duva}); docking merge equivalence
  (M6-C block).
- Adversarial restore: `restore_rejects_crafted_*` (L13513+) feed deliberately
  corrupt snapshots and assert `Fatal` refusal.
- Rollback coverage: `mod fatal_rollback` — a `Fatal` mid-iteration must restore
  every mutation channel, including unshadowed-looking ones (queue pushes, script
  logs).
- Golden replays: `csp-replay verify` over the frozen fixtures; nightly adds the
  colony-century / pathological-density scenarios (`csp-bench/src/scenarios.rs`).

---

*Verified against the working tree 2026-10-26. When this document and the code
disagree, the code wins — fix the document in the same commit.*
