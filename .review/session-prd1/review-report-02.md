# PRD-02 Review — csp-sim command contract + World shell

**Cluster:** `crates/csp-sim/src/lib.rs` — `SimCommand` enum (L515–1143), `CommandKey`/`SourceClass`/`Lease` shell (L440–506), `apply_command` dispatch + all `apply_*` handlers (L4773–5042, 5081–6825), `drain_at`/`drain_boundary` (L6849–6960), `advance_interval_subsystems` (L6969–7028), `check_serializable`/`snapshot` (L3210–3302), plus every module-side apply (`transfers.rs`, `research.rs`, `tech.rs`, `economy.rs`, `contracts.rs`, `colonies.rs`, `manufacture.rs`, `life_support.rs`, `reliability.rs`).

**Method:** enumerated every `SimCommand` variant (the PRD says "~40" — the actual count is **51**), then grep'd every non-csp-sim crate for `SimCommand::<Variant>` to classify each as UI-reachable / agent-bench-only / dead-by-content, and read every apply for stub-vs-complete and silent-no-op behavior.

---

## 1. Headline findings

1. **Every variant has a real, complete apply** — no `todo!`/empty arms, no silent swallow. The dispatch table (`lib.rs:4799–5040`) is exhaustive and each handler is fully implemented. Completeness is not the problem.
2. **Reachability is the problem.** Only **~15 of 51** variants have any player-facing producer. The rest are reachable *only* through MCP's raw `sim.command` escape hatch (`csp-server/src/mcp.rs:537–563`), csp-bench scenarios, or not at all — and three (`Undock`, `StartResearch`, `SetAeroConfig`) are dead-by-content even for agents because the required parts don't exist in `data/parts/`.
3. **One High-severity silent no-op on a reachable path:** `RunExperiment` is dropped at the *view* layer for every space situation — `science_ctx` is `None` whenever `biome` is `None`, and the only experiment part in the catalog is authored for `SpaceLow`/`SpaceHigh` (`csp-view/lib.rs:2540`, `:712`; `data/parts/csp.science.canister.ron`).
4. **One genuine severity-inconsistency defect:** `apply_bench_test` returns `Fatal` on invalid config where every sibling apply returns `Recoverable` (`reliability.rs:1137`).
5. **Command→apply honesty is otherwise good.** Rejections roll back via per-command `TickShadow` and land on `AdvanceReport.rejected` with a typed `RejectKind`; csp-app surfaces every `Ui`-channel rejection as a flight notice (`csp-app/src/lib.rs:944–956`). No *apply* silently eats a player command — the one silent drop lives one layer up, in the view's mirror (D2).

---

## 2. Per-variant reachability audit (51 variants)

Legend: **[UI]** player-reachable through the shipped app · **[AGENT]** reachable only via MCP `sim.command` / dedicated MCP tools / scripts / bench · **[DEAD]** no producer anywhere, or gated by missing content.

### Time & spawn
| Variant | Reach | Producer / evidence |
|---|---|---|
| `SetWarp(u32)` | [UI] | `csp-app/src/lib.rs:529,865,982` (warp controller logs the *multiplier*, not the rung) |
| `AdvanceTo(u64)` | [UI] | `csp-app/src/lib.rs:879` |
| `SpawnVessel` | [AGENT] | only tests/bench (`csp-view/src/map.rs:2283` test, `csp-bench/{contracts.rs:218,perf.rs:805}`); **no UI path mints an on-rails vessel** |
| `SpawnPhysicalVessel` | [UI] | launch button `csp-view/src/lib.rs:2213` + `demo::seed_pad_craft` at startup |

### Vessel control
| Variant | Reach | Producer / evidence |
|---|---|---|
| `SetThrottle` | [UI] | throttle axis `csp-app/src/lib.rs:1772,1925,3103`; MCP `vessel.control` `mcp.rs:423`; script `ControlApi` `csp-script/lib.rs:151` |
| `SetAttitude` | [UI] | attitude verbs `csp-app/src/lib.rs:1706`; MCP `mcp.rs:381–420`; script |
| `Unlatch` | [AGENT] | redundant with the control-command unlatch (`unlatched_body`); no producer |
| `ToRails` | [AGENT] | no producer — bubble→rails happens internally via burn-executor completion (`apply_to_rails_at`); the manual verb is MCP/test only (`lib.rs:14352+` tests) |
| `ToBubble` | [UI] | focus/recover pin `csp-view/src/lib.rs:1946` |
| `SetVesselTracked` | [AGENT] | **zero producers** — no tracking opt-in UI exists, so no vessel's `SoiTransition` is ever `Clamping` in a player run |
| `TakeControl` | [UI] | view lease transfer `csp-view/src/lib.rs:1570`; MCP `vessel.take_control` `mcp.rs:453`; bench eval `csp-bench/eval.rs:162` |

### Aero / parts
| Variant | Reach | Producer / evidence |
|---|---|---|
| `InstallAeroTables` | [DEAD] | the §2.4 worker path — **never issued**; sim computes tables itself at spawn/`ensure_aero` (`lib.rs:5529–5538`). Command + debug byte-compare exist but nothing calls them |
| `SetAeroConfig` | [DEAD-by-content] | applies fine, but **no part in `data/parts/` has deployable config geometry** — masks are always `ConfigMask::ALL`; command is a no-op in practice |
| `JettisonPart` | [UI] | staging `csp-view/src/lib.rs:2347`; script `ControlApi` `csp-script/lib.rs:191` |
| `Undock` | [DEAD-by-content] | requires a `DockSeam` recorded at latch (`lib.rs:6602`); **no `DockPort` part exists in `data/parts/`** so seams never form — the apply can only ever reject "no such seam" |

### Maneuver / orbit
| Variant | Reach | Producer / evidence |
|---|---|---|
| `SetManeuver` | [UI] | map maneuver panel `csp-view/src/lib.rs:679`; MCP `mission.nodes` `mcp.rs:475`; script |
| `DeleteManeuver` | [UI] | `csp-view/src/lib.rs:686`; MCP `mcp.rs:487`; script |
| `WatchTransferWindow` | [AGENT] | **no UI** — `transfer::plan_transfer` is library/MCP-only; the watch event itself is complete (`apply_watch_window` L5234) |

### Transfers / crew
| Variant | Reach | Producer / evidence |
|---|---|---|
| `TransferResource` | [AGENT] | script `ControlApi` `csp-script/lib.rs:223`; no UI anywhere |
| `TransferCrew` | [AGENT] | no producer outside tests |
| `BoardCrew` | [AGENT] | no producer outside tests; pod has `CrewSeat(slots: 2)` (`data/parts/csp.pod.mk1.ron`) but no one can ever board |
| `JoinColonyInventory` / `LeaveColonyInventory` | [AGENT] | colony tests only |
| `TransferColonyResource` / `TransferColonyCrew` | [AGENT] | colony tests only |

### Science
| Variant | Reach | Producer / evidence |
|---|---|---|
| `RunExperiment` | [UI]† | Program window `csp-view/src/lib.rs:713` — †**silently dropped in every space situation** (`science_ctx` None; defect D2) |
| `TransmitData` | [UI] | `csp-view/src/lib.rs:725` |
| `RecoverData` | [UI]* | `csp-view/src/lib.rs:727` — *issued* but requires `SurfaceLatched` (`research.rs:855`); settle latch (0.05 m/s, 128 ticks) with no legs/parachute in the catalog makes this effectively transmit-only |
| `StartResearch` | [DEAD-by-content] | **no `Lab` module part exists** — `research.rs:952–955` can only reject "node carries no lab"; `researchers` is the documented non-crew stub (`ResearcherAssign`, `research.rs:183`) |
| `StopResearch` | [DEAD] | unreachable — no project can ever start |

### Tech / economy / contracts
| Variant | Reach | Producer / evidence |
|---|---|---|
| `ResearchTech` | [UI] | `csp-view/src/lib.rs:704` |
| `ResearchUpgradeLevel` | [UI] | `csp-view/src/lib.rs:707` |
| `AcceptContract` | [UI] | `csp-view/src/lib.rs:710` |
| `RecoverVessel` | [AGENT] | **no UI** — fully implemented apply (`economy.rs:867+`) incl. site credit/crew/science, but the player can never issue it; bench only (`csp-bench/scenarios.rs:469`) |
| `HireCrew` | [AGENT] | `csp-bench/economy.rs:102` only — the roster can never gain a member in a player run |
| `ContractReward` | [AGENT] | **documented stub** — "exists now so the §8.3 economy harness exercises the funds path" (`lib.rs:903–905`, `economy.rs:841`); a signed-micros funds faucet |

### Colonies / manufacture / LS
| Variant | Reach | Producer / evidence |
|---|---|---|
| `FoundColony` | [AGENT] | bench `csp-bench/{economy.rs:253,perf.rs:912}`; the authored Rask colony is founded at boot internally, not via this command |
| `AbandonColony` / `AssignCrewToColony` / `EvacuateColony` / `ScheduleColonyInspection` | [AGENT] | tests only |
| `DamageColonyModule` | [AGENT] | command-side stand-in — the real path exists internally: debris impacts call `apply_surface_impacts` (`colonies.rs:1914`, wired at `lib.rs:4442`) |
| `ManufactureCraft` | [AGENT] | **launch bypasses it** — `launch_craft` issues `SpawnPhysicalVessel` (instant); funds ARE still charged at spawn (`lib.rs:5495–5500,5582`), so "free launch" is wrong only on the build-time/site-inventory side |
| `SetLifeSupportEnabled` | [UI-ish] | app startup issues `{enabled:true}` once (`csp-app/src/lib.rs:565`) — there is no toggle; inert anyway since no crew can board |

### Scripts / autopilot / reliability / misc
| Variant | Reach | Producer / evidence |
|---|---|---|
| `LoadScript` / `UnloadScript` / `SetScriptEnabled` | [AGENT] | RhaiHost installed (`csp-view/lib.rs:1125`) but **no editor/console UI** — bench + MCP only (`csp-bench/{replay.rs:1362,scenario.rs:481,scenarios.rs:1000}`) |
| `EngageAutopilot` / `DisengageAutopilot` | [AGENT] | `AscentAutopilot` driver installed (`csp-view/lib.rs:1126`), apply checks driver fitness honestly (`lib.rs:5011–5019`), but no UI issues it; bench only (`scenarios.rs:928`) |
| `RunBenchTest` | [AGENT] | no UI; `facility.test_stand` tech unlock has no effect |
| `SetAlarm` | [AGENT] | script `ControlApi::alarm` (`csp-script/lib.rs:235`) + MCP; **no KAC-style alarm UI exists** — the only Clamping events a player can create are `ManeuverNodeStart` |
| `sim.command` (meta) | — | `mcp.rs:537–563` raw `serde_json` decode — **exposes all 51 variants to agents**, including everything the player can't reach |

**Count:** 15 UI-reachable (`SetWarp`, `AdvanceTo`, `SpawnPhysicalVessel`, `SetThrottle`, `SetAttitude`, `ToBubble`, `JettisonPart`, `SetManeuver`, `DeleteManeuver`, `TakeControl`, `RunExperiment`, `TransmitData`, `RecoverData`*, `ResearchTech`, `ResearchUpgradeLevel`, `AcceptContract`, `SetLifeSupportEnabled` — ~15–16 counting the startup-only ones) · ~32 agent-only · 3 dead-by-content.

---

## 3. Correctness defects

### D1 — `apply_bench_test` escalates bad config to `Fatal` where every sibling uses `Recoverable`
- **Severity:** Medium (agent-reachable world halt)
- **File:line:** `crates/csp-sim/src/reliability.rs:1137–1138`
- **Problem:** `if !cfg.is_valid() { return Err(SimError::Fatal("reliability: invalid config")) }`.
- **Evidence:** `apply_manufacture_craft` documents the rule it breaks — *"Input-reachable like the five transfer sites (a config a caller installed pre-check or a corrupt record): a `Recoverable` rejection, never a `Fatal`"* (`manufacture.rs:118–122`). Every sibling apply follows it: `economy.rs:873–874` (recover), `research.rs:677,773,850` (science), `economy.rs:634` (hire path), `transfers.rs:831`.
- **Impact:** `RunBenchTest` is reachable via MCP `sim.command`. A malformed-but-decodable `balance.ron` reliability section turns one agent command into a `Fatal` — `step_frame` returns `Err`, and csp-app treats `Fatal` as session-ending (`on_sim_fatal`, `csp-app/src/lib.rs:986–991`). A bad input kills the world instead of rejecting on the channel.
- **Suggestion:** change to `SimError::Recoverable("bench test: invalid config")`.

### D2 — `RunExperiment` is silently dropped for every space situation (player-facing silent no-op)
- **Severity:** **High** — the only authored instrument is unusable in the contexts it was built for; a "runnable" button that does nothing is a silent reject on a reachable path.
- **File:line:** `crates/csp-view/src/lib.rs:2540` (`science_ctx: ctx_biome.zip(ctx_situation)`), `csp-view/src/lib.rs:711–719` (`program_command` — `mirror.science_ctx.clone()?` → `None` → `RunExperiment` dropped), `csp-view/src/lib.rs:2603–2607` (biome is only resolved for `is_surface_side()` situations).
- **Problem:** `science_ctx` is `Option<(BiomeId, Situation)>` formed by zipping `ctx_biome` and `ctx_situation`. `ctx_biome` is `None` for every non-surface-side situation (`stage_program_vessel` only looks a biome up when `situation.is_surface_side()` — `Landed|Splashed|FlyingLow|FlyingHigh`). In `SpaceLow`/`SpaceHigh` — i.e. on rails or in the bubble above the atmosphere — `science_ctx` is therefore `None`, so `program_command`'s `?` drops `C::RunExperiment` entirely: no command is ever enqueued.
- **Evidence:**
  - `csp-view/src/lib.rs:712`: `C::RunExperiment { .. } => { let (biome, situation) = mirror.science_ctx.clone()?; ... }` — the only producer of `RunExperiment` short-circuits to `None`.
  - `crates/csp-sim/src/research.rs:127–129`: `DataKey.biome` "may be an empty id for … space situations" — the sim expects `BiomeId::from("")` (test at `lib.rs:19387`, `research.rs:1220`); the view never produces it.
  - `data/parts/csp.science.canister.ron`: `situations: 49` = bits {0,4,5} = `Landed|SpaceLow|SpaceHigh` — **the only experiment part in the catalog is authored primarily for space**.
  - The row still displays as runnable: `blocked` is computed only from `spec.situations` + `cap_for` (`csp-view/src/lib.rs:2640–2653`) — no biome needed, so `runnable: true` in space; the click then evaporates with no notice (the `None` drop is not surfaced on the flight cue channel).
- **Impact:** in a real session the player can run the materials study only while `Landed`/`Splashed`/flying — never in orbit or deep space, defeating the "fly to space → run science → transmit" loop. Combined with `RecoverData` needing `SurfaceLatched` (no legs/parachute), science is effectively a pad-side + atmospheric curiosity.
- **Suggestion:** stage `biome.unwrap_or_else(|| BiomeId::from(""))` into the mirror for space situations (match the sim's empty-biome convention), or make `science_ctx` carry `Option<BiomeId>` and default at command build. A flight notice on the `None` drop would also close the silent-fail class generally.

### D3 — MCP `timewarp.set` documents a rung index but `SetWarp` stores a multiplier
- **Severity:** Low
- **File:line:** `crates/csp-server/src/mcp.rs:500–508` vs `csp-app/src/warp.rs:222` (`decide` emits `WARP_LADDER[next]` — the value, not the index).
- **Problem:** the tool doc says *"`timewarp.set {level}` — the stored ladder rung"*; the app logs `SetWarp(WARP_LADDER[idx])` i.e. literal multipliers (1,2,10,100,…). An agent sending `level: 3` expecting the ×100 rung stores multiplier ×3.
- **Impact:** cosmetic — the sim stores `warp_level` verbatim and interprets nothing (`lib.rs:4800`), but `warp_level` is hashed, snapshotted, and published to telemetry, so the recorded rung disagrees with actual advance behavior driven by `AdvanceTo`.
- **Suggestion:** either rename the arg to `multiplier` in docs, or translate rung→multiplier server-side; a test asserting `SetWarp` semantic equality with the app's emitted values would pin it.

### D4 — Lease gate leaves state-destroying science commands ungated
- **Severity:** Low (design-documented but questionable scope)
- **File:line:** `crates/csp-sim/src/lib.rs:1172–1192` (`control_gated_vessel`) — `RunExperiment`, `TransmitData`, `RecoverData`, `StartResearch`, `TransferResource`, `TransferCrew`, `BoardCrew` are deliberately ungated ("move cargo, they do not steer").
- **Problem:** `TransmitData`/`RecoverData` *destroy* pooled data (drop units, close keys); `RunExperiment` permanently spends a non-reusable module (`spent` mark — the canister is one-shot). An `External` client holding no lease can burn the player's one-shot experiment or dump its data at the transmit fraction — destructive, not just "cargo".
- **Impact:** in a server session, any connected agent can consume another player's science inventory without holding the vessel lease.
- **Suggestion:** add `RunExperiment`, `TransmitData`, `RecoverData`, `StartResearch` to `control_gated_vessel` (they spend unique vessel state — the `RecoverVessel`/`InstallAeroTables` precedent); keep `TransferResource`/`BoardCrew` ungated if the cargo rationale stands.

### D5 — `SetWarp` accepts and stores any `u32` unclamped
- **Severity:** Low
- **File:line:** `lib.rs:4800` — `SimCommand::SetWarp(level) => self.warp_level = *level`.
- **Problem:** no ladder validation; `warp_level` is hashed/snapshotted. Test at `lib.rs:11257` asserts `u32::MAX` is stored. Deliberate ("the sim interprets nothing") and covered by the shadow (L1755), so not a correctness bug — but it means the stored warp is only as honest as the last producer, and a stale `warp_level` survives a save while the wall-side `WarpController` restarts at ×1.
- **Suggestion:** none required (documented design); optionally clamp to `WARP_LADDER`'s max at apply for hygiene.

### D6 — `apply_set_life_support` logs a same-value command as applied
- **Severity:** Very low
- **File:line:** `life_support.rs:230–232` — `if enabled == self.life_support_enabled { return Ok(()) }`.
- **Problem:** early-returns Ok *before* any shadow — the command lands in `command_log` as applied but mutates nothing. Idempotent and replay-safe, just log noise.
- **Suggestion:** none required; note only.

### D7 — `sim.command` MCP escape hatch exposes the entire 51-variant surface
- **Severity:** Informational (by design)
- **File:line:** `mcp.rs:312,537–563` — `serde_json::from_value::<SimCommand>` on arbitrary JSON.
- **Problem/observation:** this is the reason nothing is agent-unreachable — every unreachable-from-UI variant (crew, colonies, contracts internals, `ContractReward` faucet, `DamageColonyModule` stand-in) is a single JSON command away for any connected client, gated only by the lease check for control-affecting variants. `ContractReward{micros}` is a direct funds faucet.
- **Suggestion:** if the server is ever exposed beyond localhost, consider a capability filter on `sim.command` (whitelist or at minimum exclude `ContractReward`).

---

## 4. Command→apply honesty assessment (player-reachable paths)

**No apply silently rejects or no-ops for a player-reachable command.** Verified:

- **Rejection discipline:** `drain_at` (L6903–6957) stamps at dequeue, runs each apply under a fresh `TickShadow`, and on `Recoverable` restores and reports `RejectedCommand{key, dequeue_index, kind, error}`. A rejected command spends no `source_seq` and leaves no log entry — post-drain state is identical to it never existing (L1266–1270).
- **Player surfacing:** `csp-app/src/lib.rs:944–956` — every rejected `Ui`-channel command produces a `"COMMAND REFUSED — …"` flight notice. `csp-view/src/lib.rs:1496–1509` uses the typed `RejectKind::ToBubble` (not text matching) to release the recover pin on rejection.
- **Pre-flight mirrors the apply:** launch runs `launch_preflight` against the same refusals before enqueuing (`csp-view/lib.rs:2191–2201`); `fire_stage` checks `InBubble` before spending the stage (L2311–2336); the maneuver picker filters to `OnRails` vessels (L665–669); `to_bubble_eligible` re-runs the apply's own reject classes (L1970+).
- **Documented clamps, not silent:** `SetThrottle` clamps a positive throttle to 0 on an all-starved vessel (L4816–4822) — the requested value is logged, the effect is clamped, the HUD shows the body's actual throttle; `SetAeroConfig` resolves over-cap masks by Hamming distance (L6405–6414) — documented in the variant doc.
- **Subtlety worth noting:** `SetThrottle{0}`/`SetAttitude` on a `SurfaceLatched` vessel *unlatches* it first (`unlatched_body` L3542–3630) — the documented §2.6.4 rule, but the side effect scrubs all pending vessel events (L3607–3613) and drops a `Scheduled` maneuver node (L3596–3601). In practice unreachable: a `Scheduled` node requires `OnRails` at apply (`apply_set_maneuver` L6018) and both `to_rails`/`to_bubble` clears `maneuver` (L5702, L5949), so a latched vessel cannot actually hold one — the code path is defensive dead code.
- **One real silent drop — see D2:** `program_command` returns `None` on `mirror.science_ctx.clone()?` (`csp-view/lib.rs:712`). The stated intent is "a one-frame-stale panel command is dropped, not misfired" (`csp-view/lib.rs:695–701`), but the mirror is *structurally* `None` for all space situations — it is not a stale-frame race, it is a permanent state for any vessel above the atmosphere. The `TransmitData`/`RecoverData` rows escape (they key off `mirror.vessel` + `mirror.keys`, not `science_ctx`), so transmit still works in orbit — but collection cannot, making the whole science loop one-directional where it matters most.

---

## 5. Completeness of the drain/tick machine (focus: stubs vs real)

- **`apply_command` (L4773–5042):** exhaustive — every variant either mutates directly or forwards to a module apply; no stub arms. `ContractReward` is the only self-described *stub interface* (documented, M7-C era) — and it's fully functional, just semantically thin.
- **`advance_interval_subsystems` (L6969–7028):** `Orbits` (re-solve solvability check), `Colonies`/`LifeSupport`/`Reliability`/`Contracts` slots are all real publish paths; the other slots are documented honest no-ops (research is event-driven — correct per §2.6.5's contract).
- **`check_serializable` (L3210–3251):** covers §7.4 items 1 (joint islands), 2 (live-contact non-latched bubble bodies), and the pending-queue invariant. Item 3 ("Stage-2 weld pending") is **vacuously satisfied** — the worker path `InstallAeroTables` is never issued so no pending-tables state can exist; worth a comment that the rule is dead by the Stage-2 deviation, not implemented. Item 4 (loader `SaveBlocking`) is session-level, correctly out of sim scope.
- **`snapshot()` field coverage (L3257–3301):** covers every progression arena (crew roster in `economy`, colonies, contracts, science+tech, builds, transfers, scripts, autopilots, benches, notifications, LS flag) plus the queue-empty precondition. No missing progression state observed.
- **In-range control drain (L3705–3728, 8795):** bubble ticks are real boundaries — `drain_at(t)` + `control_tick(t)` per tick, `not_before = clock+1` honors the one-tick latency. Complete.

---

## 6. Reachability-gap summary for the PRD ledger

The sim's command surface is the PRD's §3 feature list made literal — and ~65% of it is invisible to the player:

- **Fully dead-by-content:** `Undock` (no `DockPort` part), `StartResearch`/`StopResearch` (no `Lab` part), `SetAeroConfig` (no deployable geometry).
- **Agent-only with a real gameplay hole:** `RecoverVessel` (no recovery flow — the KSP core loop), `HireCrew`+`BoardCrew` (roster stays empty forever; `SalaryDue` fires for nobody; `CrewSeat` slots on the pod are decorative; `SetLifeSupportEnabled{true}` at startup toggles a subsystem with zero consumers), all 10 colony/transfer commands, `ManufactureCraft` (launch bypasses the build pipeline — funds still debited at spawn via `craft_cost_micros`), `RunBenchTest`, `LoadScript`/`UnloadScript`/`SetScriptEnabled`, `EngageAutopilot`/`DisengageAutopilot`, `SetAlarm`, `WatchTransferWindow`, `SetVesselTracked`, `ContractReward`.
- **Agent-only but redundant/internal:** `Unlatch`, `ToRails` (auto paths cover both), `InstallAeroTables` (sim-side recompute covers it), `SpawnVessel` (no rails-spawn UI — arguably by design), `DamageColonyModule` (real damage arrives via debris impacts).
- **Consequence for warp demotion:** with no `SetVesselTracked`/`SetAlarm`/`WatchTransferWindow` producers, the only `Clamping` events a player can ever fire are `ManeuverNodeStart` — the §2.6.5 clamp machinery is tested but near-dead in real play.

**Bottom line:** the command contract is honest, complete, and correctly rolled back — the deficit is entirely above it: the game simply never issues two-thirds of its own verbs.
