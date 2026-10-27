# Review Report C22 — Program window + Game menu (session-full-20261026)

Files: `crates/csp-ui/src/program.rs` (code L1–2677, tests L2678–2859), `crates/csp-ui/src/game.rs` (198).
Tree reviewed: `ff897d4` (the "+719 uncommitted" work landed as this commit; working tree is clean of further edits to these files).

Verification performed: full read of both files; cross-read of the glue (`csp-view/src/lib.rs` `build_ui` L625–791, `program_command` L1001–1289, `stage_program*` L3575–3934/4177–4350/4604–4995, `rebuild_from_snapshot` L1574, `load_game` L3077); sim semantics (`csp-sim/src/research.rs`, `lib.rs` apply_deploy_part L7214, `transfers.rs` apply_board_crew, `economy.rs` apply_recover_vessel/apply_hire_crew, `reliability.rs` apply_bench_test); `cargo test -p csp-ui --lib` (43 passed) and `cargo clippy -p csp-ui` (clean). One hypothesis (persisted egui ids losing data on layout change) was tested against egui 0.37 and DISPROVEN — child `scope_id` is hierarchy-derived (`new_child`: `IdSource::Child` → `parent.scope_id.with(salt)`), so `automation_section`'s persisted `apo_km`/`script_source` pattern is stable. Not flagged.

---

### [HIGH] Finding 1: A loaded save keeps the previous session's revert point — "Revert to launch" silently destroys the just-loaded world
**File**: crates/csp-ui/src/game.rs:113 (manifests) — root cause crates/csp-view/src/lib.rs:1574–1602, 3077–3079, 2258
**Problem**: `View::load_game` → `rebuild_from_snapshot` replaces `view.sim` and a few world resources but never clears `View::pre_launch` (lib.rs:1562; only writers are init L2108 and launch capture L2995). `GameMenuRes.revert_tick` is restaged every present from that stale `pre_launch` (lib.rs:2258), so the Game menu's Revert button stays enabled after a load (game.rs:113–120 gates only on `revert_tick.is_some()`), and the confirm prompt even quotes the OLD session's tick (game.rs:149–151 "restores the pre-launch snapshot (tick {tick})").
**Scenario**: session A launches (revert point captured) → Game menu → load save B → press F10 (arms, `revert_available()` still true, csp-app lib.rs:2644) → REVERT → the world from save B is silently replaced by session A's pre-launch snapshot. F10 shows the §3.4 disclosure, but for the wrong session — from the player's view this is "loading my save then pressing one button threw it away". PLAN:1180–1184 defines revert as restoring *the* pre-launch snapshot of the current game.
**Impact**: cross-session state leak; effective loss of the loaded session's progress (the file on disk survives, the live world does not). This is exactly the "panel state vs sim state desync after load" class.
**Suggestion**: in `load_game` (only there — revert must keep its point): `self.pre_launch = None;` plus `self.world.resource_mut::<GameMenuRes>().revert_tick = None;` (or let the existing restage pick it up) and clear `game.ui.confirm_revert`. `new_game` is safe already (whole `View` replaced, lib.rs:3132–3146).

### [MEDIUM] Finding 2: Deploy-parachute verb exists three times with drifted gating — the Program window copy dead-clicks on the surface
**File**: crates/csp-ui/src/program.rs:2069–2075 (window, ungated) vs 2295–2307 (part menu, gated) vs 2614–2627 (action bar, gated)
**Problem**: the sim requires `InBubble` for `DeployPart` (csp-sim/src/lib.rs:7223–7225 "deploy: vessel not in bubble"). The two NEW surfaces gate the button on `input.residence == "bubble"` with a reason ("deployable only while flying — not on the surface"); the Program window's chute row (L2069–2075) has no gate at all.
**Impact**: a latched (landed) or on-rails vessel shows an enabled "deploy" in the Program window; the click is refused at the drain and surfaces only as the generic "COMMAND REFUSED — DEPLOY: VESSEL NOT IN BUBBLE" cue — the affordance says yes, the game says no. Affordance inconsistency inside one file, on the exact surface (complaint #1's loop) the uncommitted pass was fixing.
**Suggestion**: single `chute_row(ui, input, node, emit)` helper carrying the residence gate, used by all three surfaces (see Finding 4).

### [MEDIUM] Finding 3: Lab "research" button ignores its own crew precondition — dead click with only a hint in the hover
**File**: crates/csp-ui/src/program.rs:2100–2114; resolver crates/csp-view/src/lib.rs:1208–1221
**Problem**: `apply_start_research` refuses empty researchers (research.rs:985–987) and the resolver drops the command with "RESEARCH DROPPED — NO CREW ABOARD" (lib.rs:1218–1221). The panel renders one "research {label}" button per pooled row per idle lab whenever a pooled record exists — crew presence is never staged or gated; the hover merely hints "(crew aboard staff it)" (L2104).
**Impact**: on a crewless lab vessel (probe-core science craft — the normal early game), the button is enabled and every click bounces off the drop-cue. This is complaint #6 territory (research UI confusion) and violates the file's own W1.5-e discipline two rows up, where transmit is disabled-with-reason on the EC gate.
**Suggestion**: stage a `crew_aboard: usize` (the mirror already builds `researchers` from `v.crew`, lib.rs:4987–4993) and `add_enabled(!input.crew_aboard == 0, …)` with hover "needs crew aboard to staff the lab — board or transfer a crew member first".

### [MEDIUM] Finding 4: The science/recovery verb rows are copy-pasted across three surfaces and have already drifted (DRY/CRAP)
**File**: crates/csp-ui/src/program.rs:1981–2057 + 2060–2077 (Program window) vs 2317–2383 (`pooled_rows`) + 2388–2423 (`vessel_recovery_row`) + 2255–2311 (`part_node_actions`)
**Problem**: the uncommitted pass extracted shared row-renderers for the NEW surfaces (`pooled_rows`, `vessel_recovery_row`, `part_node_actions`) but left the Program window's older inline copies in place. Result: (a) the transmit/recover pair with its EC-gate arithmetic and hover copy exists twice (window L2011–2057 vs `pooled_rows` L2317–2383) with already-different hover wording for identical verbs; (b) the recover-vessel/terminate pair exists twice (window L1981–2009 vs `vessel_recovery_row` L2388–2423 — the latter even carries a dead `let _ = dim;` at L2421, the tell of a mechanical extraction); (c) the chute row three times (Finding 2's gate drift is the direct cost of this duplication). Additionally `program_panel` itself is a 415-line function under two clippy allows (L1785–1786) whose RESEARCH/SCIENCE/LABS blocks (L1837–2140) were never extracted, breaking the `*_section` pattern the other seven sections established.
**Impact**: every future gate/copy fix must be made 2–3 times; one was already missed (Finding 2). In the fastest-growing file of the codebase (505→2,854 lines), this is where maintenance rot starts.
**Suggestion**: make the Program window emit into the same channel the shared helpers already write — either give `program_panel` a `&mut FlightActionOutput` (its `commands` are the same `ProgramCommand`s; fold `open_program` back as today) or take `&mut dyn FnMut(ProgramCommand)`. Then delete the window's inline pooled/vessel/chute blocks in favor of the helpers, add the missing `chute_row`, and extract `research_section`/`science_section` to close out `program_panel`'s allows. Net −200 lines and drift-proof by construction.

### [MEDIUM] Finding 5: §3.4's stress multipliers are displayed in the editor but not in flight (PRD coverage)
**File**: crates/csp-ui/src/program.rs:2152–2184 (RELIABILITY section); PLAN.md:1164–1167
**Problem/evidence**: PLAN §3.4: "**Stress rules** (data-driven): throttle above rated, burn beyond `rated_burn_s`, temperature fraction of `max_temp_k`, g-load, ullage-negative ignition, restarts beyond `ignitions`. Each multiplier is **displayed to the player** in editor and flight. Hidden stress multipliers are bad design." The editor half exists (`csp-ui/src/vab.rs:3506` — the "displayed to the player" surface for `regime_stress`). The flight half does not: `ReliabilityRow` (program.rs:129–146) and the RELIABILITY section (L2152–2184) render only outcome state — applied failure modes, misfire, thrust factor, Λ/θ wear %, dormant flag — never the live per-part stress multipliers; nothing else in csp-ui's flight surfaces renders them either (`grep stress` in csp-ui hits only vab.rs and the settings dormant toggle).
**Impact**: a part failing from a stress rule (e.g. g-load spike) is reported as an outcome with no cause — exactly the "hidden stress multipliers are bad design" the plan names. Not on the §6.4 cut list, so unsanctioned.
**Suggestion**: extend the staged reliability rows with the part's live `Π(stress_i)` breakdown (the sim already evaluates it for λ) and render it on the wear line, e.g. `ok (wear 12% · λ×2.1: throttle 1.4 · temp 1.5)`.

### [LOW] Finding 6: Save-name field is cleared before validation; refused names echo raw into notices
**File**: crates/csp-ui/src/game.rs:85–90; enforcement crates/csp-app/src/lib.rs:3183–3191 (`safe_slot_name`)
**Problem**: the menu clears `state.save_name` the instant Save is clicked; the app-side validator then refuses names with `/`, non-ASCII, leading/trailing space, etc., with a "refused save name '{slot}'" notice (csp-app lib.rs:1103–1107). The typed text is gone (retype), and the raw string — which may contain control characters/newlines — is echoed into the HUD cue/log (minor log-injection surface; local input only, so low).
**Suggestion**: pre-validate in the menu with the same character rule (disable Save + hover "letters, digits, - _ . space only"), clear only on emit; csp-app should keep its check (defense in depth) but sanitize the echo (`name.escape_debug()` or filter control chars).

### [LOW] Finding 7: Quick-transfer buttons mislabel the amount when stock < 10
**File**: crates/csp-ui/src/program.rs:1039–1041 ("→v{n} 10" sends `10.0_f64.min(*units)`), 1327–1329 ("deliver 10" same), 1363 ("withdraw" sends `10.min(level)`)
**Problem**: with 3.4 units pooled the button still reads "10" but sends 3.4 — and duplicates the adjacent "all" button exactly.
**Impact**: minor copy dishonesty in a data-moving verb; the label is the only preview the player gets.
**Suggestion**: hide the fixed-amount variant when `*units < 10.0`, or label with the actual amount.

### [LOW] Finding 8: "need ⚛" tag doesn't explain banked vs live science (complaint #6 residue)
**File**: crates/csp-ui/src/program.rs:1866–1870
**Problem**: tech rows show "need ⚛" when `banked < cost` even though the OVERVIEW's live science (banked + running-lab accrual, staged from `science_now`, research.rs:619–627) may exceed the cost — the sim correctly refuses (`tech.rs`: "insufficient banked science"). The panel is honest ("science ⚛ 12.3 (banked 4.5)") but the row-level refusal doesn't connect the two.
**Suggestion**: on the "need ⚛" tag hover: "spends banked science only — a running lab's accrual banks when it stops or exhausts".

### [NIT] Finding 9: Confirm dialogs swapped exact centering for hardcoded half-size guesses
**File**: crates/csp-ui/src/game.rs:141–144, 174–177 (ff897d4: `.anchor(CENTER_CENTER)` → `.default_pos(center − {190,80})`)
The offsets assume the dialog's measured size; if the disclosure text wraps wider (small windows, large fonts) the dialog parks off-center, and egui now also remembers a player drag of a modal prompt. Strip movability is the point of the change; the confirm prompts were fine anchored. Cosmetic only.

---

## PRD coverage table (scope of this cluster)

| Requirement | Status | Evidence |
|---|---|---|
| §3.2 run/transmit/recover science | **WIRED** | panel + part menu + Actions strip emit the same verbs; EC gate staged with the exact arithmetic `apply_transmit_data` enforces (research.rs:804–810 vs staging lib.rs:3876–3910); recover remainder (`1−ratio` blanket over pooled units) matches `apply_recover_data`/`apply_recover_vessel` |
| §3.2 labs escrow/accrual | **WIRED** | Start/StopResearch buttons + project rows ("acc · data left · eta") read the same closed forms research.rs evaluates (staging lib.rs:4859–4882); stop hover "bank the accrued science and release the rest" matches `apply_stop_research` (banks accrual, returns remainder to pool, research.rs:1081–1112) |
| §3.2 diminishing repeats | WIRED (sim-side; per-run yield preview not mandated as a display) | yield curve in research.rs; `collected` bookkeeping |
| §3.3 tech tree + upgrade tracks | **WIRED** | research buttons gated on prereqs+banked (matches `apply_research_tech` banked check); tracks show capped/locked/maxed; facility unlocks in tree (map §2) |
| §3.4 revert + disclosure | **WIRED** (with Finding 1 bug) | prompt quotes the PLAN:1180 policy verbatim (game.rs:152–156); F10 arms the SAME prompt (csp-app lib.rs:2643–2650 → `arm_revert_confirm`), no disclosure bypass |
| §3.4 bench/test stand | **WIRED** | `test +5/+25` buttons disclose funds+resource+time and gate on exactly what `apply_bench_test` checks (reliability.rs:1168–1210) |
| §3.4 stress multipliers in flight | **DEGRADED** | Finding 5 — editor-only (vab.rs:3506); PLAN:1166 mandates "editor and flight"; no §6.4 sanction |
| §3.5 funds/hire/salary/recovery/terminate | **WIRED** | funds from `funds_micros` (µ→whole, no rounding trap); hire gated on funds (sim enforces same); recovery site label honest incl. `abandon_recovery` remote-salvage leg (economy.rs:899–914) |
| §3.5 logistics transfers | **WIRED** | linked-vessel rows (`linked_to_focus` = sim's link predicate), colony deliver/withdraw under the joined colony only (the sim's join gate) |
| §3.5.1 colonies | **WIRED** | state/day-night/modules/fraction/stores/T_eq/inspection/evacuate/abandon/found ("prototype, no cost" honest — sim has no founding cost); starter def staged view-side |
| §3.6 contracts | **WIRED** | offers carry params/reward/deadline and are disabled-with-reason on unmet prereqs (the accept predicate's own reading); active rows show live per-kind progress |
| §2.10 scripting surface | **WIRED** | load (stock + paste), pause/resume, unload; strikes "/5", disabled reason, armed wake all displayed per PLAN:737's "visibly, with the reason in the UI" |
| §7 save/load | **WIRED** | slots + F5/F9/F10; §7.4 refusals surfaced with reason; atomic tmp+rename; slot names validated against traversal (safe_slot_name); header-only listing, newest-first |
| M10 UI quality | **WIRED** | movable panels (window/status strip), keybind hints (`OpenProgram` wired end-to-end), chip-framed readouts |

## Top 3 issues
1. **[HIGH] Stale revert point survives load** (Finding 1) — one-line fix in `View::load_game`; until then, load→F10→REVERT silently discards the loaded world.
2. **[MEDIUM] Deploy-verb gate drift from triplicated rows** (Findings 2+4) — the Program window's copy is the ungated one; the duplication is the cause, unify the emitters.
3. **[MEDIUM] Ungated lab-research button on crewless vessels** (Finding 3) — complaint #6's remaining dead click; stage crew count, disable with reason.
