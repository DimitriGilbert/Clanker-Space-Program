# PRD-18 Review — Scripting + AI/automation

**Files reviewed:** `crates/csp-script/src/lib.rs` (1570), `crates/csp-script/tests/api_freeze.rs` (169), `crates/csp-ai/src/lib.rs` (222), plus the sim-side contract in `crates/csp-sim/src/scripts.rs` and `crates/csp-sim/src/lib.rs`, and every consumer reachable by grep (`csp-view`, `csp-app`, `csp-ui`, `csp-server`, `csp-bench`).

**Verdict:** The Rhai host is mechanically excellent — token-aware preprocessor, honest atomic-tick drain, real determinism pins (no_std + libm shadowing + no wall clock), memory ceilings, well-tested freeze. The problem is not quality, it is *existence*: nothing a player does can ever load a script or engage an autopilot, the promised `mission.*` script service and three of four §2.11 AI features don't exist, the `ControlApi` is a strict subset of §2.10's own promised verb list (action groups absent from the sim entirely), and §2.10's canonical scenario — "a script whose whole job is wait until Ap, then burn" — cannot be *started* on a rails or latched vessel. Automation is first-class only for MCP agents and bench harnesses.

---

## Findings

### F1 — [HIGH] No player path to scripts or the autopilot; the entire subsystem is agent/bench-only

**Where:** `crates/csp-view/src/lib.rs:1124–1126` (install), `crates/csp-view/src/lib.rs:305–331` (`UiOutbox`), `crates/csp-ui/src/settings.rs:190–237` (`KeyAction`), `crates/csp-view/src/lib.rs:224–233` (`ProgramMirror`); absence verified by grep over `csp-ui`, `csp-app`, `csp-view`.

**Problem:** `RhaiHost` and `AscentAutopilot` are installed into the live sim, but `LoadScript`, `UnloadScript`, `SetScriptEnabled`, `EngageAutopilot`, `DisengageAutopilot`, and `SetAlarm` are issued by **nothing** in the game binary:

- `csp-ui`: zero references (the only "script" hits are tutorial-test wording).
- `csp-app`: one comment mention (`lib.rs:928`, about `PlayerAlarm` demotion).
- `csp-view`: only the two install lines.
- `UiOutbox` — the sole UI→`SimCommand` plumbing — has fields for warp/mode/launch/craft/maneuver/program/settings and **no** script, autopilot, or alarm field. `ProgramMirror` mirrors funds/science/contracts/tech only; there is no mirror of the script or autopilot arenas, so a future panel has nothing to read either.
- `KeyAction` (the entire bindable vocabulary) has no automation verb.

The only live paths in are `sim.command` raw passthrough (`crates/csp-server/src/mcp.rs:312`, descriptor at `mcp.rs:815–822`) and bench scenarios (`crates/csp-bench/src/scenarios.rs:928` `EngageAutopilot`, `:1000` `LoadScript`).

**Impact:** §2.10 calls scripting "first-class" and §2.12 lists "in-game scripts" and "built-in autopilots" as two of four coequal clients. For the player, the feature set is *zero*: no script editor, no console, no autopilot engage button, no alarm creation. An MCP agent can fly a fully automated campaign; a player cannot run one line of Rhai.

**Suggestion:** Minimum viable surface: a `UiOutbox`/`ProgramMirror` extension carrying `LoadScript`/`UnloadScript`/`SetScriptEnabled`/`EngageAutopilot`/`DisengageAutopilot`/`SetAlarm`, a script-status mirror (enabled/strikes/disabled_reason/wake_tick), an autopilot engage button on the flight HUD (Ascent with apo/pe fields is UI-trivial), and an alarm list in the map/program window. A full editor can wait; a "load .rhai file from craft dir" affordance plus the existing `script_log` would already make §2.10 reachable.

---

### F2 — [HIGH] §2.11 is ~1/4 implemented: ascent autopilot only; no transfer optimizer, no design assistant, no `mission.*` script service

**Where:** `crates/csp-ai/src/lib.rs` (whole file — 222 lines, one driver); `crates/csp-sim/src/scripts.rs:239–249` (`AutopilotProgram` has exactly `Ascent`); `crates/csp-sim/src/transfer.rs:1188` (`plan_transfer` — called only by tests: `transfer.rs:2017+`, `csp-sim/src/lib.rs:16672`); `crates/csp-phys/src/profile.rs:1–20` (MissionProfile stub); `crates/csp-ui/src/vab.rs:458,492` (`MissionProfile::default()`).

**Problem:** §2.11's v1 list is: ascent guidance, transfer optimizer (Lambert + porkchop + refinement *that outputs maneuver nodes*), design assistant (constraint solver emitting a `MissionProfile`), and the profile feeding §3.4 confidence. Observed:

- **Ascent** ✓ — a clean 5-phase gravity-turn/`SetAttitude`+`SetThrottle` driver (`csp-ai/lib.rs:70–168`), honestly scoped ("PEG/landing/rendezvous are later milestones", `csp-ai/lib.rs:9`). But see F1 — nobody can engage it.
- **Transfer** ✗ — the *planner* (`plan_transfer`, porkchop scan, refinement, `TransferPlan` output) exists and is well-tested, but nothing turns a plan into nodes for the player or an agent: no `AutopilotProgram::Transfer`, no MCP tool (`tools_v1()` at `mcp.rs:740–856` has telemetry/vessel/mission.nodes/timewarp/session/dashboard only), no UI. `WatchTransferWindow` (`lib.rs:730–737`) takes a precomputed departure tick — and since nothing in a shipped build can call `plan_transfer`, the whole transfer automation chain is dead end-to-end for players *and* agents.
- **Design assistant** ✗ — no code anywhere generates a `MissionProfile`. The object exists as a VAB-side editable stub (`MissionProfile::default()`, terrin-ascent helper used by the reliability test at `csp-sim/src/reliability.rs:1795`); §3.4's "produced by the design assistant" half is absent.
- **`mission.*` script service** ✗ — §2.10 (`PLAN.md:764–766`) explicitly promises built-in autopilots "exposed to scripts as callable services (`mission.ascend(target_apo, incl)`)". `CONTROL_API_SURFACE` (`csp-script/lib.rs:264–284`) has no `mission.*` verbs and no generic-issue escape — **a script cannot even engage the one autopilot that exists**.
- **`LlmAdapter`** — absent. The plan marks it "Later LLM", so this is a documented deferral, not a defect — noted for completeness.
- **Seam limitation:** `World::set_autopilot_driver` accepts exactly one `Box<dyn AutopilotDriver>` (`lib.rs:2504`), `AutopilotDriver::program()` returns exactly one program, and `run_autopilot_tick` skips discriminant mismatches (`lib.rs:7252–7257`). The seam as built supports one driver flying one program; a Transfer program requires either a multiplexer inside one driver or a driver map — neither exists.

**Impact:** The PRD sells an AI layer; shipped is one launch assistant that agents can reach and players can't. Interplanetary automation (Duva/Rask exist in content) has no assist at all.

**Suggestion:** Add `AutopilotProgram::Transfer` + a driver (or a `Vec<Box<dyn AutopilotDriver>>` seam) that runs `plan_transfer` → `SetManeuver` and rides `wait_node`; expose `mission.ascend`/`mission.transfer` as ControlApi verbs (a schema-major event — see F7); wire the design assistant as a pure function producing `MissionProfile` for the VAB; give MCP a `mission.plan_transfer` tool so the existing planner is at least *reachable*.

---

### F3 — [HIGH] Scripts/autopilots loaded onto non-bubble vessels are silently dead — §2.10's canonical wake scenario has no bootstrap

**Where:** `csp-sim/src/lib.rs:7286–7320` (`apply_load_script`), `:7093–7100` (`control_tick` `in_bubble` filter), `:7365–7391` (`fire_script_wake`), `:8578–8615` (`resolve_wake` residence gates), `:4999–5028` (`EngageAutopilot` apply), `:7102–7109` (autopilot `in_bubble` filter).

**Problem:** `on_tick` runs only (a) every tick while the bound vessel is `InBubble`, or (b) when a *registered* `ScriptWake` fires. But wakes register only from inside `on_tick` (`request_wake` requires `bound_script`). `apply_load_script` validates vessel existence and schedules nothing. Therefore:

- `LoadScript` on an `OnRails` or `SurfaceLatched` vessel applies "successfully" and the record **never executes once** — a zombie script. The scenario §2.10 motivates the whole wake design with ("wait until Ap, then burn" on a parked orbiter) cannot be started on a parked orbiter.
- The apsis/SOI/node wake conditions are **rails-only**: `AtApoapsis`/`AtPeriapsis` need `v.orbit()` (Some only for OnRails, `lib.rs:377–382`); `AtNextSoiTransition` needs a pending SOI event (bubble vessels never have one); `AtManeuverNode` needs a `Scheduled` node and `SetManeuver` refuses non-rails. In-bubble, only `wait_ticks`/`wait_until_tick` resolve. The only working bootstrap is: load while `InBubble`, re-arm `wait_ticks(1)` *every tick*, and hope the vessel transitions to rails before a gap — an undocumented dance to achieve the thing the feature exists for.
- Same shape for the autopilot: `EngageAutopilot` checks existence + driver match only — no residence gate — but `run_autopilot_tick` is `in_bubble`-filtered. Engaged on a `SurfaceLatched` pad craft it is a *deadlock*: the autopilot can never run a tick to issue the `SetThrottle` that `lib.rs:580–581` says would unlatch the vessel.

**Impact:** A silently-accepted dead record is the worst failure class the codebase's own docs rail against ("rejected on the originating channel … never a silent no-op" is the `SimCommand` contract). Bench/MCP callers get a successful apply and nothing happens, forever.

**Suggestion:** Either (a) reject `LoadScript`/`EngageAutopilot` for non-`InBubble` vessels (`Recoverable`, honest), or (b) better, give `LoadScript` a first-tick semantics: schedule an initial `ScriptWake` at `clock+1` so a rails-loaded script runs `on_tick` once and can arm its real wake. For autopilots, either reject non-bubble or let the first tick run to issue the unlatching throttle. Also let `wait_apoapsis`/`wait_periapsis` resolve from the *osculating* conic of a bubble vessel when it is coasting (`throttle == 0`) — the closed form is already computed for telemetry (`osculating`, `lib.rs:8529–8558`).

---

### F4 — [MEDIUM] `ControlApi` coverage is short of §2.10's promised verb list; several promised verbs can't exist because the sim has no command for them

**Where:** `csp-script/src/lib.rs:84–118` (trait), `:264–284` (`CONTROL_API_SURFACE`), `:739–910` (guest bindings); `csp-sim/src/scripts.rs:328–382` (`VesselTelemetry`).

**§2.10's list (PLAN.md:722–724): "telemetry read, throttle/steer/SAS, staging, maneuver nodes, action groups, resource transfer, part actions, alarms, log/print."**

| Promised | Shipped | Notes |
|---|---|---|
| telemetry | ✓ | `telemetry`/`telemetry_of` on the trait; `ctx.vessel` — **but `telemetry_of` has no guest binding** (exempted as "surfaces through ctx" in `api_freeze.rs:135`, yet ctx carries only the bound vessel). No vessel enumeration exists at all. |
| throttle | ✓ | `set_throttle` |
| steer | ✓ | `steer`/`steer_quat` — world-frame dir + explicit quat |
| SAS | ~ | `hold_attitude` only — "hold current". No prograde/retrograde/normal/radial/maneuver/target-point modes a kOS-equivalent implies. |
| staging | ~ | `stage(node)` and `jettison(node)` are *the same* `JettisonPart` command (`lib.rs:816–834`). No "fire next stage" verb; stage sequencing is view-side (`StagePlan`, `csp-view/lib.rs:2209`). And it is **unaimable**: `VesselTelemetry` carries no part graph or stage list, so the guest cannot learn a valid `PartNodeId` — it must guess integers. |
| maneuver nodes | ✓ | `add_node`/`delete_node`/`wait_node`; node state in telemetry |
| action groups | ✗ | **No sim concept at all** — no `SimCommand`, no `KeyAction`. Not a binding gap; the feature doesn't exist. |
| resource transfer | ~ | `transfer(to, res, amt)` exists but is unaimable twice over: no way to discover a target `VesselId`, and `VesselTelemetry` has **no resource/tank fields** — a script can't read its own fuel, let alone plan a transfer. |
| part actions | ~ | `jettison` only. No `SetAeroConfig`, `Undock`, `RunExperiment`, `TransmitData`, `RecoverData` — a script cannot perform science, undock, or reconfigure geometry: the game's core progression loop is unreachable from the "first-class" automation layer. |
| alarms | ~ | `add_alarm` only; no cancel/list. And a fired `PlayerAlarm` is never displayed — the app reads `fired_events` only for the Clamping demote (`csp-app/lib.rs:931–934`); the label the script set is invisible. |
| log/print | ✓ mechanism | …but see F5: the output has no consumer. |

**Impact:** Even after a UI lands (F1), the frozen surface can't express several PRD-promised automations, and two shipped verbs (`stage`, `transfer`) can't be used correctly without telemetry the guest doesn't get.

**Suggestion:** Add to telemetry: per-part graph listing (node id → part def/stage), resource levels, vessel roster. Add verbs for action groups (once the concept exists), `set_aero_config`, `undock`, `run_experiment`/`transmit_data`, `telemetry_of`/`vessels`, alarm cancel/list. All are additive — but the freeze means each is a declared major-version event, so batch them.

---

### F5 — [MEDIUM] §2.10's "visible" promises have no consumer: `script_log` is swallowed, strike-disable and poll-demotion reach only stderr

**Where:** `csp-sim/src/lib.rs:1299` (`AdvanceReport::script_log`), `:7177–7215` (strike/poll log lines + `disabled_reason`), `:3388–3394` (`rails_warp_refusal` names the script); `csp-app/src/lib.rs:944–959,976–977` (rejected/degraded/refusal → `log::warn!` only); consumer grep: only `crates/csp-bench/src/agent.rs:455` drains `script_log`.

**Problem:** The plan requires "disables the script, visibly, with the reason in the UI" and "demotes warp to ×1 with a visible warning naming the script". The sim honestly produces all three channels — `disabled_reason` state, `script_log` lines, the refusal string — and **none of them reach the screen**:

- `AdvanceReport::script_log` has zero readers in `csp-view`, `csp-app`, `csp-ui`, `csp-server`. `log()`/`print()` output vanishes; even the strike/poll notices the sim itself writes there vanish.
- `degraded` events and warp-refusal strings are `log::warn!`ed to stderr — invisible in the shipped GUI.
- `ProgramMirror` carries no script table, so there is nothing for a panel to show even if one existed.

**Impact:** The visibility half of §2.10's fault model is unimplemented at the presentation layer. If an agent-loaded script dies mid-flight, the player sees only that warp stays at ×1 with no on-screen reason.

**Suggestion:** Drain `report.script_log` in `csp-app` into the flight notice channel (the `COMMAND REFUSED` precedent at `csp-app/lib.rs:950–955` is the exact shape), mirror `disabled_reason`/`strikes`/`poll_demoted` per script into `ProgramMirror`, and surface `rails_warp_refusal` strings as persistent HUD notices, not just `log::warn!`.

---

### F6 — [MEDIUM] Wake-condition coverage is short of §2.10's accepted list; in-bubble `wait_*` is semantically vacuous; benign waits accrue strikes

**Where:** `csp-sim/src/scripts.rs:294–314` (`WakeCondition` — six variants); `csp-sim/src/lib.rs:8572–8577` (`InTicks(0)`/`AtTick(past)` refused), `:1074–1085` + `:7169–7195` (refused wake → runtime fault → strike); `:7388–7390` (in-bubble wake firing consumed without running `on_tick`).

**Problem:**

- §2.10 lists as accepted closed-form conditions: "absolute tick, time-to-Ap/Pe, SOI entry, node start, **altitude crossing on rails**". `WakeCondition` has no altitude-crossing variant — the promised condition is absent.
- For an `InBubble` vessel, `wait_ticks(n)` does **not** suspend the script: the control pass ticks every enabled in-bubble script every tick (7093–7100); the armed wake, when it fires, is consumed without running (7388–7390). Documented at `scripts.rs:43–46`, but the practical meaning — "wait" only matters on rails, and you can't *get* to rails-armed state without already running — deserves a louder contract, or better, `wait_ticks(n)` should suppress subsequent in-bubble ticks until `n` elapses so the verb means something in both regimes.
- `wait_ticks(0)` clamps `INT→u64` to 0 then `resolve_wake` refuses `n == 0`; a refused wake is reported as `ScriptFault::Runtime` → a **strike** (five strikes disable). A harmless edge call costs a strike and can kill the script.

**Suggestion:** Add `AtAltitude { above_m, ascending }` resolved against the rails conic (true-anomaly solve — same closed-form machinery as apsides); make refused wake registration a non-strike diagnostic (or pre-validate `n > 0`/`t > clock` guest-side); consider having an armed wake actually suspend in-bubble ticks to make `wait_*` semantics uniform.

---

### F7 — [LOW] Freeze mechanics are sound, but they enshrine an incomplete surface; `prepare` doesn't check `on_tick` arity

**Where:** `crates/csp-script/tests/api_freeze.rs` (whole file); `csp-script/src/lib.rs:936–946` (`prepare`).

**Problem:**

- `api_freeze.rs` pins 13 trait methods + 19 guest names by *reading the source* — drift-proof and genuinely good. But `frozen_surface_covers_the_trait_verbs` (api_freeze.rs:131–168) only checks trait→guest coverage; nothing checks the frozen set against §2.10's promised list. The missing verbs (action groups, `telemetry_of`, science, aero config, `mission.*`) were frozen *absent*: every gap in F4 is now a declared `csp.mcp.tools` major-version event (`lib.rs:78–83`).
- `prepare` asserts `on_tick` exists by name but not arity (`lib.rs:941`): `fn on_tick()` compiles, then `call_fn("on_tick", (ctx,))` fails `FunctionNotFound` every tick → strikes → disable. The entry-point check should verify `params == 1`.
- Cosmetic: `stage` and `jettison` register the identical `JettisonPart` command — two frozen names for one verb.

**Suggestion:** Add a PRD-coverage checklist to the freeze test (a `// §2.10:` mapping comment the test can grep, or a companion test asserting each promised verb category maps to a surface name); check `f.params == 1` in `prepare`.

---

## What's right (no action)

- **Atomic-tick drain** (`lib.rs:1048–1110`): all fallible steps (persist conversion, wake resolution) precede all mutations — a fault lands zero effects. Regression-tested (`refused_wake_drains_no_effects`, `:1520–1569`).
- **Preprocessor** (`:301–481`): token-aware — comments, string/char/raw literals skipped, `` `${…}` `` interpolations scanned as code, member calls exempt, `MAX_TEMPLATE_DEPTH` caps recursion against host-stack overflow with a regression test. Far above typical substring-grep quality.
- **Determinism pins are real, not conventional**: workspace pins `rhai = "=1.27.0", default-features = false, features = ["no_std"]` (`Cargo.toml:45`); `BasicTimePackage` never registers so wall clock is a build fact (tested `:1429–1449`); every guest transcendental is shadowed to libm (`:636–676`, tested bit-for-bit `:1454–1482`); `set_max_string/array/map_size` close the 2ⁿ-alloc OOM hole (`:613–617`, tested `:1379–1407`).
- **Strike/poll bookkeeping** is complete sim-side and fully hashed/serialized (`scripts.rs:197–232`, `539–567`); the poll detector's ≥9/10-budget near-exhaust rule honestly implements "budget-exhausting for 10 consecutive ticks" (`scripts.rs:59–69`, `lib.rs:7198–7220`).
- **Command stamping** is binding-derived, not caller-trusted — a guest can't stamp `Ui` to bypass the lease (`lib.rs:8764–8779`); `source_id` clamp semantics are consistent end-to-end.
- Wake/event pairing invariant (`wake_tick` ↔ pending `ScriptWake`) is maintained on registration, cancellation, unload, and firing — including the subtle "fires but vessel is in-bubble → consume, don't double-tick" case.

## Cross-cluster hand-offs

- Alarm UI absence (`SetAlarm`/`PlayerAlarm`/`WatchTransferWindow` unreachable): shared with **PRD-14/15**.
- `ProgramMirror`/`UiOutbox` coverage: shared with **PRD-16** (the glue where the fix lands).
- `sim.command` exposure of the whole hidden surface to agents: shared with **PRD-19**.
- `MissionProfile` consumption (§3.4 confidence) vs. its missing producer: shared with **PRD-06/08** reliability context.
