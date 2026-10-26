# PRD-17 — App shell: input, warp, audio, settings, bugreport

**Cluster:** `crates/csp-app/src/{lib.rs 3684, audio.rs 839, warp.rs 639, settings_io.rs 760, synth.rs 383, bugreport.rs 237, logring.rs 92, main.rs 18}` — winit loop, key dispatch, §2.6.3 warp controller, audio backend, settings/craft-file IO, §2.13/§2.14 crash path, wasm entry.
**Verdict:** WIRED and unusually honest — every dispatched verb that can refuse names its refusal on the cue channel, and the warp-as-logged-commands contract (§2.6.3) is implemented faithfully. The PRD gap is at the *vocabulary* level: `KeyAction` (csp-ui/settings.rs:190–237) is 20 verbs, and the whole app+view command surface reaches ~17 of ~61 `SimCommand` variants. Save/load/revert/pause/SAS/action-groups/alarms/program-open do not exist *as concepts* anywhere in the input layer — they are unreachable by rebind, button, or outbox, not merely unbound.

## 1. What the player can actually command

Direct `SimCommand` producers reachable from csp-app + the view plumbing it calls:

| Producer | Commands |
|---|---|
| warp controller (`run_frame`, lib.rs:859–884, 971–984) | `SetWarp`, `AdvanceTo` |
| held/snap keys (lib.rs:1692–1930) | `SetAttitude`, `SetThrottle` |
| stage key → `view.fire_stage` (csp-view/lib.rs:2347) | `JettisonPart` |
| `view.launch_craft` (csp-view/lib.rs:2213) | `SpawnPhysicalVessel` (free, instant — the PRD-16 launch bypass) |
| `view.recover_control` (csp-view/lib.rs:1570, 1946) | `ToBubble`, `TakeControl` |
| outbox.maneuver → `program_command`-style mapping (csp-view/lib.rs:679–688) | `SetManeuver`, `DeleteManeuver` |
| outbox.program → `dispatch_program` (lib.rs:1200–1213) | `RunExperiment`, `TransmitData`, `RecoverData`, `ResearchTech`, `ResearchUpgradeLevel`, `AcceptContract` |
| boot difficulty (lib.rs:560–573) | `SetLifeSupportEnabled` |
| boot authored colonies (csp-view/lib.rs:1214) | `FoundColony` (not player-initiated) |

**~17/61 `SimCommand` variants are player-reachable.** The other ~44 — `SetVesselTracked`, `SetAlarm`, `WatchTransferWindow`, `Undock`, `TransferResource/Crew`, `BoardCrew`, `StartResearch`, `StopResearch`, `RecoverVessel`, `HireCrew`, `ContractReward`, all 11 colony/manufacture/logistics commands, `LoadScript`/`UnloadScript`/`SetScriptEnabled`, `EngageAutopilot`/`DisengageAutopilot`, `RunBenchTest`, `Unlatch`, `ToRails`, `SpawnVessel`, `InstallAeroTables`, `SetAeroConfig`, `JoinColonyInventory`/`LeaveColonyInventory`, `TransferColonyResource/Crew`, `AssignCrewToColony`, `EvacuateColony`, `DamageColonyModule`, `ScheduleColonyInspection`, `AbandonColony`, `ManufactureCraft` — have **zero** producers in csp-app/csp-view/csp-ui. They are reachable only through MCP `sim.command` (csp-server) and bench scripts.

Dispatch honesty is real: `outbox.program`/`outbox.maneuver` are filled only by the typed `program_command`/maneuver mapping (csp-view/lib.rs:679–727), every enqueue rides `SourceClass::Ui` with per-producer FIFOs (lib.rs:69–88), and `Ui`-class rejects surface as `COMMAND REFUSED — …` HUD cues (lib.rs:944–956). Nothing smuggles; nothing reaches the hidden progression surfaces — which is the finding: the glue that *would* reach them does not exist.

## 2. Missing player-facing verbs vs a KSP-like

Absent at the vocabulary level (no `KeyAction`, no `UiOutbox` field, no panel output — not rebindable, not latent):

| Verb class | Sim-side support | Status |
|---|---|---|
| Save / load / quicksave / new-game | `csp-sim/save.rs` codec + `snapshot()`/`restore` fully implemented | reachable only via `--bug-report` CLI (lib.rs:2484) and MCP `session.save`/`load`; **no in-game verb at all** |
| Revert-to-launch / revert-to-VAB | none (no `Revert` command exists) | **absent entirely** |
| Mission-end / vessel recovery | `RecoverVessel` (csp-sim/lib.rs:882) removes vessel, credits recovery | unreachable — flights can only end in crash or recover-*control* |
| Pause | — | no ×0 rung (`WARP_LADDER` floors at 1, warp.rs:33); no pause verb; the only clock-halt is `Fatal` |
| SAS / attitude-hold | — | `SetAttitude` is a raw body-*rate* stand-in: "until M3+ real SAS" (lib.rs:1598), "the documented M2 control stand-in" (lib.rs:1671); no SAS toggle, no navball hold modes |
| Action groups | — | none in `KeyAction`, none in `SimCommand`, absent from `ControlApi` too (PRD-18); `Stage` is the only part verb |
| Alarms (KAC-equivalent §4) | `SetAlarm`→`PlayerAlarm` Clamping (csp-sim/lib.rs:554–560), demote path wired (lib.rs:931–934) | **no producer** — `PlayerAlarm` can never fire in a real session |
| Transfer-window watch | `WatchTransferWindow`→`TransferWindow` Clamping | unreachable |
| Program window key | top-bar button only (csp-view/lib.rs:615–617) | every other floating panel (settings/help/debug) has a `KeyAction`; program has none |
| OpenProgram / contract-accept from map | `AcceptContract` reachable in program window | OK, but window itself is button-only |
| Map-time controls ("warp to node", "warp to SOI") | event machinery exists | absent |
| EVA, N-body, CommNet, VT | — | correctly post-v1 (PLAN cut list) |
| Difficulty "applies to new games" | `apply_startup_difficulty` (lib.rs:560–573) | runs **once at process boot**; the settings section (csp-ui/settings.rs:787) is honest about the binding but there is no new-game verb it can ever apply to — dead configuration |

## 3. Defects

### D1 — Medium — `AdvanceReport::partial` ignored; §2.6.5 back-pressure feedback unhooked
**Where:** `crates/csp-app/src/lib.rs:924–985` (the `Ok(report)` arm reads `fired_events`/`rejected`/`degraded` — never `report.partial` or `report.effective_target`).
**Problem:** PLAN §2.6.5: "`partial` and the warp demotion come only from exceeding the *frame* budget"; "warp becomes self-limiting instead of frame-stalling." `AdvanceReport::partial` (csp-sim/lib.rs:1282–1287) is defined as back-pressure "the caller must re-issue"; `csp-bench`'s agent loop does exactly that (`agent.rs:342` `while report.partial` re-issues `effective_target`). The app does neither: on a `partial` frame the controller's `scaled_nanos` accumulator keeps growing at the *requested* rate, the next `AdvanceTo` overshoots by the same margin, and warp never self-limits.
**Impact:** under event-budget pressure (the §2.6.5 pathological case — many colonies/objects at ×10⁵) the sim hits its 2000-event/69120-tick per-call cap every frame while the HUD keeps reporting the requested rung. Displayed warp ≠ achieved rate; no demotion, no notice — the exact overshoot-dishonesty the section exists to prevent. The wall-cost measurement cannot catch it: `last_tick_ms` = step_ms ÷ clock-ticks-advanced stays sub-µs at rails and satisfies the §2.6.3 inequality at every rung.
**Suggestion:** on `report.partial`, call `warp.demote(effective−1)` (or demote toward the largest rung whose per-call budget fits) and surface a `WARP LIMITED — EVENT BACKLOG` cue; optionally honor the "re-issue `effective_target`" contract explicitly.

### D2 — Medium — `SetVesselTracked` has no producer → `SoiTransition` is always `Silent`
**Where:** `crates/csp-sim/src/lib.rs:2419` (`tracked_vessels: BTreeSet::new()`), `:4845–4847` (the *only* mutator, inside `apply_command` for `SetVesselTracked`); `crates/csp-sim/src/events.rs:449–457` (`class_of` gates `SoiTransition`→`Clamping` on `tracked.contains(vessel)`).
**Problem:** §2.6.5's warp-clamp table says "focused/tracked vessel `SoiTransition`" auto-demotes. The demote half is correctly wired in `run_frame` (lib.rs:931–934, 971–984) — but nothing in the app, view, UI, server, or bench issues `SetVesselTracked`, so `tracked_vessels` is permanently empty and every `SoiTransition` classifies `Silent`.
**Impact:** at ×10⁵ (~18 sim-minutes/frame, §2.6.2) the player flies through SOI entries/exits with no warp clamp — silent overshoot on the primary "warp is dangerous" mechanic. `ManeuverNodeStart` is *unconditionally* Clamping (events.rs:493–494), so armed nodes still catch; unplanned encounters (the common case when transiting) do not.
**Suggestion:** issue `SetVesselTracked{vessel, tracked: true}` for the flight-focus vessel on focus acquisition (clear on change), or — cheaper and less stateful — promote the *focused* vessel's transitions in `class_of` by rule. The MCP-side `sim.command` path should not be the only way to arm the game's own overshoot protection.

### D3 — Low/Medium — manual warp steps wait the full 30-frame promotion window
**Where:** `crates/csp-app/src/warp.rs:177–223` (`decide` applies the hysteresis to `pending_step` changes; the doc at :169–171 admits "per the plan's letter it also applies to manual step-ups").
**Problem:** a `,`/`.` keypress (or panel ±) doesn't engage its rung until the margined inequality has held `PROMOTE_FRAMES` = 30 consecutive frames — ~0.5 s at 60 fps, ~1.5 s at 20 fps, per step. Stepping ×1→×10⁵ takes 6 keypresses × ~0.5 s each.
**Impact:** warp keys feel broken-laggy on first use — the "press ., nothing happens" class of complaint. PLAN §2.6.3's hysteresis is written against auto-derate *oscillation*; a manual request is player intent, and demotion-side is already immediate.
**Suggestion:** engage the largest *affordable* rung ≤ the new `requested` immediately on a manual step (the affordability check still runs), reserving the 30-frame streak for re-promotion after an auto-derate — which is what the hysteresis text actually argues for.

### D4 — Low — `throttle_held` is action-keyed; dual bindings alias on release
**Where:** `crates/csp-app/src/lib.rs:232` (`BTreeSet<KeyAction>`) vs `:220` (attitude's `BTreeMap<token, KeyAction>`); release at `:1836–1842` and `:2078–2084`.
**Problem:** the attitude held-set is keyed by the *key token* precisely so two bindings on one verb hold independently (doc :1596–1602; regression test :3345). The throttle axis kept the action-keyed set: bind W and ArrowUp both to `ThrottleUp`, hold both, release either — `throttle_held` empties and the hold dies under the still-pressed key.
**Impact:** only reachable via a user-added second binding on a throttle-axis verb (defaults bind one each) — a real but narrow correctness regression vs the attitude contract.
**Suggestion:** key `throttle_held` by held-token like `attitude_held` (the ramp derives direction from the held *actions*, which is preserved either way).

### D5 — Low — audio backend is Linux-only in practice
**Where:** `crates/csp-app/src/audio.rs:193` (`PLAYERS = ["pw-play", "paplay", "aplay"]`).
**Problem:** the subprocess-player strategy covers PipeWire/Pulse/ALSA only; on macOS and Windows every spawn fails → `open` returns `None` → silent (`log::warn` at :486, no in-game notice). The M10 "audio" exit row is satisfied on exactly one platform family.
**Impact:** honest code, narrow coverage — audio silently absent for non-Linux users; only the log line says why.
**Suggestion:** document the platform scope on the settings panel (or add `afplay`-class/`powershell`-class players); at minimum surface a one-time settings notice rather than only `log::warn`.

### D6 — Low — `dormant_stress` is an unlogged construction-config write
**Where:** `crates/csp-app/src/lib.rs:568–572` (`set_reliability_config` at boot; doc at :549–559 owns the trade-off).
**Problem:** unlike `SetLifeSupportEnabled` (a logged command), the `dormant_stress: false` path mutates construction config — serialized in the save, hashed, but absent from the §7.6 command log. Determinism "rides on the snapshot carrying it": any consumer that replays a command log *without* first restoring the snapshot diverges silently.
**Impact:** consistent within the stated contract (saves are the replay unit), but a fragile seam for command-log-only tooling.
**Suggestion:** keep as-is but flag in the replay-harness contract, or route the flag through a logged `SimCommand` variant.

## 4. Verified honest / no findings

- **Frame-loop order** matches §2.5 literally: outbox(→next-Input) → `warp.observe/decide/target_tick` → `SetWarp` before `AdvanceTo` on one FIFO → egui input → held-axis ticks → `begin_frame`(Input) → `sim_step`(one `step_frame`) → `end_frame`(Sync/Presentation/UiBuild/RenderExtract) → paint. lib.rs:819–1025 + csp-view `begin_frame`/`sim_step`/`end_frame`.
- **Warp-as-commands:** the sim never measures wall time; `SetWarp` is stored verbatim and uninterpreted (csp-sim/lib.rs:4800), `AdvanceTo` rides the §7.6 log for replay; demotion backlog is dropped via `needs_rebase` re-anchor (warp.rs:241–257, tests :568–639). The demotion-inequality, one-rung-bound promote streak, and refusal-heartbeat streak-clearing are all correct and tested.
- **Refusal surfacing:** every gated verb names its refusal on the global cue channel — `throttle_blocker`/`attitude_blocker` (lib.rs:1640–1736), read-only gates on launch/maneuver/program/warp/craft-save, VAB mode refusals. `Ui`-class drain rejects also surface (lib.rs:950–955). No silently dead key remains.
- **Held-key lifecycle:** release always dispatches regardless of egui consumption (lib.rs:2378–2384); focus loss clears every held state incl. pointer/drag/capture (lib.rs:1976–1988); rebind Apply drops holds whose bindings moved (lib.rs:1951–1968); capture swallows until release by *physical* key (lib.rs:2089–2121).
- **Typing gate:** only `Key::Character` presses with a focused TextEdit are withheld — releases and non-text keys always dispatch (lib.rs:2379–2383); rebind capture intercepts before egui (lib.rs:2302–2306).
- **Craft IO:** name validation rejects traversal/separator/control (lib.rs:2623–2637); store insert only after disk write lands (lib.rs:1086–1098); demo gate on both save and load.
- **Settings IO:** tmp+rename persist (settings_io.rs:404–428), corrupt-file salvage per-field and per-binding-row (lib.rs tests :3524–3555), backfill of verbs a file predates (:3468–3501), non-finite rejection (:3503–3522), AZERTY positional tokens + char→`key-*` migration (settings_io.rs:527–696).
- **Bug-report path:** §2.13 `Fatal` → halt + read-only + bundle + dialog (lib.rs:581–602); panic hook writes a degraded bundle without sim access (lib.rs:2467–2479, bugreport.rs:182–196); `--bug-report` CLI (main.rs:7–15); log tail tee (logring.rs). All bundle artifacts best-effort, never panicking (bugreport.rs:125–176).
- **Audio:** strictly read-only on presentation resources post-`end_frame` (audio.rs:524–601); pipe-occupancy latency bound with saturation re-anchor (audio.rs:307–379); graceful absence at every failure point; NaN guards (:559–588); synthesized bank is deterministic and attribution-free (synth.rs).
- **Pick pipeline:** id-table + craft-epoch snapshot at encode, stale readbacks resolve as background `Pick(None)` rather than mis-selecting (lib.rs:829–854, 1417–1480).

## 5. Bottom line for the parent

The input layer is *mechanically excellent and semantically starved*: every verb it has works honestly, but the verb set is 20 keys covering ~17 commands. The largest player-facing absences are **save/load/new-game** (codec done, zero UI), **revert/recover** (no revert concept at all; `RecoverVessel` unreachable), **SAS** (rate-command stand-in only), **alarms/transfer-watch** (the demote machinery exists for events nothing can schedule), **pause**, and **action groups**. Two real defects sit inside the wired machinery: **`partial` back-pressure is unhooked** (D1) and **`SetVesselTracked` has no producer, so SOI transitions never clamp warp** (D2). Together with PRD-16's `stage_program` coverage gap and PRD-15's missing panels, this file is where the "half the game is missing" verdict is most literally provable: the sim's command surface is 61 verbs wide and the app's is 17.
