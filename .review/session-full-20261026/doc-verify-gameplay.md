# Doc verification — docs/GAMEPLAY-GUIDE.md (session-full-20261026)

Verifier: independent documentation verifier, 2026-10-26. Method: full read of the doc (~490
lines) against the working tree by direct source read of every cited file/symbol (csp-ui,
csp-app, csp-view, csp-sim, csp-phys, data/*), plus cross-check against this session's own
verified review reports (verified-report-C23.md et al.) where the doc makes verdict claims.
No files outside this report were written.

## Summary

- Claims checked (clustered): **72** — controls/keymap, VAB rules, launch economics, flight
  readouts, science numbers, contracts, warp, save/revert, feature table, 6 complaint verdicts.
- **VERIFIED: 64** · **MINOR-INACCURATE: 5** · **INACCURATE: 3** · UNSUPPORTED: 0.
- Status counts in the doc's own feature table, recounted by me: **22 PLAYABLE / 4 PARTIAL /
  1 HIDDEN / 2 MISSING rows** (the MISSING rows enumerate 8 absent features; "docking" and
  "web demo" are the weakest PARTIAL rows and both hold up).
- **Top-line result: PASS-WITH-FIXES.** The guide is overwhelmingly accurate — every
  mechanical constant, refusal string, data value, and code anchor I traced matches the tree,
  many at exact line numbers. But three claims are wrong in substance: complaint #2's "FIXED"
  verdict (the repo's own verified review says PARTIALLY FIXED and the code confirms it), the
  §5.2 diminishing-returns percentages (right shape, wrong numbers for every shipped
  configuration), and a fabricated FAQ troubleshooting scenario (stale throttle on rails).
  All three are one-line fixes.

## Claims table

| # | Doc claim (section) | Verdict | Evidence |
|---|---|---|---|
| 1 | §2.1 keymap table, 16 rows / defaults M V Esc . , Z X Shift Ctrl Space Tab T WASD/arrows Q E F1 / F3 P F5 F9 F10 | VERIFIED | `csp-ui/src/settings.rs:559-582` — `KeyAction` enum is exactly 24 actions; the doc's 16 rows cover all 24 (pan/throttle/warp/roll/quicksave rows combine pairs). All default tokens match. |
| 2 | §2.1 physical key positions (`key-w` = QWERTY W), rebindable, `Keymap::bind` evicts stolen key | VERIFIED | egui `KeyBinding` physical-key tokens; bind/evict test at settings.rs:971-977. |
| 3 | §2.3 warp cap options ×1…×100,000; settings persist `$XDG_CONFIG_HOME/csp/settings.ron`; new-game-only difficulty | VERIFIED | `WARP_CAP_OPTIONS` at settings.rs:163 (exact); `settings_io.rs:35-44` config_dir XDG/macOS/APPDATA; difficulty applied at save creation (settings.rs:915). |
| 4 | §1 bubble 20 km / 64 Hz; ₣1,000,000 start; 6-slot board; 16 parts; Stock Hopper = pod+canister+battery+s160+Sparc, TWR ≈ 5; Rask Mines colony; tutorial on screen | VERIFIED | balance.ron `bubble_radius_m: 20_000`, `starting_funds_micros: 1e12` (1e6 µ₣ = ₣1), `contract_offer_slots: 6`; 16 files in `data/parts/`; demo.rs:53-63 STARTER_CRAFT exactly those 5 parts; wet mass 3,860 kg × 9.81 m/s² vs 200 kN ⇒ TWR ≈ 5.3 (vac) — "≈5" fair; colonies.ron `rask-mines`; tutorial.rs STEPS. |
| 5 | §3.4/§11 launch is a build order; refusals: tech-locked, unresolvable, funds, BOM short, vessel cap, active vessel on pad; every reason on the notice row | VERIFIED | `csp-view/src/lib.rs: launch_preflight` (3441-3524) + tech-lock pre-check in `launch_craft`; all six refusal branches present with named reasons. |
| 6 | §3.4 parked latched vessel auto-recovered ("PARKED BUILD RECOVERED"); funds+BOM debited at order; `ManufactureCraft` (lib.rs:3015); delivery after mass/50 kg/s latched; view switches to Flight; pre-launch revert snapshot | VERIFIED | lib.rs:2966-3075 verbatim; `manufacture_rate_kg_per_s: 50.0`; "BUILD ORDER PLACED — delivery in …" notice; `self.pre_launch = Some(snapshot)`; `set_mode(ViewMode::Flight)`. |
| 7 | §11 pad stock 2000 metal / 400 composite; Sparc 70 metal + 10 composite; pad recovery 100% | VERIFIED | sites.ron `starting_stock`; csp.engine.sparc.ron:22 `build_resources`; sites.ron header "recovery_efficiency 1.0" for launch site. |
| 8 | §4.3 auto bubble→rails: altitude above atmosphere top (Terrin 70 km), no live contact, Pe > radius + max(atm_top, bubble_radius); no manual go-on-rails verb | VERIFIED | `apply_to_rails_at` at csp-sim/src/lib.rs:6206 (exact anchor) — all three conditions as stated; terrin/body.ron `top_m: 70_000`; `SimCommand::ToRails` has no UI/csp-view dispatch (grep: only sim-internal + comments). Auto conversion at the tick-range break (lib.rs:4866). |
| 9 | §4.2 HUD blocks: ORBIT/SURFACE/VESSEL/PROP fields incl. TWR·SL, MAXF, ΔV STG+, ISP; `—` placeholders; status strip ₣/⚛/✉ clickable | VERIFIED | flight_hud doc comment + labels `TWR·SL` (lib.rs:895), `MAXF` (:889); `program_status` (program.rs:852, movable Area returning click bool). |
| 10 | §4.2 notice stack in every view, chip + single-line | VERIFIED | `notice_overlay` (lib.rs:702) — `hud_frame()` chip, `TextWrapMode::Extend`, `.selectable(false)` labels; drawn unconditionally. |
| 11 | §4.4 maneuver node panel: one node per vessel, executor flies burn, warp demotes before burn | VERIFIED | `vessel.maneuver` is a single `Option` (csp-sim); maneuver.rs module doc: `ManeuverNodeStart` is a §2.6.5 Clamping member. |
| 12 | §4.4 planning panel: alarms, transfer eligibility notes, Lambert Find window, watch/alarm verbs | VERIFIED | `planning_panel` at lib.rs:1465; "in physics bubble — coast to rails first" string at csp-view/src/lib.rs:942. |
| 13 | §4.5 T = ToBubble + player lease, deorbit verb; chute 6 km / 260 m/s / 120 m², flying-only deploy; recovery payouts (1.0/0.9/0.25 + resources + pooled science + crew); terminate | VERIFIED | `recover_control` (csp-view lib.rs:2615); chute part `deploy_alt_m: 6_000`, `deploy_speed_mps: 260.0`, `deployed_drag_area_m2: 120.0`; `apply_deploy_part` at lib.rs:~7210 requires `InBubble`; `apply_recover_vessel` at economy.rs:881 + `VesselRecovered` trigger :~1006 (doc's 880/1006 anchors: exact/near-exact). |
| 14 | §5.1 one experiment; canister one-shot 10 units; mask 49 = Landed/SpaceLow/SpaceHigh | VERIFIED | data/parts/csp.science.canister.ron: `data_units: 10.0, reusable: false, situations: 49`. |
| 15 | §5.2 caps Terrin 30 / Rask 60 / Duva 90 / Solen 50 space-only | VERIFIED | exp.materials_study.ron caps table (48 = space-only for Solen). |
| 16 | §5.2 "run 1 ≈ 63% of cap, run 2 ≈ 86%, run 3 ≈ 95% (k = 1.0)"; formula `cap·exp(−k·collected/cap)·units` | **INACCURATE** (wrong numbers for every shipped config) | research.rs applies the exact integral `C' = C + (cap−C)·(1−exp(−k·u/cap))` (:741-753), and the 63/86/95 sequence holds **only for runs of cap/k units** (test `softcap_reproduces_plan_sequence` uses `run = cap`). The shipped canister runs 10 units: on Terrin (cap 30) runs 1-3 bank ≈28%/49%/64% cumulative; Rask/Duva are slower still. No body gives 63% on run 1. FAQ §11 repeats the wrong figures. Fix: state the formula actually applied and per-run yields for u=10 (or the run-of-cap condition). |
| 17 | §5.3 transmit = 40% now, 1 EC/unit, disabled "needs N EC (have M)"; recover = 100% or 60% remainder, closes key, latched-gated | VERIFIED | research.rs `apply_transmit_data` (:774) / `apply_recover_data` (:861) — ratio 0.4, `transmit_ec_per_data_unit: 1.0`, `(1.0 - ratio)` remainder, `closed = true`, `SurfaceLatched` gate. |
| 18 | §5.4 ₣10 per banked science point at milestone edge | VERIFIED | `science_funds_per_point: 10.0`; `credit_science_payout` on both transmit and recover. |
| 19 | §5.5 labs: mk1 2 seats / 2 researcher slots / tech.science.1; escrow + convert over sim-time; advances under warp | VERIFIED | csp.lab.mk1.ron (`CrewSeat(slots: 2)`, `researcher_slots: 2`, `tech_requirement: "tech.science.1"`); research.rs StartResearch/escrow; interval-subsystem closed form. |
| 20 | §5.7 tutorial STEPS[7..10] = run → bank → research with real-state predicates; step 7 coaches orbit close | VERIFIED as written, **but omits a verified live defect** | tutorial.rs STEPS: indexes 7/8/9 are exactly that loop; step 7 (index 6) "Close the orbit". However the rewind at tutorial.rs:264-267 (`step > STEP_LAUNCH && !has_vessel`, unscoped) is still in the tree — this session's verified-report-C23.md Finding 3 empirically confirmed that following the coached "land and recover" path rewinds the tutorial to step 2 ("vessel lost") and blocks step 9 until a new launch. A "working-tree truth" guide walking a new player down exactly this path should carry a known-issues note. Downgraded to MINOR-INACCURATE (omission with player impact). |
| 21 | §6 six contract templates and completion recipes; survey starter-completable; satellite band Pe 80-250 km / Ap ≤1500 km; part-test reads vessel state (bench doesn't count); rescue = recover event on body; resupply = colony deliver | VERIFIED | data/contracts/*.ron (bands as stated) + contracts.rs `objective_met` (:942): SatelliteOrbit non-debris closed-conic band check; PartTest module-presence on vessel state; Rescue `VesselRecovered(body)` trigger; ColonyResupply cumulative `colony_deliveries`. All six are player-completable — the doc's "the stale 2/6 uncompletable finding no longer holds" is correct. |
| 22 | §6 "already-satisfied state-predicate offers (satellite/part-test) can complete at acceptance"; advance credited at accept; board gated by tech ⇒ survey-only at start | VERIFIED | `apply_accept_contract` (:812-922): `objective_met(&params, None)` completes at accept (Rescue returns false for `None` — matches "state-predicate" wording); advance credit; tech gating per templates' prerequisites. |
| 23 | §6 survey.data "(≥5 units) — same, more runs (the cap forces multiple situations/biomes)" | **MINOR-INACCURATE** | min_units is 5.0 and a single fresh-key run yields ≈8.5 units on Terrin — one run completes it; nothing forces multiple situations/biomes. Also silently omits the second prerequisite (`ContractsCompleted(SurveyRun, 1)`). |
| 24 | §6 RESEARCH: 4 nodes 0/15/30/60; 5 tracks (Sparc Isp ×1.03 ×4, Sparc thrust, Ember Isp +5 s, tank −5%, pod MTBF ×1.25); upgrades frozen at build | VERIFIED except Ember | techtree/home.ron matches exactly, except Ember's levels are +5/+5/**+10** s — doc's "+5 s" describes levels 1-2 only. `BuiltPart.upgrade_levels_at_build` exists (csp-phys/resolve.rs:260). MINOR. |
| 25 | §6 crew ₣5,000/hire, 30-day salaries; →vN 10/all transfers (100 m link); colony verbs evacuate/inspect +1d/abandon/found outpost | VERIFIED | balance.ron `crew_hire_cost_funds: 5_000`, `salary_period_days: 30`, `transfer_radius_m: 100`; program.rs buttons at :1282-1397, :1040-1041. |
| 26 | §6 FACILITIES test stand ₣50 + 1 metal + 60 s/unit; hazard → ×0.45 | VERIFIED | balance.ron `reliability_bench_*` (50/1/60); canister qualification curve 1.0→0.5 at 100 units — the ×0.45 figure is the deeper engine curve (spot-checked plausible; knots in part files). |
| 27 | §7 ladder ×1…×100,000; ×2 physics / ×10+ rails (no bubble ticks) | VERIFIED | `WARP_LADDER` at csp-app/warp.rs:33 (exact anchor); warp.rs module doc "×2 physics warp … then rails ×10…×10⁵"; `step_bubble_range` early-returns when no `InBubble` vessel (csp-sim lib.rs:3995-4001). |
| 28 | §7 spoken refusals: `WARP REFUSED — <REASON>` per press; latched `RAILS WARP ×1 — <REASON>`; `WARP ALREADY AT ×N — SETTINGS LIMIT/LADDER TOP`; `WARP LIMITED — ×N EXCEEDS THE STEP-TIME BUDGET`; reasons: thrust/contact/atmosphere/script | VERIFIED | warp_step_request (csp-app lib.rs:1398) and on_step_report (:1251, refusal read at :1295) — all four strings verbatim; `rails_warp_refusal` at csp-sim lib.rs:3707 (exact anchor) names bubble-resident thrust/contact/atmosphere and polling scripts. |
| 29 | §7 "leaving the bubble zeroes [throttle]" | VERIFIED | auto-rails conversion requires `throttle == 0` (lib.rs:4791); "on-rails vessels have no throttle" (:6184). |
| 30 | §11 FAQ "a vessel that left the bubble keeps a stale throttle until zeroed" | **INACCURATE** (contradicts §7 and the code) | No "stale throttle" exists anywhere in the tree (grep: 0 hits); the under-thrust refusal fires only for `InBubble` vessels (lib.rs:3731-3737) and a vessel only leaves the bubble coasting at throttle 0. The FAQ scenario cannot occur and directly contradicts the doc's own §7 parenthetical. Delete the clause. |
| 31 | §7 interval subsystems under warp (colonies, build orders, labs, deadlines, salaries, dormant hazard, transfers, debris decay); clamping demotions (alarms, SOI, node start, deadlines, research, insolvency) | VERIFIED | `advance_interval_subsystems` (lib.rs:7668) + event kinds; insolvency event in economy.rs (:70-73); `ManeuverNodeStart` Clamping. |
| 32 | §8 save/load slots `.cspsave`, quicksave/F5/F9, save refused while pending/contacts/joints/tainted; F10 revert with verbatim honesty disclosure; crash dialog read-only | VERIFIED | do_load at csp-app lib.rs:~1157 (doc anchor 1159: near-exact); §7.4 invariant `check_serializable`; game.rs:153-155 disclosure string matches the doc's quote word-for-word; `crash_dialog` (csp-ui lib.rs:219). |
| 33 | §9 alarms playable; autopilot PARTIAL ascent-only (10-10,000 km, in-bubble only); scripting 4 stock scripts, strikes 5, 20k ops, wake verbs; MCP native-only | VERIFIED | STOCK_SCRIPTS ×4 incl. "circularize at apoapsis" and "ascent hand-off" (program.rs:770+); target range `10.0..=10_000.0` (program.rs:1640); `strikes >= 5` disables (scripts.rs:6); `script_op_budget: 20_000`; csp-bench/src/bin/csp-mcp.rs; csp-server (axum + ws); apps/telemetry exists. |
| 34 | §3.1 VAB rules: ghost placement, auto/axial/side mount, symmetry ×1/2/4 (X cycles), subtree grab, root refuses grab, reroot, translate, Del/Backspace, remove-only promotes children, Ctrl+Z/Ctrl+Shift+Z, 16 parts, PartLocked/NoCompatibleAttach named errors | VERIFIED | vab.rs: `VabAction` enum (Grab/Remove/Reroot/Translate/RemoveOnly/SetSymmetry/Undo/Redo), root-refusal comment at :197-200 (doc anchor 200: exact), `AttachMode` at :70 (exact), module doc "`X` cycles radial symmetry 1→2→4", undo = ctrl+Z / ctrl+shift+Z (vab.rs:2336-2337), error enum :299/:318. |
| 35 | §3.2 readouts: wet mass, Δv vac/SL, per-stage rows, mission-profile margin strings, "clean — launchable" | VERIFIED | `csp_phys::readout::analyze` (readout.rs:143); "short on Δv or launch TWR" vab.rs:3123; "clean — launchable" vab.rs:3602 (exact anchor). |
| 36 | §3.3 craft files native-only, hidden in web demo | VERIFIED | vab.rs:3183-3186 `if vab.file_io()` — "hidden entirely in the demo build". |
| 37 | §10 feature table row spot-checks (8 weakest): docking PARTIAL (part + joints exist, tech.rocketry.3, no UI, unverified in play); life support HIDDEN-by-default; web demo PARTIAL (60-part cap, no craft IO/saves); colonies PARTIAL (fixed prototype via FoundColony); autopilot PARTIAL; MCP PLAYABLE-for-agents; sound PLAYABLE; 16 parts | VERIFIED | dockport part + joints.rs exist, no dock UI strings in csp-ui; `life_support_enabled: false` default (csp-core, per-save toggle); `DEMO_PART_LIMIT: 60` (demo.rs:25); FoundColony prototype; axum-ws server; audio.rs/synth.rs. Table recounts 22 PLAYABLE / 4 PARTIAL / 1 HIDDEN / 2 MISSING rows. |
| 38 | Complaint #1 (science/recovery undiscoverable) — FIXED | VERIFIED | part_menu (program.rs:2432), flight_action_bar (:2542), clickable status strip, tutorial tier. |
| 39 | Complaint #2 (widgets fixed-position) — **FIXED** | **INACCURATE** (should be PARTIALLY FIXED) | This session's verified-report-C23.md empirically confirmed (egui 0.37 headless harness): the readout block **cannot be dragged by its own text** — the row labels at csp-ui/src/lib.rs:786-810 are default-selectable `ui.label`s that win the hit-test over the Area's move widget (still unfixed in the current tree — I re-read the code); and the navball's first-open parking is 242 px too high (no `.default_size`, lib.rs:1018-1024 — also still unfixed). Windows/status strip/Game strip drag fine; the navball Area drags from its parked rect. The doc's own sibling review grades this PARTIAL; the guide should too (and §4.2's "bottom-center" navball deserves the mis-park caveat). |
| 40 | Complaint #3 (sky grain/stars) — ADDRESSED IN CODE, visually unverified | VERIFIED (honest) | terrain.wgsl ~288-296: 12 per-step jittered taps ("~√N quieter"); ~1098-1105: `smoothstep(6.0e-4, 4.0e-3)` star gate — both at the doc's cited lines. |
| 41 | Complaint #4 (warp refuses silently) — FIXED | VERIFIED | See row 28. |
| 42 | Complaint #5 (notice readability) — FIXED | VERIFIED | See row 10; verified-report-C23 upheld with contrast measurements. |
| 43 | Complaint #6 (research/science UI confusing) — PARTIALLY FIXED, residual named | VERIFIED (honest) | Matches the verified reviews' grading. |
| 44 | Footer: "verified by direct read of the cited files … purely visual qualities marked unverified" | MINOR-INACCURATE | Direct read is exactly the method that cannot validate interaction claims like complaint #2's "movable" (the Area is movable; the *drag from text* is not) — the footer's caveat covers sky/"feel" but the untested-claim boundary is wider than stated. |

## Worst three inaccuracies

1. **Complaint #2 verdict "FIXED"** (GAMEPLAY-GUIDE.md §10 complaint table): should be
   **PARTIALLY FIXED** — readout block not draggable over its text (labels selectable,
   csp-ui/src/lib.rs:786-810), navball parks 242 px high on first open (lib.rs:1018-1024);
   both confirmed by this session's verified-report-C23.md and re-confirmed present in the
   tree by this verifier.
2. **§5.2 science diminishing-returns numbers**: "run 1 ≈ 63% / run 2 ≈ 86% / run 3 ≈ 95%"
   hold only for runs of cap units; the shipped 10-unit canister banks ≈28/49/64% of the
   Terrin cap over three runs. Wrong substance for every playable configuration; FAQ repeats it.
3. **§11 FAQ stale-throttle scenario**: "a vessel that left the bubble keeps a stale throttle
   until zeroed" is unsupported by any code (0 grep hits), contradicts §7's correct
   "leaving the bubble zeroes it", and describes an impossible refusal cause.

Minor: survey.data "cap forces multiple situations/biomes" (one 8.5-unit run completes a
5-unit ask); Ember track "+5 s" (third level is +10 s); §5.7 tutorial described without its
verified rewind-on-recovery defect; footer's verification-method caveat too narrow.

## Completeness — missing topics (would a new player/dev learn to play from this alone?)

Mostly yes for the core loop; gaps, in rough priority order (max 10):

1. **Known-issues section** — the guide claims working-tree truth but omits the three
   verified live defects above (readout drag, navball parking, tutorial rewind).
2. **Situation band definitions** — which altitudes split Flying/SpaceLow/SpaceHigh; the
   science loop depends on them and no threshold is given anywhere.
3. **Control authority requirements** — does a craft need a crewed pod or probe core to
   accept throttle/stage/autopilot? Never stated.
4. **Re-entry heating and structural failure as gameplay** — parts carry max_temp and
   structural limits; "drag does the rest" never says whether you can burn up or break
   apart, or what a FAILED part does to a live flight.
5. **Multi-vessel control** — "player lease" is named but switching control between two
   in-flight vessels (vs map camera focus) is undocumented.
6. **Docked/linked resource + crew transfer mechanics** — the buttons are listed but the
   10 s crew traversal delay and docked-port flow are not.
7. **Biomes** — the science cap keys on biome; the guide never says how to tell which biome
   you are in or that biomes.ron defines them.
8. **Debris economics** — dropped stages become debris (cap 64, decay under warp); what is
   recoverable and what evaporates is not explained.
9. **Insolvency consequences** — "demotes warp and notifies" but the actual effect (funds
   clamp, penalty scheduling) is one clause.
10. **Life-support rates when enabled** — one toggle line; consumption rates and what
    happens on shortfall are absent (a player enabling it learns nothing).

## Verdict

**MINOR-INACCURATE → INACCURATE on three specific claims; overall PASS-WITH-FIXES.**
Fix complaint #2's verdict to PARTIALLY FIXED (with the two C23 caveats), replace the
§5.2/FAQ repeat percentages with the u=10 numbers (or state the cap-units condition), delete
the FAQ stale-throttle clause, soften survey.data's "forces multiple runs", correct Ember's
+10 s third level, and add a short known-issues list — then this guide is fully trustworthy.
