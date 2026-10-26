# PRD Review — Cluster PRD-15: egui panels — HUD, navball, program window, settings, tutorial

**Files reviewed (read in full):**
- `crates/csp-ui/src/lib.rs` (1090 — flight HUD, notice overlay, crash dialog, map overlay, help panel, maneuver panel)
- `crates/csp-ui/src/program.rs` (505 — `ProgramInput`/`ProgramCommand`, status strip, Program window)
- `crates/csp-ui/src/settings.rs` (1024 — `Settings` model, `KeyAction` vocabulary, settings panel)
- `crates/csp-ui/src/tutorial.rs` (545 — first-flight checklist model + overlay)
- `crates/csp-ui/src/navball.rs` (542 — attitude instrument + heading tape)

**Cross-checked against:** `PLAN.md` (§2.10–2.11 automation, §3.2 science, §3.3 tech, §3.4 reliability/revert, §3.5–3.5.1 economy/crew/colonies, §3.6 contracts, §4 mod-table "Kerbal Alarm Clock — v1 (M5)", §7 save/load, M9b/M10 exit rows); `crates/csp-view/src/lib.rs` (`stage_program`/`stage_program_vessel`/`stage_program_overview` L2485–3003, `program_command` L692–731, `build_ui` L540–633, `ViewMode` L85–93); `crates/csp-view/src/demo.rs` (starter craft L38); `crates/csp-app/src/lib.rs` (`apply_startup_difficulty` L549–573, rejected-command → `flight_notice` L944–955); `crates/csp-sim/src/research.rs` (`apply_run_experiment`/`apply_transmit_data`/`apply_recover_data` L670–919); `crates/csp-sim/src/reliability.rs` (`NotificationKind` L383–397); `data/{experiments/exp.materials_study.ron, parts/csp.science.canister.ron, techtree/home.ron, systems/home/sites.ron}`; `crates/csp-ui/src/vab.rs` (`top_bar` L1912–1989, `part_blurb` L2252–2286); sibling reports `review-report-06.md`, `review-report-16.md`.

## The progression-surface inventory (the review's core evidence)

`program.rs` is the **only** progression surface. Its whole vocabulary is the 6-variant `ProgramCommand` (program.rs:161–195): `ResearchTech`, `ResearchTrack`, `AcceptContract`, `RunExperiment`, `Transmit`, `Recover`. `ProgramInput` (program.rs:123–154) is the entire staged mirror: funds, science, offers, active, tech, tracks, vessel context, experiments, pooled data, `latched`, reliability rows, notices.

| PRD surface | Panel status | Evidence |
|---|---|---|
| Funds/science/tech/contracts/experiments/reliability | **Present** | program.rs whole file |
| Colony panel (state, stores, crew, found/supply/inspect) | **Absent — no field, no map marker** | `ProgramInput` has zero colony fields; `MapLabelKind` (lib.rs:192–205) has no `Colony` variant; the authored Rask colony is invisible |
| Crew roster / hire / board / transfer | **Absent** | no fields; `HireCrew`/`BoardCrew`/`TransferCrew`/`AssignCrewToColony`/`EvacuateColony` unreachable. `ModuleDef::CrewSeat` still blurbs "N crew seat(s)" in the VAB (vab.rs:2272) — a module the game cannot populate |
| Lab / research projects (`StartResearch`/`StopResearch`) | **Absent** | no fields; `ModuleDef::Lab` blurb "science lab" (vab.rs:2278) is dead schema text; `facility.lab` unlock has no surface or effect |
| Save / load game | **Absent** | no `KeyAction`, no outbox slot, no button — codec is bug-report/MCP only |
| Alarm panel + transfer-window watch (`SetAlarm`/`WatchTransferWindow`) | **Absent** | PLAN §4 L1358 lists this as **v1 (M5)**; no fields, no map panel |
| Script console (`LoadScript`/`UnloadScript`/`SetScriptEnabled`) | **Absent** | §2.10 "first-class" automation is MCP/bench-only |
| Autopilot (`EngageAutopilot`/`DisengageAutopilot`) | **Absent** | installed `AscentAutopilot` cannot be engaged by a player |
| Manufacture / build order / site inventories | **Absent** | `ManufactureCraft` unreachable; launch bypasses it entirely (PRD-16 F3) |
| Vessel recovery for credit (`RecoverVessel` + sites) | **Absent** | `terrin-downrange` recovery site (sites.ron) is dead content; `MapLabelKind::Site` renders it but it does nothing |
| Revert-to-launch | **Absent** | explicit PRD requirement — see Finding 5 |
| KSC / space-center mode | **Absent** | `ViewMode` = Flight/Map/Vab only (csp-view lib.rs:85–93); `facility.launchpad`/`facility.vab`/`facility.test_stand` unlocks are invisible state |
| Bench test / mission confidence | **Absent** | `RunBenchTest` unreachable; no `MissionProfile` confidence readout (M9b exit row requires one) |
| Facilities view (dormancy/upkeep) | **Absent** | `FacilityDormant`/`StaffAttrition` are designed events with no display |
| Difficulty / settings | **Present, half-honest** | settings.rs — see Finding 11 |

Net: the panel layer exposes ~6 of the game's ~45+ verbs and **zero** read-only mirrors of the RTS layer — a colony the sim runs every tick cannot be *seen*, let alone operated.

## Findings

---

### [HIGH] Finding 1: Nothing in the game teaches the science loop — the tutorial stops at orbit and no surface points onward

**File:** `crates/csp-ui/src/tutorial.rs:70–108` (`STEPS`), `tutorial.rs:300–316` (finish card), `crates/csp-ui/src/settings.rs:190–237` (`KeyAction` — no `OpenProgram`), `crates/csp-ui/src/vab.rs:1982` (the only door), `crates/csp-view/src/demo.rs:38` (starter craft), `crates/csp-ui/src/program.rs:212–226` (status strip)

**Problem:** The only instructional content in the shipped game is the 7-step first-flight checklist: VAB → assemble → launch → throttle → climb → space → orbit. It never mentions science, experiments, contracts, research, or the Program window. The completion card reads *"Orbit achieved — your first mission flew."* with Dismiss/Restart — no "what's next". The Program window itself is reachable only by clicking "Program" in the top bar: there is **no `KeyAction`** for it (the 20-action enum ends at `ToggleDebug`), so no keybinding, no help-panel entry, no tutorial step ever names it. The flight HUD (`FlightHudInput`, lib.rs:399–487) carries no science affordance — a player in flight sees no indication that their craft might carry an instrument or that data can be run. The always-on status strip prints `₣ {funds}   ⚛ {science}   ✉ {contracts}` (program.rs:221) with `interactable(false)` — no labels, no tooltips; a new player has no way to learn that `⚛` is science or `✉` is active contracts.

**Evidence:** The seeded starter craft `STARTER_CRAFT = ["csp.pod.mk1", "csp.tank.s160", "csp.engine.sparc"]` (demo.rs:38) carries **no experiment** — the tutorial-guided first vessel physically cannot do science, and the Program window's SCIENCE section will read "no experiment modules aboard" (program.rs:417). The *only* in-fiction hint that a canister exists is the VAB palette blurb "experiment · 10 data" (vab.rs:2273–2275) on `csp.science.canister` — one line in a part description, with nothing saying where or how it runs. `canister.ron` itself comments "not airborne, it needs to sit still" — information the player never sees.

**Impact:** This is the user's "how do I do science" made structural. Even a player who finds the Program window must guess that (a) a canister must be added in the VAB, (b) it runs from a Program-window button during flight, (c) run ≠ bank — a second transmit/recover step exists. KSP teaches this via contracts and the R&D building; here nothing does.

**Suggestion:** Extend `STEPS` past orbit with a second checklist tier ("first science"): attach a canister in the VAB → launch → open Program → run → transmit/recover → research a node. Add an `OpenProgram` `KeyAction` + default binding and a labeled (or hoverable) status strip. Cheap interim: seed the starter craft with a canister and have the finish card print "open Program [key] — your canister is waiting".

---

### [HIGH] Finding 2: The science panel is a trap — the only *working* loop is pad-camping, and the orbital loop is an enabled-but-dead button

**File:** `crates/csp-ui/src/program.rs:400–414` (enabled "run"), `crates/csp-view/src/lib.rs:2603–2607 + 2540 + 711–719` (the drop), `data/parts/csp.science.canister.ron` (`situations: 49`), `crates/csp-sim/src/research.rs:726–728, 883–917` (key close)

**Problem:** This overlaps PRD-16 Finding 1 but belongs in this cluster's discoverability story because the *panel* is where the lie lands. `stage_program_vessel` stages `ctx_biome` only for surface-side situations (csp-view lib.rs:2603–2607), so `science_ctx` is `None` in `SpaceLow`/`SpaceHigh`. The panel's own `runnable` flag still evaluates true in orbit — situation mask 49 includes SpaceLow|SpaceHigh and a (terrin, Space*) cap is authored — so the "run" button renders enabled and **clicking it produces nothing**: `program_command` returns `None`, the command never reaches the outbox, no refusal notice fires. Meanwhile the contexts that *do* work are perverse: `Landed` is in the mask, and a pad-spawned vessel is `SurfaceLatched`/`Landed` on a biome-mapped body — so "add canister → LAUNCH → sit on the pad → Program → run → recover +10⚛" is the reliable path. `Splashed` is *not* in the canister mask, so splashdown science doesn't exist. And recovery **closes the key program-wide** (research.rs:883–917): once you recover `(terrin, landed, pad-biome)`, all future Landed data at that biome is dead — repeated pad farming is bounded by the 30-unit soft cap anyway (research.rs:734–743).

**Evidence:** Button enabled in orbit + silent drop: csp-view lib.rs:2640–2661 computes `runnable` from situation mask + cap, while the command path needs `mirror.science_ctx.clone()?` (lib.rs:712). Pad-recovery legality: `apply_recover_data` requires only `SurfaceLatched` (research.rs:855–857) — a never-launched pad vessel qualifies.

**Impact:** The intended loop (fly to space, run, bank) is broken; the discoverable loop (run on the pad, recover on the pad, never fly anywhere) works and banks the *full* yield — the degenerate strategy is also the only reliable one. Combined with Finding 3, the practical ceiling is ~30 data → ~30⚛ at Terrin/Landed, enough for `tech.science.1` (15) and part of `rocketry.2` (30) — then all Landed science at that biome is closed forever.

**Suggestion:** Fix `science_ctx` staging (PRD-16 F1's fix — stage on situation, empty `BiomeId` for space). Until then, gate the button on `science_ctx.is_some()` so the panel never shows a dead verb. Separately, disclose key closure in the pooled row ("recovery closes this context") since the close is permanent and non-obvious.

---

### [HIGH] Finding 3: Transmit is ElectricCharge-gated with zero disclosure — the stock path can never transmit

**File:** `crates/csp-sim/src/research.rs:797–803` (EC debit), `crates/csp-ui/src/program.rs:78–93` (`DataRow` — no EC fields), `crates/csp-view/src/demo.rs:38` (no battery in starter craft), `crates/csp-app/src/lib.rs:944–955` (the refusal does surface)

**Problem:** `apply_transmit_data` debits `units · transmit_ec_per_data_unit` `ElectricCharge` and refuses "insufficient ElectricCharge" — but `DataRow` carries no EC cost or vessel-charge field, the panel shows no vessel EC gauge, and neither the canister blurb nor anything else mentions a charge requirement. The stock craft (pod/tank/engine, no `csp.battery.z1k`) has **zero EC aboard**, so "transmit +4.0⚛" on the out-of-the-box path is doomed on every press. To the game's credit the refusal is *not* silent — the rejected-command sweep pushes "COMMAND REFUSED — TRANSMIT: INSUFFICIENT ELECTRICCHARGE" onto the global cue channel (csp-app:944–955) — but the player only learns the requirement after failing, and the recovery alternative is the same pad-recovery the game already pushes them toward.

**Evidence:** research.rs:797–803; `DataRow` fields at program.rs:78–93 (units/yields/transmitted — no EC); transmit button always enabled at program.rs:426–431.

**Impact:** A hidden resource precondition makes the cheaper banking path look broken rather than gated. Players will click transmit, see a cryptic refusal, and either bounce off or default to recover-on-pad (Finding 2's degenerate loop). It also silently narrows design space: survey contracts that require `DataBanked` still trigger on either path, but the battery gate is invisible.

**Suggestion:** Stage vessel EC + per-row cost into `ProgramInput`/`DataRow`; render the transmit button disabled with hover "needs N EC (have M)" mirroring the recover button's existing pattern. Add "requires ElectricCharge to transmit" to the experiment blurb.

---

### [HIGH] Finding 4: The contract board is accept-blind — no reward, deadline, or requirements; contract events never surface anywhere

**File:** `crates/csp-view/src/lib.rs:2939–2949` (labels), `crates/csp-ui/src/program.rs:46–52, 262–276` (`OfferRow`, accept UI), `crates/csp-sim/src/reliability.rs:383–397` (`NotificationKind` — two variants only), `crates/csp-view/src/lib.rs:2987–3002` (LOG feed)

**Problem:** An offer renders as literally `"#0 contract.survey"` — index + raw template id. `OfferRow` has no fields for reward, advance, deadline, parameters, or description, and the staged label carries none. Active contracts render the same way — no progress state, no expiry countdown. The player must accept a contract having seen only its identifier. Worse, the LOG section is fed by `sim.notifications()`, whose kind enum has exactly two variants — `PartFailed` and `IgnitionMisfire` — so **contract acceptance, completion, failure, and deadline events produce no player-visible record at all**; the only signals are the `✉` counter in the status strip and the funds number silently changing. (§2.6.5's table makes `ContractDeadline` a `Clamping` event that auto-demotes warp — so a deadline the player can neither see nor plan around can still yank their warp.)

**Evidence:** stage labels at csp-view lib.rs:2942, 2948; `NotificationKind` at reliability.rs:385–397 — no contract variant exists to mirror; `input.notices` feeds only from it.

**Impact:** Accepting contracts is a blind bet; contract lifecycle is invisible. The one progression surface that could create pull ("take contract → learn the science loop") instead teaches nothing — an accepted `contract.survey` never tells the player it's satisfied by banking science data.

**Suggestion:** Extend `OfferRow`/active rows with the template's reward, deadline tick, and a one-line description (loc keys already exist in contract data); add a generic event feed — contract transitions, science banked, funds deltas — into `NoticeRow` (the sim already publishes these as events; only the mirror enum is narrow).

---

### [HIGH] Finding 5: Revert-to-launch — an explicit PRD player-facing feature — has no surface at all

**File:** `PLAN.md:1180–1184` ("Revert-to-launch **exists** and restores the pre-launch snapshot — it is one of the top-three QoL features of the genre… The UI says so explicitly at the revert prompt"), `PLAN.md:1506` (M9b exit row: "revert policy in the UI"), `crates/csp-ui/src/settings.rs:190–237` (no `KeyAction`), `crates/csp-view/src/lib.rs` (no outbox slot / verb)

**Problem:** No revert verb exists anywhere in the panel layer: no keybinding, no top-bar button, no VAB action, no outbox field. The crash/loss path is "vessel lost — build or load a craft and LAUNCH again" (tutorial.rs:339) — full manual rebuild/relaunch, and since the pre-drawn θ design makes revert load-bearing (PLAN states the UI must *disclose* that revert keeps pre-drawn thresholds), its absence also removes the designed explanation point.

**Evidence:** `KeyAction::ALL` (settings.rs:241–262) — 20 actions, none revert-adjacent; `UiOutbox` fields (csp-view lib.rs:311+ via build_ui 620–632) — no revert slot.

**Impact:** Every misfire/dormant failure/crash costs a full build+launch cycle; the reliability system's "retry the flying, not the dice" promise is undeliverable. This is a PRD exit-row miss, not a scope cut.

**Suggestion:** Session-level revert: capture the pre-launch snapshot in `csp-view` at `launch_craft`, add a `Revert` `KeyAction` + top-bar button with the PRD's mandated disclosure line ("revert is not a reroll").

---

### [HIGH] Finding 6: Save/load has no player surface — and "Difficulty (applies to new games)" names a verb that doesn't exist

**File:** `crates/csp-ui/src/settings.rs:787–798` (difficulty section), `crates/csp-app/src/lib.rs:549–573` (`apply_startup_difficulty` — process-start only), PLAN §7

**Problem:** Two coupled gaps. (a) The §7 save codec is complete and tested, but the only persistence a player can reach is craft files — there is no save-game/load-game verb anywhere (no `KeyAction`, no button, no outbox slot; bug-report bundles and MCP `session.save`/`session.load` are the only writers). (b) The settings panel shows "Difficulty (applies to new games)" with `life_support_enabled`/`dormant_stress` checkboxes — but **there is no new-game verb in the application**: the flags are applied once at process start (`apply_startup_difficulty`), so editing them does nothing until the process is restarted. The label is honest about *when* they bind but the section implies an action (start a new game) that cannot be performed from inside the game.

**Evidence:** settings.rs:787–798 (the section), csp-app lib.rs:560–573 (start-only application); no `NewGame`/`SaveGame`/`LoadGame` in `KeyAction`, `UiOutbox`, or `View`'s method vocabulary.

**Impact:** A crashed or finished session can only be continued by restarting the process — every session is effectively ironman-by-accident, which interacts badly with Finding 5 (no revert). The difficulty checkboxes create a "did it apply?" doubt the UI never resolves.

**Suggestion:** Minimum: session save/load buttons (the codec path already exists for bug-report bundles). Add a "restart session" so "new games" is a real noun; annotate the difficulty section "takes effect on next session start" if a restart verb isn't shipped.

---

### [MED] Finding 7: Alarms and the transfer-window planner — a v1/M5 PRD row — have no panel, key, or mirror

**File:** `PLAN.md:1358` ("Kerbal Alarm Clock | Alarms + transfer-window planner … | **v1 (M5)**"), `PLAN.md:535–544` (`PlayerAlarm`/`ContractDeadline` are `Clamping` warp-demoting events), `crates/csp-ui/src/lib.rs:1027–1090` (`maneuver_panel` — no alarm/watch affordance), `crates/csp-ui/src/program.rs:123–154` (no alarm rows)

**Problem:** `SetAlarm` and `WatchTransferWindow` exist in the sim and are fully unreachable. The map's maneuver panel covers node placement only — no "watch next window to rask/duva", no alarm list, no countdown display. `maneuver_panel` additionally returns early when its (on-rails-only) vessel list is empty (lib.rs:1035–1037) — it simply isn't drawn, so a player can't even discover that maneuver nodes exist; with no reachable `OnRails` path (PRD-16 F2) the panel is invisible in practice. The result: the two PRD-specified tools for *waiting for the right moment* are absent, while `ContractDeadline`/`PlayerAlarm` clamping events still exist in the design as things that can demote warp on the player.

**Impact:** Interplanetary play (Duva/Rask are authored) has no timing tool; transfer windows are computable by the sim (`WatchTransferWindow` → transfer.rs) and invisible to the player. Deadlines can still fire `Clamping` demotions the player never saw coming.

**Suggestion:** An ALARMS section in the Program window (list + set/clear) and a "next window" readout in the maneuver panel are the smallest honest surfaces; even read-only alarm display would stop silent warp demotions being mysterious.

---

### [MED] Finding 8: Scripting and autopilot — PRD's "first-class" automation — have no console or toggle

**File:** `crates/csp-ui/src/` (zero matches for script/autopilot across the crate), PLAN §2.10–2.11

**Problem:** `RhaiHost` and `AscentAutopilot` are installed at startup, and the sim honors `LoadScript`/`UnloadScript`/`SetScriptEnabled`/`EngageAutopilot`/`DisengageAutopilot` — none of which any panel, key, or outbox slot can emit. There is no script editor, no console, no autopilot engage button. The only way to touch the automation layer is MCP or bench — the agent-facing surface again out-prioritizes the player's.

**Impact:** §2.10's promised player scripting is a dead feature in the shipped game; the ascent autopilot — which would paper over the game's hardest flight-skill gap for new players — cannot be engaged.

**Suggestion:** A minimal script console (text field + load/unload + log tail) and an "engage ascent autopilot" toggle on the Program window or flight HUD would expose what's already installed.

---

### [MED] Finding 9: The reliability section is read-only — no bench test, no qualification display, no mission confidence

**File:** `crates/csp-ui/src/program.rs:96–121, 453–497` (`ReliabilityRow`, RELIABILITY/LOG sections), PLAN.md:1506 ("test-stand facility, part-test contracts, real `MissionProfile` confidence UI")

**Problem:** The panel displays per-part failures, misfire, thrust factor, wear %, and dormant flag — genuinely useful — but offers **zero verbs**: `RunBenchTest` is unreachable, `facility.test_stand` (a tech unlock) does nothing, and the PRD-mandated `MissionProfile` confidence number (`exp(−Λ_expected)`) appears nowhere — not in the VAB readout, not pre-launch, not in the panel. Qualification progress (the curve the canister's own reliability def carries) isn't displayed either; wear% is shown but "how do I make it not fail" has no answer in the UI.

**Impact:** Reliability is a system the player can only suffer, never manage — the M9b exit row's player-facing half is missing. Part-test contracts (a shipped template, `contract.part_test.ron`) reference a mechanic with no reachable verb.

**Suggestion:** Add a bench-test button per reliability row (gated on `facility.test_stand` unlock — which would then do something) and a launch-readiness/confidence readout in the VAB or Program overview.

---

### [MED] Finding 10: The tech tree never says what a node buys

**File:** `crates/csp-ui/src/program.rs:11–24` (`TechRow` fields), `crates/csp-view/src/lib.rs:2951–2963` (staging), `data/techtree/home.ron` (`description`, `unlocks` fields never read), `crates/csp-ui/src/vab.rs:2244–2247` (palette "not researched" tag)

**Problem:** A tech row is `name + cost + ✓/○` — that's all the mirror carries. Every node's authored `description` loc key and `unlocks` block (facilities, contract types, track-level caps; parts indirectly via `PartDef::tech_requirement`) is dropped at staging. The VAB does dim unresearched parts with a "not researched" tag (vab.rs:2244–2247) — so the *part* side knows it's gated — but nothing links "research rocketry.2" to "unlocks tank.s320 + engine.ember". The player is asked to spend the game's scarcest currency on a name.

**Impact:** Purchase decisions are uninformed; the tech tree can't function as a progression *teaser* (its main UX job in the genre). Combined with Finding 4, both spend-side and earn-side surfaces under-inform.

**Suggestion:** Extend `TechRow` with a resolved description + unlock summary line (loc-resolved part names for `tech_requirement`-gated parts); at minimum a hover tooltip.

---

### [MED] Finding 11: Difficulty honesty — the "life support" checkbox enables a subsystem that can never act; both flags are effectively dead mid-session

**File:** `crates/csp-ui/src/settings.rs:133–156` (`DifficultySettings`), `settings.rs:787–798` (the section), `crates/csp-app/src/lib.rs:549–573`

**Problem:** Two issues. (a) `life_support_enabled: true` enqueues `SetLifeSupportEnabled` — into a subsystem that consumes crew resources, while **no crew can ever exist** (`HireCrew`/`BoardCrew` unreachable; Finding 4's absent crew section). The checkbox promises a survival mechanic with zero observable effect — unlike `GraphicsQuality`, which was deliberately *hidden* because "a lying control is worse than an absent one" (settings.rs:88–94), this lying control ships. (b) Per Finding 6, neither flag can take effect on any session the player can create — they're process-start writes with no new-game verb. Separately, "dormant stress" is jargon — the only place a player could learn what it means is the reliability section's ", dormant" tag (program.rs:481), which requires a dormant-class part to exist.

**Impact:** A settings screen that over-promises; the checkbox is the only place "life support" appears in the entire game, implying a depth that isn't reachable.

**Suggestion:** Either hide `life_support_enabled` until crew exists (same rule the codebase applies to `quality`), or annotate it "no crew gameplay yet — currently has no effect". Add a one-line description under "dormant stress".

---

### [LOW] Finding 12: Program-window micro-honesty issues (batched)

**File:** `crates/csp-ui/src/program.rs`

- **Transmit never gated** (program.rs:426–431): clickable even when EC is short or the key is closed — the COMMAND REFUSED cue names the reason afterward (acceptable), but there's no forewarning. Contrast the recover button, which *is* gated + hover-explained (program.rs:432–445).
- **Recover hover under-specifies** (program.rs:438–441): "vessel must be on the surface" is the only stated precondition; the sim also refuses on data-at-key aboard another vessel and live-project escrow (research.rs:879–892) — unguessable from the panel.
- **`science` vs `banked` is a dead distinction** (program.rs:128–131, staged at csp-view lib.rs:2496–2497): `science` = banked + in-flight project accrual, but `StartResearch` is unreachable, so the two numbers are identical forever — the dual display implies a lab loop that cannot run.
- **LOG reads generic but is reliability-only** (program.rs:489–498): only two `NotificationKind` variants can ever appear (Finding 4).
- **Status strip glyphs** (program.rs:212–226): `✉` counts only *active* contracts (`input.active.len()`, csp-view lib.rs:555) — offers pending don't show; glyphs unlabeled and non-interactive.
- **"no vessel in focus"** (program.rs:368) is the SCIENCE section's empty state — it doesn't say how to focus one (launch, or focus on the map).

---

### [LOW] Finding 13: Navball is surface-frame only — no orbital mode, no node/target markers

**File:** `crates/csp-ui/src/navball.rs:24–38, 289–312`

**Problem:** `NavballInput` stages exactly one prograde (surface-relative) plus up/north/east — no orbital-prograde toggle, no maneuver-node marker, no target marker. For the game's current single-body scope this is defensible (surface-relative ≈ orbital in high orbit), and the instrument itself is well-built — the ground-cap decomposition has a real property test (navball.rs:430–474), far-hemisphere clamping and dead-astern letter suppression are handled. But once on rails around Solen or transiting to Rask, "surface" prograde is the frame the player needs least, and node markers are the genre's core affordance (a `SetManeuver` panel exists — its burn is never mirrored onto the ball).

**Suggestion:** Low priority — but when maneuver nodes become reachable (PRD-16 F2), the navball should gain orbit-frame + node-marker inputs; the input struct's extension is a one-field change.

---

### [LOW] Finding 14: "recover control of vessel" reads like vessel recovery — it's a takeover verb, and the real recovery verb is absent

**File:** `crates/csp-ui/src/settings.rs:278` (`KeyAction::RecoverControl` label)

**Problem:** The keymap label "recover control of vessel" describes the `ToBubble`+`TakeControl` takeover path — not `SimCommand::RecoverVessel` (funds/resource credit at a site), which has no surface at all (Finding 4's inventory). A player scanning Controls will read it as the recovery mechanic that doesn't exist.

**Suggestion:** Relabel "retake control of vessel" / "fly vessel again"; land the actual recovery verb separately.

---

### [INFO] Missing-by-design vs missing-by-oversight — the verdict

**Deliberate, documented, honest:**
- `GraphicsQuality` persisted but unshown — "a lying control is worse than an absent one" (settings.rs:88–94, 755–758). Model behavior.
- Advanced render tier — §6.4 cut-list item 1.
- `debug_panel` dev-gated (title still says "M0 scaffold" — cosmetic residue at lib.rs:103).
- Craft-file save/load exists (VAB) though game save/load doesn't — consistent with the session-not-campaign scope the team evidently chose.
- `maneuver_panel`'s empty-vessel early return is honest — it just also makes the feature undiscoverable.

**PRD-specified but unlanded (oversight, not cut-list):**
- Alarms + transfer planner (PLAN §4: v1 M5) — Finding 7.
- Revert-to-launch "in the UI" (M9b exit row) — Finding 5.
- `MissionProfile` confidence UI (M9b exit row) — Finding 9.
- Save/load (§7) — Finding 6.
- Scripting/autopilot "first-class" (§2.10/2.11, M9a) — Finding 8.
- The entire §3.5/§3.5.1 RTS layer (colonies, crew, logistics, manufacture, facilities) — the inventory table. The sim implements it; the panel layer doesn't even *mirror* it.
- Contract detail/alarm feed — Finding 4.
- Science discoverability — Findings 1–3: the loop exists and (on the pad) works, but every teaching surface that would name it is absent. The panel is the *last* place this should have been solved and the only place it still can be cheaply.

**Panel-layer quality notes (things that work well):** refusal cues genuinely propagate (`report.rejected` → `flight_notice`); `Recover`'s latch-gating + hover reason is the right pattern; the tutorial's real-progress predicates (`step_complete` on `TutorialInput`, tutorial.rs:141–152) are honest design worth extending; keybinding physical-token/layout-observed label machinery (settings.rs:329–509) is unusually careful; ui_masks pointer-pass ordering (csp-view lib.rs:592–599) prevents click-through correctly.
