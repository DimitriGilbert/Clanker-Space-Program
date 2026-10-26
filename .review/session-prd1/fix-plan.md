# Fix Plan — session-prd1 (PRD-informed review → orchestration-compatible fix plan)

**Synthesized from:** `exploration-map.md`, `review-report-01..24` (23 files; PRD-07 merged into report-06), `STATUS.md` (verification summary — all findings CONFIRMED, zero dismissals), `PLAN.md` R3.

**The verified picture.** The sim implements essentially the full PRD game (~all of PLAN §3) and it is honest, deterministic, and heavily tested. The missing half is the **player surface**: only ~15–17 of 51 `SimCommand` variants are player-reachable; the rest are reachable only through MCP `sim.command` (`csp-server/src/mcp.rs:312`), bench scenarios, or nothing. Every headline gap funnels through three choke points: `UiOutbox` (8 fields — `csp-view/src/lib.rs:305`), `program_command` (6 of 51 variants — `csp-view/src/lib.rs:698`), and `View`'s public method vocabulary.

**Plan convention.** Work is grouped into sequential **phases** P1→P4; inside each phase, **workstreams** (W-x.y) are sized for one implementer. Each workstream lists: requirement (finding + file:line evidence), inputs (files to touch), outputs (deliverables), validation criteria, and dependencies. **File-conflict notes** name the contended files; workstreams that touch the same file are marked sequential. Phases are committed and shipped individually, in order.

**Global gates (run at the end of every workstream, all must pass before commit):**
1. `cargo fmt --check`
2. `cargo clippy --workspace --all-targets -- -D warnings`
3. `cargo check --workspace`
4. `cargo test -p <affected crates>` (and `cargo test --workspace` before the phase ships)
5. `cargo check --workspace --exclude csp-server --exclude csp-bench --exclude xtask --target wasm32-unknown-unknown` (wasm gate — `csp-sim`/`csp-phys`/`csp-data`/`csp-view`/`csp-ui`/`csp-app` must stay wasm-clean)
6. `cargo run -p xtask -- lint-deps` and `cargo run -p xtask -- lint-attribution` when dependencies or `data/`/`assets/` files change
7. Golden replays (`cargo run -p csp-replay -- verify` over the committed suite) whenever `csp-sim`/`csp-phys` behavior changes; **any change to serialized/hashed state requires a `HASH_SCHEMA_VERSION` bump + `xtask bless-goldens` in the same commit** (PLAN §7.1/§8.4). New `SimCommand` variants must be **appended** with new explicit discriminants (append-only rule) and do not by themselves require a hash bump.

---

## Phase 1 — Loop-breaking fixes (make the game completable end-to-end)

**Goal:** a player can launch → reach orbit → run science → land → recover → get paid, and the pad can never trap them. This phase unblocks maneuver nodes, SOI transitions, contracts, transfers, and recovery.

**Sim-file contention note:** `crates/csp-sim/src/lib.rs` is touched by W1.1 (residence), W1.3 (TerminateVessel), and W1.4 (deploy state if sim-side). Assign **one implementer** to all `lib.rs` work in this phase, or land them in the order W1.1 → W1.3 → W1.4.

### W1.1 — Auto `InBubble → OnRails` transition (CRITICAL — the top fix)

- **Severity:** CRITICAL. Reports 16-F2, 03-D1, 05-G1, 01-D1 (3 reviewers agreed).
- **Requirement:** A player vessel must be able to reach `OnRails` without any command. Today `ToRails` is issued only by `csp-bench/src/scenarios.rs:549`, tests, and MCP; `apply_set_maneuver` requires `v.orbit()` (`lib.rs:6018`), the map maneuver panel's vessel list is permanently empty (`csp-view/src/lib.rs:665-669`, `csp-ui/src/lib.rs:1035`), `recover_control` can never fire (`csp-view/src/lib.rs:1917-1997`), `schedule_next_soi` no-ops for bubble vessels (`lib.rs:4729-4740`), and `OrbitAchieved` contract triggers (`contracts.rs:224-226`) can never fire. `balance.bubble_radius_m` (`data/balance.ron:8`, `csp-data/src/balance.rs:67`) is dead data — the §2.3 distance-based bubble model was never built; the implemented model is body-centric unbounded (`bubble.rs:397-406`).
- **Inputs:** `crates/csp-sim/src/lib.rs` (residence check in/after `step_bubble_range` ~L3656-4451; `apply_to_rails_at` ~L5636-5749; `apply_to_bubble` ~L5908-5964), `crates/csp-sim/src/bubble.rs` (`PhysicsConfig`), `data/balance.ron` (consume or document `bubble_radius_m`), `crates/csp-view/src/lib.rs` (optional surfacing notice).
- **Design (chosen — report-03 option (b), the refusal-predicate inversion):** at each `step_bubble_range` boundary (or every K ticks — implementer's choice, bounded cost), for every `InBubble` vessel evaluate the same predicates `apply_to_rails_at` uses, and convert automatically when **all** hold:
  - `throttle == 0` (coasting — a burning vessel correctly stays in physics; §2.6.2 already forces throttle 0 on bubble-exit)
  - no live ground contact / not settle-latched
  - altitude above `atm.top_m`
  - the osculating conic clears the surface *and* the atmosphere — see the companion gate fix below
  A ballistic ascent that cuts throttle in orbit rails within a bounded number of ticks; a suborbital hop fails the periapsis gate, stays in the bubble, and reenters. Surface a one-line notice ("vessel N on rails") via the existing `fired_events`/notice channel so the transition is not silent.
- **Companion fixes in the same file (same implementer):**
  - **Atmosphere-dipping conic gate (report-05 D1):** `apply_to_rails_at` currently requires `r_p > radius_m` (`lib.rs:5684-5693`) but not `r_p > radius_m + atm.top_m`, while `apply_set_maneuver`'s `min_node_r` *does* include `atm_top` (`lib.rs:5994-5997`) — raise the stamp gate to `r_p > radius + atm_top` so a vessel on a reentry conic is never railed.
  - **`apply_to_bubble` clearance/occupancy gates (report-03 D2, HIGH):** it stamps `body.pos` unconditionally (`lib.rs:5908-5964`). Mirror the existing refusal halves: reject `Recoverable` when the materialized hull's lowest point penetrates the terrain datum at the stamp tick, and run `spawn_site_occupied_by` (`lib.rs:5361-5420`)/equivalent against InBubble + latched residents so `recover_control` on a doomed conic refuses instead of spawning underground or ghost-overlapping.
  - **`finish_burns` stale-phase terrain sample (report-03 D4 / 05-D2, low):** `apply_to_rails_at` samples ground contact at `self.clock` (range start) while the conic is stamped at `t_next` (`lib.rs:4450, 5666-5673`). Pass the eval tick (`t_next`) through.
- **Outputs:** coasting vessels above atmosphere auto-rail; maneuver/SOI/transfer/recover-control/orbit-contract layers unlock with no new UI; `ToRails`/`ToBubble`/`SetManeuver`/`SetVesselTracked` become live verbs. Optionally also expose `ToRails` as a view verb for edge cases.
- **Validation:** (a) new sim test: spawn → burn to orbit → cut throttle → assert residence becomes `OnRails` within the check interval; (b) suborbital hop test: periapsis-in-atmosphere conic is *never* railed; (c) `duva_reachable_in_sim`-style path works player-side: rails → `SetManeuver` → executor → SOI transition; (d) `cargo test -p csp-sim`; golden replays green or reblessed (residence transitions change state → expect a `HASH_SCHEMA_VERSION` bump only if the serialized form changes — the transition itself is command-log-consistent since the auto-conversion must emit through the same `apply_to_rails_at` path; **decide and document whether the auto-conversion is logged as a `ToRails` command or an event — determinism requires it be part of the command/event stream, not ambient state**).
- **Parallel:** conflicts with W1.3/W1.4 on `lib.rs` (sequential or same implementer). Disjoint from W1.2, W1.5.

### W1.2 — Science drops in space: fix `science_ctx` staging (CRITICAL-adjacent HIGH)

- **Severity:** HIGH (primary science loop is a dead button in orbit). Reports 16-F1, 15-F2, 02-D2.
- **Requirement:** `stage_program_vessel` derives `ctx_biome` only for surface-side situations (`csp-view/src/lib.rs:2603-2607`), so `science_ctx = ctx_biome.zip(ctx_situation)` (lib.rs:2540) is `None` for `SpaceLow`/`SpaceHigh`; `program_command` then drops `RunExperiment` silently (lib.rs:711-719) while the panel's `runnable` flag still shows enabled (lib.rs:2640-2653). The sim accepts an **empty** `BiomeId` for space (`research.rs:127-129`, test at `research.rs:1220`). The only shipped instrument (`csp.science.canister.ron:48`, mask 49 = Landed|SpaceLow|SpaceHigh) is built exactly for these contexts.
- **Inputs:** `crates/csp-view/src/lib.rs` (L2540, L2603-2607, L711-719), `crates/csp-ui/src/program.rs` (~L400-431 for button gating/hover).
- **Outputs:** stage the context on the *situation*: `science_ctx: ctx_situation.map(|s| (ctx_biome.clone().unwrap_or_default(), s))` (empty `BiomeId` for space, matching the sim convention). Push a `flight_notice` when `program_command` drops a command on `None` so a stale mirror can never produce a dead click again (the drop rule itself is correct for stale frames — the fix is wrong content + silent channel). Gate/hover-state the run button on `science_ctx.is_some()` as defense-in-depth.
- **Validation:** view-level test feeding a `SpaceLow` mirror through `program_command` asserts a `RunExperiment` reaches the outbox (the test report-16 §"coverage note" says this class of test would have caught it); manual: pod+canister in orbit → run → data appears in pooled rows.
- **Parallel:** disjoint files; independent.

### W1.3 — `RecoverVessel` player verb + new `SimCommand::TerminateVessel` (HIGH)

- **Severity:** HIGH — pad trap (seeded starter craft blocks second launches; refusal text names a verb that doesn't exist, `csp-view/src/lib.rs:2411-2418`), dead rescue contracts (`contracts.rs:979-981` needs `VesselRecovered`), dead recovery economy (`economy.rs:867-986` fully implemented, zero producers). PLAN:1228 mandates `TerminateVessel`; **the variant does not exist in the enum**.
- **Requirement:** `apply_recover_vessel` is complete (resource credit to named site at `recovery_efficiency`, part-cost payout, crew return, science banking, `VesselRecovered` trigger). `remove_vessel` exists (`lib.rs` ~L4566+). Add:
  - `SimCommand::TerminateVessel { vessel }` — **appended variant, new discriminant**. Apply: refuse on lease-held/maneuver-active states as appropriate; mark aboard-crew `Lost` (or return-to-roster when `SurfaceLatched` — pick the documented rule); scrub pending vessel events via `event_names_vessel`; free the pad berth. For debris/hulk cleanup per §3.5 object caps.
  - Player verb: `ProgramCommand::RecoverVessel` (+ optionally `TerminateVessel`) on `SurfaceLatched` vessels. `ProgramMirror` already stages `input.latched` (lib.rs:2562) and the sites table is installed (lib.rs:1173-1181); the recovery site (`terrin-downrange`, `data/systems/home/sites.ron`) can be selected or defaulted to the launch site.
  - HUD/map affordance to recover a latched focused vessel (secondary; the Program-window path is the required one).
  - Softened occupancy text is no longer needed once the verb exists — verify the refusal now names a verb that works.
- **Inputs:** `csp-sim/src/lib.rs` (enum + dispatch + apply), `csp-sim/src/economy.rs` (recover apply already exists), `csp-sim/src/contracts.rs` (no change — trigger already wired), `csp-view/src/lib.rs` (mirror + `program_command`), `csp-ui/src/program.rs` (button + gating), `crates/csp-ui/src/lib.rs` (HUD affordance optional).
- **Outputs:** pad-trap closed; `contract.rescue` completable; recovery-funds leg live; `TerminateVessel` gives debris/hulk removal per PLAN.
- **Validation:** sim test: `TerminateVessel` removes a latched hulk + unblocks next `SpawnPhysicalVessel` at the site; view test: latched vessel → `ProgramCommand::RecoverVessel` → funds credit + `VesselRecovered` trigger; `contract.rescue` completes in sim. New-variant discriminant appended (check `#[repr(u16)]`/`StableDiscriminant` table).
- **Parallel:** `lib.rs` contention with W1.1 — same implementer or sequential. UI half (`csp-ui`, `csp-view` mirror) can proceed in parallel with the sim half under an agreed variant signature.

### W1.4 — Parachute/decelerator part (CRITICAL for the recovery loop)

- **Severity:** HIGH → loop-breaking. Reports 23-F1, 22-F5/F5-table, 06-correction. `RecoverData` requires `SurfaceLatched` (`research.rs:855`) and no survivable Terrin reentry exists — a pod reentering at orbital velocity cannot shed ~1.5 km/s with `max_temp_k` 1200 K and no decelerator. Science is transmit-only (40%); the economy can't close (report-23: transmit-only ceiling ≈92 science < 105 node cost).
- **Requirement (schema + phys + sim + data):**
  - Schema: add a decelerator variant to `ModuleDef` (`csp-data/src/part.rs` ~L506-624). Recommended shape: `ModuleDef::Parachute { deploy_alt_m, deploy_speed_mps, packed_drag_area_m2, deployed_drag_area_m2, deploy_time_s }`. Validator rules (sane ranges, finite, category compatibility). This also gives `PanelJam` a real subject (reports 23-F7, 22-F5).
  - Resolve: carry a parachute spec through `resolve.rs` into `WeldPart` (`csp-phys/src/part.rs:220-303` — add field).
  - Sim: per-node `deployed` state on the vessel (**serialized + hashed → `HASH_SCHEMA_VERSION` bump + rebless goldens**); a deploy path — recommend a new appended command `DeployPart { vessel, node }` plus auto-deploy when `alt < deploy_alt_m && speed < deploy_speed_mps` if desired; deployed parachute contributes analytic drag at the node position in the force accumulation (this does *not* require aero-table recomputation — additive force term); `PanelJam` failure mode blocks deployment.
  - Content: author `data/parts/csp.chute.s125.ron` (Utility/Structure category, tech.start or tech.rocketry.2 gate so the early loop works), loc entries in `data/loc/en.ron`, demo-set copy under `apps/demo/data/parts/` (keep the demo/subset rule consistent — decide whether chute ships in the 60-part-cap demo; if yes, mirror it).
  - Bonus cheap content (data-only, recommended in the same stream): static landing legs `csp.legs.s125` (structure part — a leg is just a strong low collider, no module needed, report-22 ledger), `csp.dockport.s125` (pure data gap — schema/sim/validator all live, unblocks `Undock` and the M6 content path), `csp.lab.mk1` (needed by W2.2 anyway), `csp.probe.s0625` (Control-only probe core for the satellite contract — data-only per report-22).
- **Inputs:** `csp-data/src/part.rs`, `csp-data/src/validate.rs`, `csp-phys/src/{part.rs,resolve.rs}`, `csp-sim/src/lib.rs` + a module for the deploy/drag (force accumulation site — implementer picks: additive drag term in the per-part force pass, or a `ForceSite` contribution), `data/parts/`, `data/loc/en.ron`, `apps/demo/data/`, golden fixtures.
- **Outputs:** survivable reentry exists; `RecoverData` becomes a real path (100% + key closure); `contract.rescue` has a physical completion route; `PanelJam` has a subject.
- **Validation:** bench/eval scenario or sim test: pod+chute+canister deorbits from ~100 km, deploys, settles to `SurfaceLatched` intact (thermal + touchdown speed within part limits) → `RecoverData` banks 100%. Validator tests for the new variant. `cargo test -p csp-data -p csp-phys -p csp-sim`; wasm check; `HASH_SCHEMA_VERSION` bump + `xtask bless-goldens` in the same commit; `lint-attribution` for any new asset files.
- **Parallel:** mostly disjoint (schema/phys chain + data) — `lib.rs` edits for the deploy apply share the file-contention note; coordinate with W1.1/W1.3 implementer.

### W1.5 — Loop-honesty quick wins (batch; mostly disjoint files)

Bundle of small, independent fixes that make the *working* loop discoverable and honest. One implementer can take the batch; files are disjoint.

| # | Requirement (evidence) | Files | Output | Validation |
|---|---|---|---|---|
| a | `save_seed` never set — every game draws identical offers + identical failure draws (`csp-view/src/lib.rs:1073-1241` `from_source` missing `set_save_seed`; default 0 at `csp-sim/src/lib.rs:2445`) | `csp-view/src/lib.rs` | `sim.set_save_seed(<entropy>)` at `from_source` (native entropy; fixed constant for the §6.3 demo); seed surfaced in bug-report | new-game offers differ across two runs; demo stays reproducible |
| b | Starter craft can't do science: `STARTER_CRAFT` = pod+s160+sparc, no canister/battery (`csp-view/src/demo.rs:38`); tutorial never mentions science (`csp-ui/src/tutorial.rs:70-108`); no `KeyAction` for Program | `csp-view/src/demo.rs`, `csp-ui/src/{tutorial.rs,settings.rs}`, `csp-view/src/tutorial.rs` | starter gains canister + battery; a second tutorial tier ("first science": run → transmit/recover → research); `OpenProgram` KeyAction + default binding + finish-card pointer; labeled/hoverable status strip (`program.rs:212-226`) | tutorial step predicates real (`step_complete` on `TutorialInput`); canister runs on pad in a fresh boot |
| c | `SetVesselTracked` has no producer → every `SoiTransition` stays `Silent`; warp never clamps for encounters (`csp-sim/src/lib.rs:4845-4866`, `events.rs:449-457`, `csp-app/src/lib.rs:931-934`) | `csp-view/src/lib.rs` or `csp-app/src/lib.rs` | issue `SetVesselTracked{tracked:true}` for the flight-focus vessel on focus acquisition (clear on change) — simplest; alternative: promote focused vessel in `class_of` | sim test: focused vessel's `SoiTransition` classifies `Clamping` |
| d | Contracts accept-blind + 2/6 uncompletable + deadline penalty trap (`csp-view/src/lib.rs:2939-2949`, `program.rs:262-276`, `contracts.rs:1145-1182`; `contract.rescue`/`colony_resupply` uncompletable until W1.3/W2.3) | `csp-view/src/lib.rs`, `csp-ui/src/program.rs`, `data/contracts/` or generator | (interim) render `ContractParams` → kind-specific summary (~60-line match) + reward/deadline/days-remaining on offers and active; (interim honesty) filter generated offers to completable kinds OR gate by reachable-body/reachable-verb until W2.x lands | board shows param/reward/deadline; no guaranteed-penalty offers appear before their verbs ship |
| e | Transmit EC-gated with zero disclosure (`research.rs:797-803`; `DataRow` has no EC fields `program.rs:78-93`; starter has no battery) | `csp-view/src/lib.rs` (stage EC), `csp-ui/src/program.rs` | stage vessel EC + per-row EC cost in `DataRow`; transmit disabled-with-hover "needs N EC (have M)"; blurb note | transmit button gates correctly on a zero-EC craft |
| f | `apply_bench_test` returns `Fatal` on bad config where every sibling uses `Recoverable` (`reliability.rs:1137-1138`) | `csp-sim/src/reliability.rs` | `SimError::Recoverable("bench test: invalid config")` | malformed config rejects on channel, not world halt |
| g | `AdvanceReport::partial` ignored by app — §2.6.5 back-pressure unhooked (`csp-app/src/lib.rs:924-985` reads `fired_events`/`rejected`/`degraded`, never `partial`/`effective_target`) | `csp-app/src/lib.rs`, `csp-app/src/warp.rs` | on `partial`, demote warp toward the largest fitting rung + `WARP LIMITED — EVENT BACKLOG` cue | synthetic event-storm frame demotes warp; displayed warp tracks achieved rate |
| h | Fired notification-class events have no display consumer — `PlayerAlarm`/`TransferWindow`/`ContractDeadline`/`ConstructionComplete`/`Insolvent` vanish (`lib.rs` `fired_events` ~L7457; `csp-app/lib.rs:931-934` reads only `class==Clamping`) | `csp-app/src/lib.rs` (+ `csp-ui` notice channel) | drain `report.fired_events` into the flight-notice channel with per-kind formatting (label, deadline name) — bounded | a fired `PlayerAlarm` prints its label on the HUD cue |
| i | `science` vs `banked` dead distinction + LOG is reliability-only (`program.rs:128-131, 489-498`; `NotificationKind` has 2 variants, `reliability.rs:383-397`) | `csp-sim/src/reliability.rs` (+ `csp-view`/`csp-ui` rows) | extend `NotificationKind` with contract/science/economy event kinds (accept/complete/fail/deadline, science banked, funds deltas) emitted at the existing notify sites — serialized-unhashed ring, safe | contract accept/complete/deadline appear in Program LOG |

**Phase-1 exit criteria:** `cargo test --workspace` green; player-shaped loop demonstrable headlessly: launch → auto-rails → maneuver → orbit contract completes; canister runs in space; latched vessel recovers or terminates; chute survives reentry. Commit + ship.

---

## Phase 2 — Player surface (make the implemented game reachable)

**Goal:** the ~28 unreachable-but-implemented commands get honest UI. The sim side is done; this phase is mirror → outbox → panels. Also ships the content (lab, colony visibility, crew) that makes the sim's second half real.

**Foundation workstream W2.0 lands first** — it defines the plumbing every later panel uses. Then W2.1–W2.6 run in parallel where files are disjoint (see conflict map).

### W2.0 — Mirror/outbox/command plumbing foundation (do first)

- **Requirement:** `UiOutbox` (lib.rs:305-331) has 8 fields and no colony/crew/alarm/save/script/transfer slot; `program_command` (lib.rs:692-731) handles 6 of 51 variants; `ProgramInput`/`ProgramCommand` (`csp-ui/src/program.rs:123-195`) is the only progression surface; `ProgramMirror` (lib.rs:224-233) doesn't mirror colonies/crew/labs/alarms/scripts.
- **Inputs:** `csp-view/src/lib.rs` (mirror structs, `stage_program` ~L2485-3003, `program_command`, `UiOutbox`), `csp-ui/src/program.rs` (`ProgramInput`, `ProgramCommand`, row types).
- **Outputs:** agreed extension contract for all W2.x: new `ProgramCommand` variants (RecoverVessel, HireCrew, BoardCrew, TransferCrew, StartResearch, StopResearch, SetAlarm, WatchTransferWindow, LoadScript/UnloadScript/SetScriptEnabled, EngageAutopilot/DisengageAutopilot, JoinColonyInventory/LeaveColonyInventory, TransferColonyResource, TransferColonyCrew, FoundColony, AbandonColony, AssignCrewToColony, EvacuateColony, ScheduleColonyInspection, RunBenchTest, TerminateVessel, TransferResource, ResourceTransfer-to-site as needed); new `ProgramInput` sections (colonies, roster, lab projects, alarms, scripts, autopilot, vessels-in-program, site inventories); corresponding `ProgramMirror` staging in `stage_program` (query `sim.colonies()`, `economy` roster, `science.projects`, `scripts`/`autopilots` arenas, pending alarm/watch events). Keep the "stale-frame commands drop, don't misfire" rule (lib.rs:695-701) and the drop-notice from W1.2.
- **Validation:** mirror fields populate from a seeded world (colony + roster + projects appear); every new `ProgramCommand` maps to a `SimCommand` enqueue through the existing `SourceClass::Ui` FIFO; `cargo test -p csp-view -p csp-ui`.
- **Parallel:** prerequisite for W2.1–W2.6 (interface contract). The mirror side (`csp-view`) and the panel-side (`csp-ui`) struct definitions can be edited in parallel once the field list is agreed in this workstream.

### W2.1 — Contracts UI (HIGH)

- **Requirement:** offers render as `"#idx template-id"` only (`csp-view/src/lib.rs:2939-2949`); `OfferRow` (`program.rs:46-52, 262-276`) has no reward/deadline/params; active rows show no progress/countdown; deadline expiry silently debits (`contracts.rs:1145-1182`); contract events produce no player-visible record (W1.5-i widens `NotificationKind`). Two templates were uncompletable pre-W1 (rescue → W1.3; colony_resupply → W2.3).
- **Inputs:** `csp-ui/src/program.rs`, `csp-view/src/lib.rs` (staging), `data/contracts/*.ron` (loc descriptions already exist), `csp-sim/src/contracts.rs` (only if a progress field needs mirroring).
- **Outputs:** offer rows with template description (loc), param summary per kind (orbit band/body/min units/amount/target), advance+completion rewards, deadline (tick + days); active rows with progress predicate summary + days-remaining countdown; LOG entries for accept/complete/fail/deadline (via W1.5-i); remove the interim offer filter once the underlying verbs ship (rescue works post-W1.3; resupply works post-W2.3 — keep the gate for anything still uncompletable).
- **Validation:** every shipped template renders a full row; `contract.satellite` completes after W1.1 (orbit achievable); rescue completes post-W1.3; a deadline shows a countdown before it debits.

### W2.2 — Crew, labs, and research UI + missing parts (HIGH)

- **Requirement:** three stacked dead layers (report-06 H2): no `Lab` part (resolver/sim fully support it — `research.rs:943-959` rejects "node carries no lab" forever); `BoardCrew` mints orphan `CrewId`s with no roster record (`transfers.rs:999-1011` — not the §3.5 hire→board flow); `HireCrew` (`economy.rs:800-839`) unreachable; `SalaryDue` never fires (`economy.rs:727-736`); `SetLifeSupportEnabled` is a setting that can never consume anything. `facility.lab`/`facility.test_stand` tech unlocks (`data/techtree/home.ron:33,67`) have no payload.
- **Inputs:** `data/parts/` (author `csp.lab.mk1`: `Lab{throughput_data_per_s, multiplier>1, researcher_slots:1-2}` + `CrewSeat`; `csp.tank.life.s` LS provisions — data-only per report-22), `data/techtree/home.ron` (give `tech.science.1` a real part payload), `csp-sim/src/transfers.rs` (BoardCrew roster validation — refuse or resolve against roster instead of minting orphans), `csp-ui/src/program.rs`, `csp-view/src/lib.rs`.
- **Outputs:** roster section in Program window (hire button w/ cost, member list w/ kind/skill/location/salary); board/transfer rows on the focused vessel (seat-gated by `CrewSeat` on `csp.pod.mk1`); `StartResearch`/`StopResearch` controls on a lab-carrying vessel (data escrow + accrual rows); LS provisioning path exists in content; `BoardCrew` validates against the roster.
- **Validation:** sim+view tests: hire → board → scientist aboard lab vessel → `StartResearch` accrues → `ResearchProjectComplete`/`ResearchDataExhausted` fire (both `Clamping`, correctly) → science lands; `SalaryDue` publishes once an `Active` member exists; `cargo test -p csp-sim`.
- **Parallel:** data files disjoint from sim/UI; `transfers.rs` sim edit is small and module-local.

### W2.3 — Colonies and logistics UI (HIGH)

- **Requirement:** 5,168-line colony sim + authored Rask colony (`data/systems/home/colonies.ron`, founded at boot `csp-view/src/lib.rs:1218`) are invisible — no panel, no map marker, no mirror fields (reports 16-F5, 06-H3, 14-F4). `contract.colony_resupply` needs `TransferComplete` into colony stores (`contracts.rs:988-1007`, `transfers.rs:1047-1064`), which needs `JoinColonyInventory`/`TransferColonyResource` on a latched-in-radius vessel (`transfer_radius_m` 100 m) — none reachable. Colony inspections never fire (`colonies.rs:1697` `inspection_tick` None; `ScheduleColonyInspection` no UI).
- **Inputs:** `csp-view/src/lib.rs` (mirror/staging), `csp-ui/src/program.rs` (colony section) or a dedicated colony panel, `csp-view/src/map.rs` (colony markers — `SiteTable`-parallel `ColonyTable`, `map.rs:894-912` site-marker pattern; also latched-vessel markers + active-vessel/debris distinction per report-14 F4), `csp-sim/src/colonies.rs` (only if read accessors needed — `sim.colonies()` exists), `csp-sim/src/transfers.rs`.
- **Outputs:** colony section/panel: per-colony state (`Nominal/Degraded/Dormant/Abandoned` + reason), body/site, module list, store levels, power day/night state; verbs: `ScheduleColonyInspection`, `AbandonColony`, `AssignCrewToColony`, `EvacuateColony`; vessel-side verbs when latched within radius: `JoinColonyInventory`/`LeaveColonyInventory`, `TransferColonyResource`, `TransferColonyCrew`; `FoundColony` affordance at a site (or documented deferred if founding rules are thin); map markers for colonies and latched vessels; active-vessel marker distinction.
- **Validation:** the Rask colony is visible with live store levels advancing under warp; a latched vessel within 100 m joins inventory and a resource transfer publishes `TransferComplete` → `contract.colony_resupply` completes in sim.
- **Parallel:** `map.rs` marker work is disjoint from `program.rs` panel work — two implementers OK.

### W2.4 — Alarms, transfer planner, encounter prediction (HIGH — the KAC row, PLAN §4 v1/M5)

- **Requirement:** `SetAlarm`/`WatchTransferWindow` unreachable (lib.rs:5204-5257); `plan_transfer`/`porkchop_scan`/`departure_node`/`predict_encounter` callable only by tests (`transfer.rs:1007+`, `lib.rs:16466-16860`); fired `PlayerAlarm`/`TransferWindow` produce no display (W1.5-h adds the notice feed); no SOI visuals or encounter display on map (`map.rs` — `predict_encounter` never called, `refresh_node_previews` ~1070-1098 draws only the same-frame post-burn conic); `maneuver_panel` is bare (`csp-ui/lib.rs:1027-1090`) and invisible until W1.1 populates on-rails vessels.
- **Inputs:** `csp-view/src/map.rs` (+ `csp-ui/src/lib.rs` maneuver/alarm panels), `csp-view/src/lib.rs` (outbox/mirror for alarms+watch), `csp-sim/src/transfer.rs` (read-only use of planner fns), `csp-sim/src/events.rs`.
- **Outputs:** ALARMS section (list pending `PlayerAlarm`/`TransferWindow` heap entries w/ tick+label; `SetAlarm` create/clear); "next window" transfer affordance: destination picker (reuse `MapPick`/`BodyTable`) → `plan_transfer` → offer `SetManeuver` (departure node) + `WatchTransferWindow`; `predict_encounter` on the predicted post-node conic → labeled `Enter`/`Exit` markers ("Duva encounter Pe 120 km, v∞ …", `approach.impacts` warning tint) + optional rebased-conic polyline; vessel-eligibility honesty in the picker ("in physics bubble — coast to rails first"; dominant-parent note per report-14 F5); node label gains burn-time estimate + `Burning` status (`dv_remaining` is staged, `map.rs:838`); optional `presentation_tick` lerp for map positions at high warp (report-14 F2 — or defer with a comment).
- **Validation:** with W1.1 landed: focused vessel on rails → panel lists it → set node → predicted conic draws + encounter marker for a Duva transfer; alarm fires at tick and posts its label; `WatchTransferWindow` demotes warp at the window.
- **Parallel:** `map.rs` vs `program.rs` disjoint.

### W2.5 — Automation UI + sim-side automation fixes (HIGH — pillar 4)

- **Requirement:** `RhaiHost` + `AscentAutopilot` installed (`csp-view/src/lib.rs:1124-1126`) but `LoadScript`/`UnloadScript`/`SetScriptEnabled`/`EngageAutopilot`/`DisengageAutopilot` have no producer (report-18 F1). Sim-side defects: `LoadScript` on OnRails/SurfaceLatched applies "successfully" and never executes (zombie — lib.rs:7286-7320; wakes register only from inside `on_tick`); `EngageAutopilot` on a latched vessel deadlocks (lib.rs:4999-5036 apply + 7102-7109 in-bubble filter); `script_log` has zero consumers outside `csp-bench/agent.rs:455`; `rails_warp_refusal` naming a polling script is stderr-only; strike-disable is invisible. Wake coverage: no `AtAltitude` variant (§2.10 promised "altitude crossing on rails"; `scripts.rs:294-314`); `wait_ticks` semantically vacuous in-bubble; refused wake → strike is harsh (`lib.rs:7169-7195`); `prepare` doesn't check `on_tick` arity (`csp-script/lib.rs:936-946`); `telemetry_of` unbound; `VesselTelemetry` lacks resources/part-graph/vessel-list (scripts.rs:328-382); `mission.*` service absent (§2.10:764-766).
- **Inputs:** `csp-ui/src/{lib.rs,program.rs}` or new console panel, `csp-view/src/lib.rs`, `csp-app/src/lib.rs` (script_log drain → notices), `csp-sim/src/{lib.rs,scripts.rs}` (bootstrap/gates/wake), `csp-script/src/lib.rs` (bindings + arity check + telemetry fields + `mission.ascend`), `csp-ai/src/lib.rs` (optional `AutopilotProgram` extension — see deferred note).
- **Outputs:** script console (load .rhai source/file per focused vessel, enable/disable/unload, strikes + `disabled_reason` + `wake_tick` display, `script_log` tail); autopilot engage control on flight HUD (Ascent + target apo field; `DisengageAutopilot`); `report.script_log` drained into flight notices (the `COMMAND REFUSED` precedent at `csp-app/lib.rs:950-955`); `rails_warp_refusal` strings as persistent HUD notices.
  - **Sim fixes:** `LoadScript` schedules an initial `ScriptWake` at `clock+1` so a rails/latched-loaded script runs `on_tick` once and can arm its real wake (report-18 F3 option b); `EngageAutopilot` either rejects non-bubble vessels `Recoverable` or lets the first tick issue the unlatching `SetThrottle` (pick and document); refused wake registration → non-strike diagnostic; `AtAltitude { above_m, ascending }` wake variant resolved against the rails conic; `prepare` asserts `params == 1`; consider `wait_ticks(n)` suppressing in-bubble ticks until elapsed (spec it or document the current semantic).
  - **ControlApi additions (batch — additive, still pre-freeze):** `telemetry_of`/`vessels`, resource levels + per-part graph listing (aimable `stage`/`jettison`/`transfer`), alarm cancel/list, `mission.ascend(target_apo, incl)` calling the installed driver, `run_experiment`/`transmit_data`/`recover_data` verbs so scripts can do science (report-18 F4 table).
- **Validation:** script loaded on an on-rails vessel runs its first tick and arms an apsis wake (the §2.10 canonical scenario works); autopilot engaged pre-launch flies the ascent; a polling script demotes warp AND posts a named notice; `api_freeze.rs` updated for added surface (it pins surface-by-source).
- **Parallel:** `csp-script`/`csp-ai`/`csp-sim/scripts.rs` mostly disjoint from `csp-ui`/`csp-view` panel work; `csp-sim/src/lib.rs` edits here (wake bootstrap, autopilot gate) should be batched with any remaining `lib.rs` sim work to avoid re-contention — sequence after W1.x sim workstream closes.

### W2.6 — Reliability surface + manufacture pipeline (MED)

- **Requirement:** `RunBenchTest` unreachable; `facility.test_stand` unlock inert; qualification progress invisible; dormant failures only visible inside Program LOG (report-08 L3); mission confidence readout exists in VAB (`vab.rs:2804-2815`) — keep. **Manufacture:** launch bypasses `ManufactureCraft` (`csp-view/src/lib.rs:2210-2220` enqueues `SpawnPhysicalVessel`); funds ARE charged at spawn (`lib.rs:5495-5500,5582` — correct the record: charge is real; what's missing is site-inventory BOM + build time); site inventories start empty (`economy.rs:648-658`, `sites.ron` no starting stock) and nothing writes them (recovery — W1.3 — and `AbandonColony` salvage are the only writers; `TransferEnd` is `Vessel|Colony` only, `transfers.rs:200-206`); `build_resources` on every part def is dead data.
- **Inputs:** `csp-view/src/lib.rs` (`launch_craft` ~L2047-2230, `StagePlan` — already tolerant of late materialization via `(min_id, fingerprint)`), `csp-sim/src/manufacture.rs` (`apply_manufacture_craft` :111-248, delivery :255-340), `data/systems/home/sites.ron` (starting stock), `csp-ui/src/program.rs` (facilities/bench section).
- **Outputs:** (a) facilities/bench section: `RunBenchTest` per family/row (funds+inventory charge + `BenchTestComplete`), qualification display, dormant-failure HUD cue for the focused vessel; (b) route `launch_craft` through `ManufactureCraft` — order → build time → `ConstructionComplete` delivers `SurfaceLatched` vessel → throttle/unlatch to fly; seed `terrin-launch` site inventory with a starting stockpile; `ConstructionComplete` surfaces (W1.5-h feed) + "build order in progress" state; **fallback if scoped out:** keep `SpawnPhysicalVessel` but debit `build_resources` from site inventory in `apply_spawn_physical` and strip/doc-mark `build_resources` honesty.
- **Validation:** launch → order → `ConstructionComplete` at the build-time tick → vessel materializes latched → preflight/berth semantics preserved; funds + site inventory both debited; bench test runs and posts completion; `cargo test -p csp-sim -p csp-view`.
- **Parallel:** `csp-view` launch path contends with W2.0's lib.rs edits — sequence after W2.0 or same implementer.

**Phase-2 exit criteria:** every `SimCommand` that has a real gameplay path has a UI producer or an explicit `ui-unreachable:` annotation (see W4.5 reachability lint); `cargo test --workspace` green; the career arc is playable: science → tech → contracts → crew/lab → colony resupply → manufacture. Commit + ship.

---

## Phase 3 — VAB/editor fixes (user complaint #2)

**File contention:** `csp-phys/src/session.rs` is shared by W3.2/W3.3/W3.4 — one implementer or sequential. `csp-ui/src/vab.rs` is shared by everything below — batch or same implementer per wave.

### W3.1 — Hangar floor occludes the bottom cap (HIGH — "can't put shit below")

- **Requirement:** `emit_environment` (`csp-view/src/vab.rs:833-1012`, emitted at :779) draws floor disc + contact blob + grid into the **same vertex stream the GPU pick pass rasterizes** (`csp-render/src/vab.rs:320-356`, `Greater` depth-write at :141-146). From beneath the craft the floor wins depth in color AND pick → bottom cap invisible and unclickable (`Pick(None)` → `Select(None)`). Pitch clamp was already fixed (commit `396e12c`); the occluder came later.
- **Inputs:** `csp-view/src/vab.rs`, `csp-render/src/{vab.rs,vab.wgsl}`.
- **Outputs:** exclude environment verts from the pick pass (split env/part streams or tag verts and discard in `fs_pick`); make the floor non-occluding for parts (depth-write off / bias) and/or lower `floor_z` by a real margin (~`extent·0.5` below `min_z`, not 2%).
- **Validation:** camera pitched under the craft → bottom cap visible and clickable; a part attaches below the stack via the canonical gesture; `vab_latency` test still green.

### W3.2 — Undo stack (HIGH — "the editor ate my rocket")

- **Requirement:** `remove_part` deletes the whole subtree (`session.rs:1304-1316`; root removal wipes the session) and there is **no undo anywhere** (grep `undo|redo` = zero). Three instant-delete gestures: right-click (`vab.rs:2143-2148`), Del/Backspace (:2156-2158), drag-to-palette (:2120-2125).
- **Inputs:** `csp-phys/src/session.rs` (option: `remove_part` returns removed records or a `detach_subtree` variant), `csp-ui/src/vab.rs` (undo stack + keybinds).
- **Outputs:** bounded undo stack of `CraftSession` clones captured in `VabSession::apply` before mutating arms (all ops funnel through `apply`); Ctrl+Z / Ctrl+Y (or Ctrl+Shift+Z); subtree-size confirmation (e.g. warn when `removed.len() > 1`); optionally `remove_and_reparent` (children re-edge to the removed node's parent, resnapped) as an alternate delete mode.
- **Validation:** delete mid-stack tank → subtree gone → Ctrl+Z restores bit-identical session (weld graph equal); rapid delete/undo cycles don't corrupt staging.

### W3.3 — Root move / dead gestures / whole-craft translate (HIGH — "can't move the rocket")

- **Requirement:** root `grab` refused ("the craft root can't be picked up", `vab.rs:1081-1102`); drag-on-root is a fully dead gesture — `orbit_block` set on any part press (`vab.rs:2101-2103`) blocks camera orbit while refused `Grab` re-fires every frame (`vab.rs:2108-2114`); no translate exists in `VabAction` (`vab.rs:152-259`); `CraftSession::set_transform` (`session.rs:1441-1455`) is test-only API with zero production callers; `reattach`-on-root reports misleading `WouldCycle` (`session.rs:1333`, report-10 L4).
- **Inputs:** `csp-ui/src/vab.rs`, `csp-view/src/vab.rs` (`apply_vab_input` :165), `csp-phys/src/session.rs`.
- **Outputs:** a whole-craft move path — either root-grab lifts the entire craft (no-op reattach on drop is already the semantic) or a `VabAction::Translate` op through `set_transform` (offset/rotate/nudge tool for placed parts too); `orbit_block` driven by the press anchor for the gesture's life and released when grab is refused (drag falls through to camera orbit); rate-limit the refusal notice; typed `RootImmovable`-style error for programmatic root moves instead of `WouldCycle`.
- **Validation:** LMB-drag on the root either moves the craft or orbits the camera — never goes dead; a translate op moves the whole stack and welds bit-identically modulo pose; notice spam gone.

### W3.4 — Attach/remove correctness batch (MED)

| # | Requirement (evidence) | Fix |
|---|---|---|
| a | Empty-session LAUNCH silently spawns hidden stock craft (`csp-view/src/lib.rs:2069-2071`) | Refuse with `EmptySession` message (`session.rs:1755` exists); stock craft behind demo/debug flag |
| b | Axial plan can land on the opposite face from the click (`vab.rs:1425-1466`, `side_match` is only a sort preference) | Restrict axial candidates to the clicked side on cap hits; existing "no free attach node on that side" error otherwise |
| c | No overlap checking anywhere (`session.rs:1748-1832` validate has no geometric check; `plan_radial` exact-float `surf:` ids `vab.rs:1551`) | Quantized surf-position occupancy (~5 cm cells) and/or AABB-overlap warning in `validate` reusing weld AABBs |
| d | One-node-many-children unenforced in the model — `bound_attach_nodes` exists but `add_part`/`reattach` don't consult it (`session.rs:1053-1067`); double-binding welds coincident parts; reachable by any non-VAB writer (MCP/crafted file) | Typed error in `add_part`/`reattach` (or `SessionWarning::AttachNodeOccupied` in validate) |
| e | `surf:` namespace collides with authored node ids; decode shadows authored nodes and accepts unbounded geometry (`session.rs:737-756,773-784`); validator doesn't reserve prefix | Reserve `surf:` in def validator; prefer authored nodes on verbatim match; bound decoded norms to plausible part scales |
| f | `ATTACH_DIAMETER_REL_TOLERANCE = 1e-6` makes pre-scaled placement impossible while attach-then-rescale bypasses the same rule (`session.rs:151`, `:1504-1509`) | Pick one semantic: widen gate to size-class band (~5-10%) or enforce diameter check in `resnap` too |
| g | VAB hotkeys bypass the rebindable keymap (`vab.rs:2054-2066`); stage DragValue caps at 64 vs `MAX_STAGE_INDEX = 999` (`vab.rs:3021`, `session.rs:182`) | Route through `KeyAction` or document as VAB-local; fix the cap |
| h | `joint_loads` torque arm evaluated at part origin, not attach node (`structural.rs:210`) | Record the snapped joint point at attach time (`snap_transform` knows it) and carry it on `PartNode`/`WeldPart` — or defer w/ tracking note |
| i | `Discrete` scale rule allows continuous `stretch` (`resolve.rs:753-762`) | Bound `stretch` for Discrete parts (menu or {1.0}) or document |
| j | Doc nits: `surf:` doc contradiction (`session.rs:696-700`), stale-pick epoch claim (`csp-app/lib.rs:833-852`), `UiOutbox::warp_step` doc (`csp-view/lib.rs:305-310`), `StuckDecoupler` "no decouple command" (`reliability.rs:69-72`), `debug_panel` "M0 scaffold" title (`csp-ui/lib.rs:103`), KeyAction label "recover control" vs recovery verb confusion (`settings.rs:278` → "retake control of vessel") | Comment/label fixes; zero-risk |

**Phase-3 exit criteria:** the two verified user complaints are dead: parts attach below via the canonical gesture; the rocket moves; deletes are undoable; `cargo test -p csp-ui -p csp-view -p csp-phys -p csp-render` + `vab_latency` green. Commit + ship.

---

## Phase 4 — Save / server / hygiene (durability + integration + the meta-fix)

### W4.1 — Player save/load/new-game/revert + save-format unification + restore hardening (HIGH)

- **Requirement:** §7 save machinery complete but player-invisible (bug-report bundles + MCP only — reports 16-F11, 15-F6, 08-H1, 04-D1/D2, 17). **Two divergent `.cspsave` formats:** the §7.2 CSPSAVE codec (`csp-sim/src/save.rs:292-394`) is used only by bug reports (`csp-app/lib.rs:616-635,2490-2502`); MCP `session.save/load` writes a different bincode `SaveFile` (`csp-server/src/backend.rs:256-291,502-568`) with no version chain/`data_set_hash`/byte-exact proof — same extension, mutually unreadable. `session.load` re-installs only **3 of ~10 §7.3 config channels** (`csp-bench/src/bin/csp-mcp.rs:80-93`) → save→load→N-ticks diverges silently (report-04 D2 — `publish_contracts` refills a different board from empty `contract_defs`). `data_set_hash` recorded everywhere, checked nowhere (report-08 M3). §7.4 rule 4 (`SaveBlocking` session flag) unimplemented (report-08 M2). Revert-to-launch absent (PLAN:1180-1184 mandates it + disclosure text).
- **Inputs:** `csp-sim/src/{save.rs,lib.rs}` (`World::restore` ~L3169-3195, `check_serializable` ~L3210-3251, restore gates ~L2779-3100,9623-9904), `csp-view/src/lib.rs` (`View::{save_game,load_game}`, pre-launch snapshot in `launch_craft`), `csp-app/src/lib.rs` (save-dir plumbing, bug-report path reuse), `csp-ui/src/{settings.rs,program.rs,lib.rs}` (KeyActions + buttons + revert prompt with the mandated "revert is not a reroll" disclosure), `csp-server/src/backend.rs`, `csp-bench/src/{bin/csp-mcp.rs,replay.rs}`.
- **Outputs:**
  - Player verbs: save / load / quicksave / new-game / revert-to-launch (pre-launch `WorldSnapshot` captured in `launch_craft`; restore keeps pre-drawn θ per §3.4 — UI states it). `KeyAction` additions + top-bar buttons; §7.4 refusal reasons surfaced when save is refused.
  - Format unification: route `csp-server` `SaveFile` through `csp_sim::save::{encode_save,decode_save}` (the CSPSAVE header already carries everything `SaveInfo` needs — recommended) OR rename the MCP artifact `*.mcpsave` and document the split; add a cross-format negative test either way.
  - Restore channels: centralize the re-install list — `csp-bench/replay.rs:783-819` is the correct superset — into one host-callable function; better: `World::restore` records which channels the snapshot's live state needs (`restore_warnings()`/`needs_config` checklist hosts must satisfy) so the contract stops being documentation-only.
  - `data_set_hash` drift warning on load (§7.7): compare save's hash against live `data_set_hash` → `Recoverable`-class warning listing what changed.
  - `save_blocked: Option<String>` world field set by install-time `SaveBlocking`, checked in `check_serializable`, serialized-not-hashed (the `notifications` precedent).
  - Crafted-save restore hardening batch (report-04): `DebrisDecay{v}` requires `debris_since.is_some()` at restore AND at fire (`lib.rs:7623-7625`); `control_lease` coherence (`Lease::Script` → live script; `Lease::Agent` → `source_seqs` entry, `lib.rs:2779-2784`); `body.throttle ∈ [0,1]` (`lib.rs:9887-9898`); `ScriptState.vessel` `< next_vessel_id` (`lib.rs:3065-3098`); θ=+∞ rejected at restore (`lib.rs:9814-9815` — the `MIN_POSITIVE` clamp makes it unreachable legitimately); `aero_mask` bits vs live graph; `SurfaceLatched.pose.altitude_m ≥ 0`; `facts.bodies_visited` resolved against `orbits.bodies()`; `TransferCargo::Crew` seated-at-source check; `SoiTransition`/`SoiRescan`/node events vessel-residence check.
- **Validation:** save → load → N=10 000 ticks hash-equal against uninterrupted run (the §7.5 test — see W4.5 harness); MCP `session.save`/`session.load` round-trips through the unified codec with matching hash after channel re-install; a crafted snapshot with `DebrisDecay` on a crewed vessel refuses at load; revert restores pre-launch state with identical θ.
- **Parallel:** `csp-sim` restore work vs `csp-view`/`csp-app`/`csp-ui` verb work — split by crate with an agreed `View::save_game/load_game` signature.

### W4.2 — Server hosting + read surface + session tools + SPA (HIGH — M9c/d rows are dead code)

- **Requirement:** `Server::start` has zero non-test callers (`csp-server/src/serve.rs:103`; `csp-app` doesn't dep `csp-server`; no `[[bin]]`) — the "attach to a running game" row and the whole telemetry SPA are unreachable (reports 19-F1, 20-F1). The read surface is vessels+clock+warp (`telemetry.rs:25-33`) while `sim.command` writes everything — agents can `HireCrew`/`FoundColony` but can never read the roster, funds, colonies, contracts, or science back (19-F3). `drain_and_step` discards `fired_events`+`degraded` (`backend.rs:393-421`). Session-class tool families `editor.*`/`camera.*`/`map.focus`/`craft.load` are absent and unimplementable on the `ControlBackend` seam (`mcp.rs:740-860`, `backend.rs:125-172`, `http.rs:41-51`). SPA hard-codes `vessels.0` (`apps/telemetry/dashboards/default.json`) — wrong-vessel hazard.
- **Inputs:** `csp-app` (+`Cargo.toml` dep + settings/CLI flag — wasm-purity gate must keep it native-only; the positive assertion in lint-deps already guards this), `csp-server/src/{serve,backend,mcp,http,telemetry,ingress}.rs`, `apps/telemetry/**`.
- **Outputs:**
  - Host: `csp-app` hosts `Server::start` behind a settings/CLI flag (default `127.0.0.1` + token file already correct; `spa_dir` pointed at packaged `dist/client`), OR a `csp-serve` bin. Decide; §2.12 requires the attach path exist.
  - `TelemetrySnapshot` extended with a `program` object (funds, science, contract summary, tech, colonies, crew, scripts/autopilots status, `focus_vessel_id`) — the generic dotted-path projection makes every addition subscribable for free; also unblocks "telemetry SPA as program view."
  - `SubmitReport` gains `events` (bounded + truncation count) and `degraded`; `tracing` dep added to the crate with `degraded`/`rejected` at `warn` (closes §2.14).
  - `ControlBackend` session-verb seam + `craft.load` (minimum — `SpawnPhysicalVessel` is unusable without it); `editor.*`/`camera.*`/`map.focus` can follow on the same seam.
  - Smaller defects: `csp://schema/commands` resource or `sim.commands` listing (discoverability, 19-F9); `timewarp.set` rung-vs-multiplier doc fix (`mcp.rs:500-508`, report-02 D3); Origin/Host unparseable → 403 (`http.rs:320-325`, F6); `GameHost` placeholder `World::new()` snapshot → `Option<Arc<TelemetrySnapshot>>` (`backend.rs:813-815`, F7); 503 body honesty ("request may still have applied", `http.rs:631-652`, F8); client-identity doc or token-bound leases (F5); `VALID_RATES` dedup (F11).
  - SPA: `focus_vessel_id`-driven vessel paths instead of `vessels.0` (+ `vessels.N.found` guard); dashboard picker via `GET /api/dashboards` + `?dashboard=`; validate shipped `default.json` in a test (F8); graph hidden-tab gap break (F4); orbit-map near-circular clip fix (`orbit-map.tsx:70-77`, F5); `time_to_periapsis_s` in Orbit table.
- **Validation:** `csp-server` e2e suite stays green + new tests for program reads; the game binary boots the server and the SPA attaches (manual or e2e); a `session.save` through attach-mode round-trips with hash equality (needs W4.1's channel fix); wasm purity gate passes (`csp-server` still absent from wasm tree — positive assertion).
- **Parallel:** `csp-server`/`apps/telemetry` work is fully disjoint from `csp-app` hosting wiring (interface = `GameHost`).

### W4.3 — Sim defect batch (MED — one `lib.rs` workstream)

Bundle of confirmed sim defects not already fixed above. One implementer, `csp-sim`-scoped.

| # | Requirement (evidence) | Fix |
|---|---|---|
| a | Event budget splits a same-tick batch mid-tick — "corrupt mid-state" the spec forbids (`lib.rs:7948-7952` break vs `fire_due_events` L7434-7595, `MAX_EVENTS_PER_BATCH=1024`) | Break on budget only at a tick boundary: `events_processed >= event_budget && event_heap.peek().is_none_or(|e| e.tick > clock.ticks())` |
| b | `apply_set_aero_config` silently substitutes a different mask, logs applied (`lib.rs:6405-6414`) | Reject `Recoverable` on cap, or report the substitution on `degraded` |
| c | `RejectKind` types exactly one variant (`lib.rs:1232-1237`) | Populate per-family kinds or narrow the enum's contract to "kinds consumers branch on" + doc |
| d | `advance_orbits` unthrottled `degraded` per unsolvable conic per frame (`lib.rs:7041-7059`) | Dedup/rate-limit persistent entries |
| e | `physics.dominant` fixed "terrin" forever — non-Terrin vessels can't materialize/burn/recover (`lib.rs:5925-5927,6021`; `bubble.rs:397-406`; `csp-view/lib.rs:1983,1099-1114`) | Full dominant-body hand-off is a big design change — **interim:** gate contract `bodies` lists to `terrin`-reachable bodies + document single-bubble scope in `SetManeuver`/PLAN; file the hand-off as tracked follow-up |
| f | `at-or-past tick` commands silently re-anchor to `clock+1` (`lib.rs:5214,5248,6033`) | Keep the documented clamp but surface it (degraded or doc the variant contract) |
| g | `physics.aero` writes unshadowed — the one silent exception to "every mutation is shadowed" (`lib.rs:6322-6340,6379-6403`) | Explicit comment/assertion on the derived-cache invariant |
| h | `RunExperiment` has no debris/aliveness gate (`research.rs:668-761`, report-06 L6) | Add `VesselState`/debris gate |
| i | `Unlatch`/unlatch side-effects scrub pending events + drop `Scheduled` node (`lib.rs:3542-3630` — defensive dead code today) | Keep or document; low |
| j | `deadline_tick` hashed `u16` vs rule-7 `u8` tag (`contracts.rs:546-552`); `hash_script` double id (`scripts.rs:539-548`) | Comment/mark intentional, or fold into next schema bump |
| k | `SetWarp` stores any u32 unclamped (`lib.rs:4800`) | Documented design; optional ladder clamp for hygiene |
| l | `CraftDef::part_ids` serialized-unhashed on a falsified claim (`bubble.rs:102-126`, `lib.rs:9667-9671`, `joints.rs:943-951`, `tech.rs:270-287`, report-01 D2) | Correct the doc to the real invariant + `// HASH-EXCLUDED` marker (hashing it = schema bump — defer) |

### W4.4 — csp-phys defect batch (MED — one workstream)

| # | Requirement (evidence) | Fix |
|---|---|---|
| a | `aero == None` silently substitutes placeholder drag in atmosphere — spec wants refusal (`simple.rs:1546-1564`; unreachable in shipped flow but a public-backend trap) | Surface it: one-shot `degraded`/`Recoverable` per vessel per graph change, or typed error; document on the `PhysicsBackend` trait |
| b | Soft-capture joint impulses never reach `external_loads` — joints carry unbounded load (`simple.rs:2095-2138` vs contact write-back `:2464-2483`) | Write per-row impulse into a `Contact`-shaped/joint-impulse record attributed to port hulls, or cap joint impulse |
| c | `Environment::for_body` maps `rotation_period_s: None` (tidally locked) → zero spin — wrong fixed phase + disabled settle gates for Rask (`simple.rs:434-441`; latent — dominant is terrin) | Contract change: take resolved period (or typed refusal on unresolved tidally-locked input) |
| d | `update_settle` counts vessel–vessel contact as latch-eligible → suspended hulls (`simple.rs:2331`) | Gate on ≥1 *ground* contact (`c.peer.is_none()` — the debris-impact rule's own distinction) |
| e | `mass_kg = 0` → NaN one tick before the sim's Fatal (`simple.rs:1592` et al.) | Validate `mass_kg > 0 && is_finite` at `step` entry |
| f | 20 t-lander soak is 4000 ticks, not 10 000 (`simple.rs:2878-2932`) | Extend soak to 10k (cheap) |
| g | `hulls_overlap`/`vessel_contact` re-derive `facet_planes` per call (`simple.rs:1241-1253`) | Hoist (perf note) |

### W4.5 — CI / test / tooling / meta-fixes (MED — closes the "no gate could see this" hole)

- **Reachability lint (the meta-fix, report-24 F4):** static surface lint — enumerate `SimCommand` variants; each must have a producer in `csp-ui`/`csp-app`/`csp-view` or an explicit `ui-unreachable:` annotation. Ratchets the reachability table so this bug class can't regrow. Also diff `ProgramMirror` coverage vs sim state fields.
- **`lint-deps` currently RED (report-24 F1):** `csp-app` violates `both-sim-and-render` (`csp-app/Cargo.toml:19-22`; need is real — `csp_sim::VesselId` at `csp-app/lib.rs:254`). Pick: re-export sim types through `csp-view` and drop the dep, or amend PLAN §2.1 + `check_deps` to allow `csp-app` a type-level edge. Gate must go green — it masks everything else in the step.
- **Golden coverage (report-24 F2, 21-F6):** `SCENARIOS` has 2 of the 6 §8.2-required scenarios. Add: `docking-merge` (possible once W1.4's `csp.dockport.s125` exists, or a synthetic port craft), `warp-ladder` ×10⁵ jump, `×1-vs-×10⁵ equivalence` as a golden, `colony-century`/pathological-density (promote `perf.rs::colony_warp_run` into a pinned golden).
- **§7.5 N=10 000 harness + kitchen-sink world (report-04 G1/G2):** per-scenario mid-run snapshot→restore→+10 000-tick hash compare in `csp-bench`; one table-driven rich world (colony + contract + build + bench + transfer + script-bound vessel + debris) — the test that would have caught report-04 D2.
- **Eval/bench gaps (report-21):** `WorldSpec::Shipping` (full `data/` world — `contracts.rs::contract_world` is the template); `AgentSpec::Log` same-tick + tick-0 groups (`scenario.rs:424-439`); `rejected_commands.is_empty()` as default pass condition (+`allow_rejections` opt-in); suite cardinality pin (`eval_suite.rs:34-76`); `csp-mcp --scenario <name>` (`bin/csp-mcp.rs:145-149`); `build.rs` lockfile warn (F11); science-loop + colony-resupply + contract-lifecycle-honest-path scenarios (F1/F3/F4/F5).
- **`csp --smoke` app e2e (report-24 F3, §8.5):** offscreen adapter + N frames + snapshot/hash dump + CI entry — the tier that sees winit/GPU/schedule integration.
- **Docs gate:** add `RUSTDOCFLAGS=-Dwarnings cargo doc --workspace --no-deps` to CI + fix the 5 stale links (report-22 F9) + `missing_docs` lint per §8.6 (report-24 F10).
- **Validators:** `ModuleReq` satisfiability vs loaded parts (`validate.rs:4229-4235` gains `&LoadedParts` — report-22 F6); failure-mode↔module applicability (`PanelJam` → deployable-bearing modules; `IgnitionFailure`/`StuckGimbal`/`DegradedThrust` → `Engine`; `StuckDecoupler` → `Decoupler`; `Leak` → `Tank` — report-23 F7); facility registry `data/facilities/*.ron` or doc-mark `unlocks.facilities` (23-F8/22-F7); `data/parts/` non-empty sanity (22-F10); site↔terrain flatness warning (report-13 F4).
- **Nightly tier (report-24 F7):** `nightly.yml` — doc, machete/udeps, MSRV verify, `xtask lint-quality` skeleton, full goldens, cross-platform divergence report (report-only); `cargo-deny` pin + decide `multiple-versions`; coverage floors + ratchet (`ci/coverage-floors.json`); perf baseline measured on ci-host (`perf-log-ci-host-release.ron`, F6); declare `RapierBackend` unimplemented in PLAN or build the minimal oracle path (pinned deps exist but are undeclared).
- **Demo/content drift (report-23 F10):** byte-equality test `apps/demo/data/<p> == data/<p>` for shared files (or generate the subset at build).
- **clippy.toml domain:** extend determinism lints to `csp-ai` (already deps `libm`) — report-24 F12; proptest for the 3 missing §8.1 properties (F9) or document the deviation.
- **Dead-data honesty:** `monoprop` (no consumer — RCS is post-v1 or implement `RcsThruster`), `Drill`/`Converter`/`Control.torque_nm`/`power_w` resolved-and-dropped (doc-mark "reserved — display only" or implement the vessel-side paths — report-22 F1/F2/F3), `ForceSiteKind::Rcs` discriminant, `InstallAeroTables` never issued (worker path superseded — doc-mark), `InstallAeroTables`/`SetAeroConfig`/`Unlatch`/`SpawnVessel` reachability annotations feeding the lint above.
- **Map polish leftovers (report-14):** `sample_orbit` uniform-in-E sampling (F7 periapsis starvation); impact-point clip for sub-surface arcs (F9); focus-on-vessel when opening map (F6); hierarchy-order focus cycle (F10); apsides on predicted conic; `presentation_tick` (F2 if not done in W2.4); pre-latch degenerate osculating ellipse gate.
- **Structures/state-driven VFX (report-13):** `with_sim` terrain seed divergence → build `TerrainConfig` from sim config (`lib.rs:1355-1361`, F2); flat-datum stand-in pop during async bake → prewarm or gate (F3); map site marker on `ground_radius` not datum (F5); `vessel_parts` model key drops part identity → pass `part_ids` (F6); KSC click affordances/state-linked visuals (F1 — or document cosmetic scope).
- **Renderer (report-12 — all Baseline-scope):** `RenderTier` enum + init selection log (G1); aerial perspective on flight stream (D1 — biggest visual win); craft receives terrain shadow (D5); biome edge blend (D3); lit/unlit explicit flag not normal-length (D2); unguarded caps indexing + device-lost terminal (D6); 10⁷ m depth check recorded in perf log (G4); part draws instanced or documented (G2).

**Phase-4 exit criteria:** save/load/revert work player-side with unified format and hash-verified restore; server attach path exists with a readable program surface; the defect batches are landed; the reachability lint runs in CI so "half the game is missing" can never regress silently; `cargo test --workspace` + wasm + goldens green. Commit + ship.

---

## Dependency & file-conflict map

**Hard ordering (must land before dependents are testable):**
- W1.1 (auto-rails) → W2.4 (alarms/transfer/map need on-rails vessels), maneuver/encounter UI, `contract.satellite`, SOI clamping.
- W1.3 (RecoverVessel/TerminateVessel) → `contract.rescue` completable; W4.1 recovery-credit tests.
- W1.4 (parachute) → recovery loop closes (W2.x science recovery demos); `PanelJam` validator rule (W4.5); docking-merge golden (needs a DockPort part — also authored in W1.4's bonus content).
- W2.0 (mirror plumbing) → W2.1–W2.3, W2.5 panels.
- W2.2 (crew/lab) → life support becomes meaningful; `SalaryDue`; W4.5 authored-salary economy probe.
- W2.3 (colonies) → `contract.colony_resupply`; removes interim offer gating from W2.1.
- W4.1 (restore channels + format unification) → W4.2 attach-mode save round-trip.

**File contention (serialize or single-implementer):**
- `crates/csp-sim/src/lib.rs`: W1.1, W1.3, W1.4(deploy apply), W2.5 (wake/bootstrap), W4.1 (restore), W4.3 — route through ONE sim implementer per phase, or sequence strictly.
- `crates/csp-view/src/lib.rs`: W1.2, W1.5(a,b? no—demo.rs), W2.0, W2.6, W4.1 — the mirror/glue file; sequence within phase 2.
- `crates/csp-ui/src/program.rs`: W1.2, W1.5(d,e,i), W2.0–W2.3, W2.5, W2.6 — sequence panels or one UI implementer per wave.
- `crates/csp-phys/src/session.rs`, `crates/csp-ui/src/vab.rs`: all of phase 3 — one implementer.
- `crates/csp-server/src/backend.rs` + `telemetry.rs`: W4.2 internals — one implementer.
- `data/**`: W1.4, W2.2, W4.5 content edits — data implementer; each content change needs validator tests + `lint-attribution` + demo-drift check.

**Suggested parallel dispatch per phase (max ~5 in flight per session rules):**
- **P1:** A: sim `lib.rs` (W1.1+W1.3 sim half). B: W1.2 + W1.5(b,c,d,e) view/UI. C: W1.4 schema+phys+data. D: W1.5(a,f,g,h,i) app/sim-modules. E: reserve/review.
- **P2:** A: W2.0 foundation (lands first). Then B: W2.1+W2.2 UI, C: W2.3 colonies+map, D: W2.4 alarms/transfer/map, E: W2.5 script/autopilot (sim-side after P1 sim implementer frees `lib.rs`). F: W2.6.
- **P3:** A: W3.1 (view+render). B: W3.2+W3.3+W3.4 (session.rs + vab.rs — one implementer).
- **P4:** A: W4.1 saves (sim restore + view/app verbs). B: W4.2 server+SPA. C: W4.3 sim batch. D: W4.4 phys batch. E: W4.5 CI/test/validators/content-hygiene.

## Deferred / out-of-scope (recorded so nothing is silently dropped)

- **Full §2.3 distance-bubble model** (20 km radius, hysteresis, lattice recentering, `active_vessel` sim state) — W1.1 ships the altitude-rule inversion instead; PRD-literal model is a larger architecture change. `bubble_radius_m` stays dead data until then — consume or annotate.
- **Dominant-body hand-off** (multi-SOI bubble worlds, non-terrin materialization, Duva landing) — W4.3-e ships the honest interim gate; full fix is a tracked follow-up.
- **§2.11 remainder:** transfer autopilot program, design assistant/`MissionProfile` generator, `LlmAdapter` — `csp-ai` seam needs multi-driver support first (`lib.rs:2504` single-driver).
- **Action groups** (no sim concept anywhere), **subassemblies**, **part search**, **auto-staging**, **wings/wheels/RCS/vessel-ISRU** — schema+sim work beyond this plan's scope; doc-marked as content gaps.
- **Advanced render tier, N-body, CommNet, EVA, VT, migrations, LLM** — correctly post-v1 per PLAN §4/§6.4/§7.7.
- **`quarterly/` ritual + Advanced-tier gates** — process items for the owner, not implementable.

## How the parent verifies each phase

Per phase, after all workstreams merge: `cargo fmt --check` → `cargo clippy --workspace --all-targets -- -D warnings` → `cargo check --workspace` → `cargo test --workspace` → wasm check → `xtask lint-deps` + `lint-attribution` (if deps/data touched) → golden replays (rebless if hash-schema bumped, with the bump in the same commit) → build the game binary (`cargo build -p csp-app` — user wants builds each time) → commit + ship, then dispatch the next phase.
