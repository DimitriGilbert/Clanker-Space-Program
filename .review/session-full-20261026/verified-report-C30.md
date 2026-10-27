# Verified Report C30 — verification of review-report-C30.md

Verified against working tree HEAD `7dd4b86` on 2026-10-26. Every file:line citation
was opened and read in context; empirical checks: `cargo test -p csp-bench --lib` →
53 ok, `--test eval_suite` → 12 ok (both re-run during verification — matches the
report's premise that the suite is green). Result: **9 confirmed, 0 dismissed**.
One minor wording imprecision noted inside Finding 4 and Finding 8; neither changes
a verdict.

---

### Finding 1: Eval tier proves only the M2 flight arc; format cannot host the new loop — CONFIRMED

**Original**: PLAN §8.5 requires "contract lifecycle" + "colony supply loop" proofs;
the 5 committed scenarios are 3 ascent variants + 2 plumbing probes; `WorldSpec` has
only `Empty|Fixture`; the fixture ships no contracts/recipes/experiments/colonies;
`ManufactureCraft`/`BoardCrew`/`JoinColonyInventory`/`ResourceTransfer`/`RunExperiment`
have zero non-comment issuers in csp-bench.

**Verification**:
- `scenario.rs:62-72` — `pub enum WorldSpec` has exactly `Empty` and `Fixture{name}`. No data-root variant. `csp-eval.rs` (whole bin read) offers only `--fixtures-root`, so the bin cannot reach shipping data either.
- `scenarios.rs:50-63` — `SCENARIOS` registry contains only `trivial` and `flight-launch`.
- `ls data/test-fixtures/flight-launch/` — `balance.ron`, `sites.ron`, `system.ron`, `terrin/body.ron`, `solen/body.ron`, plus golden artifacts. **No** `contracts/`, `recipes/`, `experiments/`, `colonies.ron`.
- `scenarios.rs:145-248` (`flight_initial`) — loads experiments (186-194), contracts (209-217), recipes (228-235), colonies (241-247) from those absent paths. `csp-data/src/load.rs` confirms every loader returns an empty set on a missing path (e.g. `load_colonies_from` 519-523, `load_recipes_from` 575-579, `load_experiments_from` 639-643, `load_contracts_from` 710+). `flight_initial` never calls `set_tech_tree` or `set_parts`. So an `AcceptContract` setup command hits an empty board (board is a pure function of `contract_defs` per `missing_config_channels`'s own doc), `ManufactureCraft` cannot resolve `RecipeId`s, `RunExperiment` rejects on empty `experiments` (the sim's own doc at lib.rs:2744 states this verbatim).
- `ls data/eval-scenarios/` + full reads: `ascent-mission`, `lease-launch`, `script-ascent` (ascent), `idle-warp`, `log-replay` (plumbing). None touches manufacture/contracts/colonies/crew/science/save-load.
- Grep across csp-bench for the 5 commands: only hits are `FoundColony` in code (economy.rs:254, perf.rs:917 — not in the claimed list) and comments in replay.rs:804/808. The five named commands: zero code occurrences. (`BoardCrew` is a real variant, csp-sim lib.rs:825.)
- PLAN §8.5 (line 1806) quote verified verbatim: "Whole systems interact correctly over mission-scale time: launch→orbit→land, contract lifecycle, colony supply loop, agent completing a mission".
- PLAN §6.4 (1543+) cut list verified: render tier, life support, third body, docking, web polish — e2e coverage is not on it.
- `contracts.rs:69-112 contract_world` verified — full shipping wiring (system, balance, science, experiments, tech tree, economy, sites, contracts), reachable only from Rust (probe + its tests), never from `.ron`.
- Suggestion's premise verified: `csp-server/src/telemetry.rs:49-73` — `ProgramSnapshot` exposes `funds_micros`, `contract_offers`, `contracts_active`, `crew_roster`, `colonies`, `sites`.

**Verdict**: CONFIRMED at HIGH. The tier the PLAN designates for these proofs structurally cannot author them.

---

### Finding 2: Contracts harness verifies 1 of 6 templates — CONFIRMED

**Original**: `contract_run` drives only `SatelliteOrbit`; rescue/colony_resupply/survey.data/part_test have no lifecycle exercise anywhere in csp-bench; their predicates are the riskiest code (transition-keyed rescue, colony-delivery sums, survey-data returned-units math).

**Verification**:
- `ls data/contracts/` — exactly 6 templates: satellite, survey, survey.data, rescue, colony_resupply, part_test.
- `contracts.rs:152-249` (`contract_run`) read in full — the only completion path is `if let ContractParams::SatelliteOrbit {..}` at 208-225 (spawn into band). No other `ContractParams` variant is matched anywhere in the crate (grep: `ContractParams::(Rescue|ColonyResupply|SurveyData|PartTest)` → zero bench hits).
- `the_board_respects_the_fact_gate` (contracts.rs:268-283) — asserts only that `contract.survey` offers publish; no lifecycle.
- Predicate claims verified in source: `csp-sim/src/contracts.rs:988-990` `Rescue` matches `ContractTrigger::VesselRecovered(b) if b == body`; 995-1016 `ColonyResupply` sums `facts.colony_deliveries` keyed `(c.id.0, resource)` over colonies on `body`; 1021+ `survey_returned_units` computes returned = collected − pooled − escrowed. Line refs accurate.
- History claim verified: `.review/session-prd1/fix-plan.md:90,117` — "2/6 uncompletable… rescue → W1.3; colony_resupply → W2.3"; `docs/GAMEPLAY-GUIDE.md:336` — "rescue and resupply both have player paths now" (i.e. recently added). The exploration map exists at `.review/session-prd1/exploration-map.md`.
- Honesty note verified: `csp-sim/src/lib.rs:5869-5871` — `apply_spawn_vessel` fires `ContractTrigger::OrbitAchieved` on spawn; `lib.rs:8566-8571` — the auto SOI re-base edge fires the same trigger. The probe does test the real evaluation path.

**Verdict**: CONFIRMED at HIGH.

---

### Finding 3: csp-mcp `session.save`/`session.load` + `with_reconfigure` dead to the suite — CONFIRMED

**Original**: the e2e mission test stops at `session.snapshot`; save/load tools never called by bench tests; the bin's reconfigure hook (Rhai host, autopilot, fixture op-budget reload, balance-failure branch) is dead code to the tests.

**Verification**:
- `csp-server/src/mcp.rs:314-315` — `"session.save" => self.t_save(args)`, `"session.load" => self.t_load(args)`. Registered as claimed.
- `tests/eval_suite.rs:398-503` (`mcp_mission_agent_flies_ascent`) read in full — drives initialize, tools/list, spawn, lease gate, ascent to 40 km, and ends at `session.snapshot` (line 498). Grep for `session.save|session.load|with_reconfigure` across `tests/` and `src/`: zero usages.
- `bin/csp-mcp.rs:80-94` — `with_reconfigure` closure re-installs `script_op_budget` (with the `Err` branch at 85-89 falling back to eprintln + loaded save's budget), `RhaiHost`, `AscentAutopilot`. Reachable only via `session.load` on a `--fixture` run — exactly the untested path.
- PLAN §2.12.1 (line 830) verified: "An eval harness spawns `csp-mcp`; it never spawns the windowed app."
- Parenthetical verified: csp-server has its own test modules (`backend.rs:1033`, `mcp.rs:875`); the *bin glue* is what's untested.

**Verdict**: CONFIRMED at MEDIUM.

---

### Finding 4: `EvalScore` excludes setup-phase funds and time — CONFIRMED

**Original**: `funds_start` is captured after `build_world` drains setup; `sim_time_s` starts from `Lockstep`'s post-setup `start_tick`; the 200 000-tick setup test would score ~0 s.

**Verification**:
- `scenario.rs:627-628` — `let world = file.build_world(...)` then `funds_start = world.funds_micros()`. `build_world` (448-487) drains every `SetupSpec::Command` through `drain_pending` (which `step_frame`s to the next boundary, following partials) and `PadLaunch` via `spawn_pad` (which advances the clock by 1). Setup funds deltas (e.g. a `ManufactureCraft` debit) land before `funds_start` is read.
- `agent.rs:169` — `Lockstep::new` stamps `start_tick = world.clock().ticks()` on the post-setup world; `elapsed_ticks` (249-250) = clock − start_tick. `scenario.rs:719` uses it for `sim_time_s`.
- `tests/eval_suite.rs:114-134` (`setup_advance_to_survives_frame_budget_partials`) — `setup: [Command(AdvanceTo(200_000))]`, asserts `final_tick == 200_001`. For that scenario the score's `sim_time_s` covers only the Idle agent's single boundary (1 tick = 0.025 s) — the report's "reports as 0 s" is a hair imprecise (0.025 s, not exactly 0) but materially correct.
- PLAN §2.12.1 score contract "(Δv used, sim-time elapsed, funds spent)" verified at line 834.
- Latency noted honestly in the report itself: no committed scenario changes funds in setup today; this bites when Finding 1's manufacture scenario lands. MEDIUM is the right severity for a defined-wrong comparison boundary.

**Verdict**: CONFIRMED at MEDIUM.

---

### Finding 5: Colony probe synthetic, not authored content; "plots the currency curves" unimplemented — CONFIRMED

**Original**: `colony_production_run` authors a fake terra/pad/ColonyDef with 21 600 s rotation instead of `data/systems/home` + authored colonies.ron; staffed-base probe has no facilities/upkeep (`StaffAttrition`/`FacilityDormant` don't exist); `EconomyReport::months` never leaves the crate.

**Verification**:
- `economy.rs:201-312` read in full — synthetic `BodyId("terra")` (207), `SiteId("pad")` (233), inline `ColonyDef` generator+drill+store (255-277), `rotation_period_s: Some(21_600.0)` (227). Only `data/balance.ron` is loaded (shipping_balance, 68-71); no system/colonies load.
- The authored content exists and is unexercised: `data/systems/home/colonies.ron` — schema_version 2, Rask mining outpost (solar+battery+drill+converter `recipe.smelt_ore`+stores), header documents the tidally-locked EC balance. Matches the report's description.
- `staffed_base_run` (78-174) — crew salaries only; no facilities/upkeep anywhere.
- Grep `StaffAttrition|FacilityDormant` in csp-sim: zero hits (consistent with the cross-referenced C08 gap).
- PLAN §8.3 (1789-1791) verified: "plots the currency curves" is in the contract. `EconomyReport::months` consumers: grep across crates/apps/xtask finds none outside economy.rs's own tests. Bins confirmed as exactly `csp-eval`, `csp-mcp`, `csp-perf`, `csp-replay` — none renders it.

**Verdict**: CONFIRMED at MEDIUM.

---

### Finding 6: DRY — steering/Rhai/pad-pose/world-wiring duplicated 2–4×; `install_config` exists to prevent it — CONFIRMED

**Original**: gravity-turn math twice in Rust (plus twice in Rhai); Rhai script verbatim in test + .ron; pad pose duplicated in the MCP test; world wiring hand-rolled 3× with different channel subsets; `mark_facts` reinstalls 5 of 7 `install_config` channels.

**Verification**:
- Steering: `eval.rs:109-131` `steer_dir` (DVec3, `pitch_start_m`/`pitch_end_m` fields) vs `eval_suite.rs:516-544` `ascent_steer` ([f64;3]) — same f = (alt−1500)/58500 (test hardcodes the constants), same `pitch = π/2·(1−f)`, same `up·sin + prograde·cos`, same degenerate-fallback intent (and the fallbacks have already diverged slightly: `up×Y→Z` vs `[0, up.z, −up.y]`). Test comment at 513-515 claims "an independent computation" — it is a transcription. Third and fourth copies exist in the two Rhai scripts.
- Rhai: normalized (whitespace-stripped) diff of `scenarios.rs` test `SRC` vs `data/eval-scenarios/script-ascent.ron` — 30/30 lines **identical**. Verbatim duplication confirmed.
- Pad pose: `spawn_pad` (`scenarios.rs:352-374`: `pad = (R+3.5,0,0)`, `rot0 = Y(π/2)`, `vel = body_spin×(pad−center)`) vs `eval_suite.rs:427-441` hand-building the same `SpawnPhysicalVessel`. Confirmed.
- Wiring: `flight_initial` (16 setter/found calls incl. caps/docking/transfer/colonies), `contract_world` (8 setters, tech tree, no physics), `replay.rs:783-821` restore-reinstall (7 data `set_*` + script engine + autopilot). `csp-sim/src/lib.rs:2688` `config()`, `2715` `install_config()` — installs exactly the 7 channels replay.rs hand-rolls; its doc (2706-2711) warns hand-rolled copies "dropped channels (the MCP `session.load` path re-installed 3 of ~10 and silently diverged)". Quote accurate.
- `mark_facts` (`contracts.rs:121-145`) — reinstalls experiments, tech_tree, sites, contracts, recipes = 5 of 7; misses `parts`, `colony_env`. Verified; unused there today, as the report concedes.

**Verdict**: CONFIRMED at MEDIUM (maintainability; the divergence examples are real).

---

### Finding 7: `event_budget` enforced off-by-one, after a whole boundary — CONFIRMED

**Original**: check `run.applied() > file.event_budget` runs after `run.step()`, so up to one boundary batch overshoots; the field doc oversells.

**Verification**: `scenario.rs:682-689` — `run.step()?` then `if run.applied() > file.event_budget { stop }`. Post-step + strict-greater: final `applied` ∈ (budget, budget + last batch]. A `Schedule` boundary carries N commands in one step (ScheduleAgent returns the whole batch), so the overshoot is N, not 1. Field doc at 375-376: "Max applied commands before the loop stops" — does oversell. Cosmetic today; LOW is right.

**Verdict**: CONFIRMED at LOW.

---

### Finding 8: `terrin_circular` hardcoded radius, self-arguing doc, seed-fragile probe — CONFIRMED

**Original**: 600_000.0 baked in (correct today vs terrin/body.ron:16); drawn body ignored for the datum; doc argues with itself; a seed with no satellite offer fails the funds assertion undiagnosed.

**Verification**:
- `contracts.rs:56` — `semi_major_axis_m: 600_000.0 + alt_m`. `data/systems/home/terrin/body.ron:16` — `radius_m: 6.000e5`. Matches today. The function takes only `alt_m`; the call site (218-221) has the drawn `body` but no radius flows from it. (Wording nit: the report says the function "takes the contract's drawn body at the call site" — the call site has it, the function doesn't take it; substance unchanged.)
- Doc comment 49-53 verified: "satisfies both for the authored ranges? No — the midpoint of the drawn band always fits". Self-arguing as quoted.
- Seed chain verified: 187-192 `.map_or(probe.offer, ...)` fallback → accepted contract is not satellite → `if let SatelliteOrbit` (208) skips spawn → `funds_after_complete == funds_after_accept` → 298-301 assertion fails on the live disjunct (the `completed.is_empty()` disjunct is false — the survey fact was injected at 175; line 305 even pins `completed == [(0,1),(1,1)]`). Latent for reseeding/body-list edits; LOW is right.

**Verdict**: CONFIRMED at LOW.

---

### Finding 9: Dead assertion disjunct + unused probe knob — CONFIRMED

**Original**: `|| a.completed.is_empty()` is dead (completed always holds the injected survey fact) and the message claims a discrimination the assert doesn't perform; `band_months` is read only by tests, never by `staffed_base_run`.

**Verification**:
- `contracts.rs:175` injects `(ContractKind::SurveyRun, 1)`; 298-301 is the assert; 305 pins `a.completed == vec![(0, 1), (1, 1)]` — non-empty by construction, so the second disjunct is provably dead and the message "completion paid only when the contract completed" overclaims.
- `economy.rs` — `staffed_base_run` (78-174) reads `probe.staff`, `probe.drain_fraction`, `probe.years`; `probe.band_months` is referenced only at test sites 384-385. Confirmed: a knob the probe ignores.

**Verdict**: CONFIRMED at LOW.

---

## PRD coverage table verification

Spot-verified every row against source/grep; all accurate:
- 3 ascent .ron + 2 plumbing .ron (WIRED row) — confirmed by reading all 5 files.
- Flight arc PARTIAL (golden `flight_run`, not eval tier) — confirmed; no .ron drives the full arc.
- Satellite PARTIAL (probe, spawn-into-band) — confirmed (Finding 2).
- rescue/colony_resupply/survey.data/part_test MISSING — zero bench hits (grep).
- Colony PARTIAL (synthetic) — confirmed (Finding 5).
- Colony resupply delivery MISSING — no `JoinColonyInventory`/`TransferColonyResource`/delivery exercise in bench.
- `ManufactureCraft` MISSING, zero issuers — confirmed (grep; only a comment in replay.rs:808).
- Agent-side save/revert MISSING in bench — confirmed (Finding 3).
- Crew hire PARTIAL (`HireCrew` in economy.rs:102) / board+transfer MISSING (`BoardCrew` lib.rs:825 and `TransferCrew` lib.rs:813 have zero bench issuers) — confirmed.
- Science run MISSING in bench (`RunExperiment` zero hits) — confirmed.
- "Agent completes a mission" PARTIAL (`ascent-mission` is a climb) — confirmed from the .ron's own goals (apoapsis + throttle 0 + InBubble; no mission/contract object).
- Lease/script/warp/log WIRED — confirmed from the .ron files.
- Economy bands/save-size/insolvency WIRED (economy.rs tests, 4 tests read) — confirmed.

## Summary

9 confirmed / 0 dismissed. Both HIGHs (eval tier cannot express §8.5's contract/colony
proofs; contracts harness covers 1 of 6 templates) are structurally verified against
the format, the fixture contents, the registry, and the PLAN text; the MEDIUMs and
LOWs each check out at the cited lines with the claimed mechanics. No false positives
found. Minor imprecisions (Finding 4's "0 s" vs 0.025 s; Finding 8's phrasing of who
"takes" the body) do not affect any verdict.
