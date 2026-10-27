# Verified Report C02 — csp-sim lib.rs §A: command contract + World shell

Verification of `review-report-C02.md` against the working tree. Every finding was re-checked at
its cited file:line with surrounding context; the two HIGHs were traced end-to-end (opt-out text →
event classing → mutator/issuer grep → app-side demotion → PLAN quotes → save/load path). No file
in the project tree was modified.

Verdict summary: **7/7 findings CONFIRMED, 0 dismissed.** The Undock counter-claim reconciles with
C10's verified HIGH (details below): consistent about the apply body, but C02's honesty-table
wording "not a no-op" is wrong at the behavior level. The WorldSnapshot↔World CLEAN diff
spot-check (3 fields + 1 config channel) holds.

---

### Finding 1: `SetVesselTracked` opt-out lies — PRD §2.6.5 focused-vessel SOI clamping unreachable for players — CONFIRMED (HIGH)

**Original**: The opt-out claims player focus/tracking is "view-side session state, not a sim
command", but the event class is sim-side and gated on `tracked_vessels`; nothing in production
ever issues `SetVesselTracked`, so PLAN §2.6.5's "focused/tracked vessel `SoiTransition`" Clamping
member is unreachable for players; only manual alarms clamp.

**Verification** (every link in the chain):
- Opt-out text verbatim at `crates/csp-sim/src/lib.rs:564-565`: "an MCP/agent tracking verb — the
  player's focus+tracking is view-side session state, not a sim command."
- Classing is sim-side: `crates/csp-sim/src/events.rs:449-457` — `class_of` returns
  `EventClass::Clamping` for `SoiTransition` **iff** `tracked.contains(vessel)`, else `Silent`.
- Mutators: the only production **insert** into `tracked_vessels` is the `SetVesselTracked` apply
  (`lib.rs:5295`); the apply also restamps queued SOI events (`lib.rs:5299-5316`), exactly as the
  report says. (Precision nuance, does not affect the finding: the latch merge re-keys an
  already-tracked id during a dock merge — `joints.rs:1132-1134` `absorbed_tracked` → `prim` — and
  removal paths remove ids — `lib.rs:4849`, `:5028`. Neither can newly track a vessel.)
- Issuers: `rg SetVesselTracked` across all crates hits only csp-sim — declaration, docs, apply,
  and five test constructions (`lib.rs:13006/13016/13028/13171/23673`), all inside `#[cfg(test)]`
  (module starts `lib.rs:11881`). **Zero production issuers** in csp-view/csp-ui/csp-app/
  csp-server/csp-bench/csp-script/csp-ai. MCP's `sim.command` raw passthrough (`csp-server/src/
  mcp.rs` `t_sim_command`) *could* decode one from JSON — that keeps the "MCP/agent verb" half of
  the annotation technically true — but no production code path issues it, so the player case is
  unserved.
- Demotion is fired-event-class-driven only: `crates/csp-app/src/lib.rs:1251-1262` —
  `report.fired_events.iter().any(|e| e.class == EventClass::Clamping)`; the comment at 1251-1253
  concedes the design: "(PlayerAlarm, tracked-vessel SoiTransition)" — and nothing ever tracks.
  View-side focus (`csp-view/src/map.rs:273` `focus_vessel: Option<VesselId>` session field;
  `csp-view/src/flight.rs:1963-1966` newest-spawn heuristic) has no write path into the sim-side
  set.
- PLAN quote verified: `PLAN.md:543` (§2.6.5 class table) — "| `Clamping` | auto-demotes warp so
  the player cannot overshoot | **focused**/tracked vessel `SoiTransition`, `ManeuverNodeStart`,
  `PlayerAlarm`, … |". The word "focused" is in the PRD row and is served nowhere.
- Player harm at ×10⁵, concrete: PLAN's own back-pressure text (`PLAN.md:549-550`): "At ×10⁵ a
  frame covers ~18 sim-minutes". A player warping at ×10⁵ toward their own moon encounter: the
  `SoiTransition` fires `Silent` (vessel untracked — nothing can track it), warp is not demoted,
  and each subsequent frame advances ~18 sim-minutes at the old rung — the craft crosses the SOI
  and blows past periapsis several sim-minutes before the player can visually react. That is
  precisely the overshoot the Clamping class exists to prevent. Mitigation confirmed as
  manual-only: `SetAlarm` is UI-reachable (`csp-view/src/lib.rs:1232`, program panel) and
  `PlayerAlarm` is always Clamping — but the player must know the SOI tick in advance and set the
  alarm themselves.

Severity HIGH is correct: a PRD-named player protection is absent while the opt-out annotation
asserts it is covered "view-side" — covered nowhere. Both fix routes in the suggestion are real
(the command already exists, is logged, and restamps queued events; or reclass at schedule time).

---

### Finding 2: `active_vessel` does not exist as sim state — §2.2 and §7.2 both name it — CONFIRMED (HIGH)

**Original**: PLAN §2.2 makes `active_vessel` sim state, §7.2 lists it in the serialized set; the
word appears nowhere in csp-sim; focus is view-side and lost across save/load; deviation
undocumented.

**Verification**:
- `grep -rn active_vessel crates/csp-sim` → **zero hits**. Neither `World` (lib.rs:2183-2432) nor
  `WorldSnapshot` (lib.rs:1389-1552) has any such field or equivalent.
- PLAN references verified: `PLAN.md:195-196` ("`active_vessel: Option<VesselId>` is **sim
  state** (it gates control authority), changed only by command"), `:1627` (§7.2 serialized set:
  "sim tick; warp level; `active_vessel`; control leases"), `:267` ("radius of `active_vessel`"),
  `:269` ("recentre when the active vessel's lattice cell changes" — the bubble is in fact
  body-centric: `World.physics` is "backend + dominant body", lib.rs:2360-2364).
- Adjudication trail verified: `REVIEW2-normal.md` H17 fix (L203-206): "Also move 'active vessel /
  control authority' into sim state, since it gates control"; M49 fix (L477-479): "`active_vessel:
  Option<VesselId>` is sim state, changed only by command". The plan adopted it; the code never
  implemented it; no doc comment records the supersession.
- Save/load behavior verified: the save payload is `csp_sim::save::Save` = `{game_version, label,
  data_set_hash, state_hash, snapshot}` (`crates/csp-sim/src/save.rs:128-144`) — envelope +
  WorldSnapshot only, no focus. `save_game` (`csp-view/src/lib.rs:3062-3072`) writes exactly that.
  On load, `rebuild_from_snapshot` (`csp-view/src/lib.rs:1574-1602`) calls
  `view.clear_flight_focus_override()` and `set_mode(ViewMode::Flight)` — flight focus falls to
  `focus_vessel() = vessels.last()` (`flight.rs:1963-1966`), whose own doc admits "a fresh launch
  (or a new breakup fragment) takes the camera". Camera/HUD jump to the newest vessel on every
  load: confirmed.
- The supersession half of the report is also accurate: per-vessel `Lease` (lib.rs:459-478) +
  `control_gated_vessel` (lib.rs:1243) + explicit `VesselId` addressing do serve the
  control-authority rationale; the report correctly credits this while still flagging the missing
  documentation.

Severity HIGH stands (PRD-named serialized state missing without §6.4 sanction + a real
player-visible load defect), and the report's own impact/honesty analysis is careful and correct.

---

### Finding 3: `SpawnVessel` opt-out names a nonexistent variant (`BuildVessel`) — CONFIRMED (MEDIUM)

**Original**: The opt-out says the launch path uses `BuildVessel` + `SpawnPhysicalVessel`;
`BuildVessel` exists nowhere but the comment; real launch path is `ManufactureCraft` →
`ConstructionComplete` with `SpawnPhysicalVessel` as the site-less fallback; conclusion
(MCP/test-only) still true.

**Verification**:
- `rg BuildVessel` across the repo → exactly one hit: `crates/csp-sim/src/lib.rs:552` (the comment
  itself). The named API does not exist.
- Real launch path verified: `csp-view/src/lib.rs:~3018-3025` enqueues
  `SimCommand::ManufactureCraft` for an authored launch site; the site-less sandbox fallback
  enqueues `SpawnPhysicalVessel` at ~3037-3044 with the comment "Sandbox/test worlds only; the
  shipped data set always authors `terrin-launch`".
- Conclusion still true: csp-view's `SpawnVessel` hits are test-only — `lib.rs:5630` sits inside
  `#[cfg(test)]` (starts 5106), `map.rs:3257` inside `#[cfg(test)]` (starts 2814); csp-bench
  issues it (`perf.rs:810`, `contracts.rs:218`); MCP's raw `sim.command` passthrough can decode
  it. Elements-only spawn is genuinely MCP/test-only — the stated *reason* is just false.

CONFIRMED as written; the suggestion (rewrite the reason; have lint-reach flag unknown backticked
identifiers in opt-outs) is sound.

---

### Finding 4: §7.4 rule 3 (pending Stage-2 weld) neither implemented nor documented as vacuous; `check_serializable` doc stale — CONFIRMED (MEDIUM)

**Original**: `check_serializable` implements rules 1/2/4 + queue-empty but not rule 3; nothing
says why; the doc lead-in ("the one M1 case") is stale; vacuousness is fragile given the prior
review contemplated deferred voxelization.

**Verification**:
- `check_serializable` (`lib.rs:3497-3550`) body checked rule by rule: rule 4 (`save_blocked`,
  3498-3509), queue-empty (3510-3514), rule 1 (`!self.joints.is_empty()`, 3515-3522), rule 2
  (ground contact OR vessel-vessel contact via `bubble_vessel_contact(..., false)`, 3523-3548).
  No rule-3 check exists anywhere in the function, and no comment addresses its absence.
- Stale lead-in confirmed at 3491-3496: the doc says the class is "applied to the one M1 case
  that can violate it: undrained commands in flight" — while the body checks four conditions.
- Vacuousness grounded: `ensure_aero` (`lib.rs:6846`) computes aero tables synchronously;
  `InstallAeroTables` attaches at apply. And the fragility claim is real, not hypothetical:
  `REVIEW2-normal.md:190-194` explicitly adjudicated "(2) aero voxelization — expensive,
  **deferred**, with the child bodies inheriting the parent's aero table until the recompute
  lands … On wasm, voxelization runs incrementally across frames". A future implementation of
  that adjudication reintroduces exactly the pending-Stage-2 state rule 3 guards, with no refusal
  hook and no doc trail.

CONFIRMED: not a live bug (the report says so and is right), but the honesty gap and stale doc are
exactly as described. MEDIUM is appropriate.

---

### Finding 5 (DRY/CRAP): TickShadow's hand-rolled ~30-`Option` shadow machinery across capture/merge/restore + parallel field lists — CONFIRMED (MEDIUM)

**Original**: Every new arena adds 6-8 hand-rolled touch points following one identical
"earliest-wins Option" pattern; two `#[allow]`s concede the complexity budget; a missed touch
point silently breaks rollback.

**Verification**:
- `TickShadow` (lib.rs:1703-1822): 33 fields total, **26 are `Option`** (the report's "~30" is a
  slight overcount — immaterial to the pattern claim).
- `capture` inits all 26 to `None` verbatim (1827-1863).
- `merge_committed` carries `#[allow(clippy::cognitive_complexity, clippy::too_many_lines)]`
  (1881) and repeats `if self.X.is_none() { self.X = committed.X; }` ~24 times (1905-2005).
- `restore` carries the same `#[allow]` (2014) and repeats `if let Some(v) = self.X { world.X = v; }`
  ~24 times (2055-2172).
- The special cases are exactly the ones the report exempts: `vessels` (Whole/PerItem merge and
  restore logic, 1883-1904/2055-2079), `event_heap` (Ops/Whole reconciliation,
  1920-1948/2089-2112), `pending` (drained/issued/seq_restore, 2020-2054). Everything else is the
  uniform pattern — macro-generable as suggested. One added nuance for the fix: field `benches`
  restores into `world.bench_runs` (2161-2163) — a declarative macro needs rename support
  (trivial, standard).
- Parallel lists confirmed: `WorldSnapshot` fields (1389-1552), `snapshot()` (3560-3600),
  `restore()` constructor (3408-3488) — the 6-8-touchpoint-per-arena claim is accurate.

CONFIRMED as the crate's highest-drift surface; the macro/trait suggestion is feasible.

---

### Finding 6: Stale "M1 / no vessel removal" rationales on the arena docs — CONFIRMED (LOW)

**Original**: WorldSnapshot.vessels and World.vessels docs justify no-free-list/no-generations with
"at M1 there is no vessel removal", but removal has existed for many milestones; conclusion still
holds, reason is stale.

**Verification**: Both doc comments verbatim at lib.rs:1403-1406 and 2216-2219. Vessel removal is
real and shipped: `RecoverVessel` (lib.rs:915), `TerminateVessel` (lib.rs:1193), oldest-first
debris eviction (lib.rs:311-312), and the removal paths that scrub `tracked_vessels`
(lib.rs:4849, 5028). The conclusion (a `BTreeMap` arena with a monotonic counter has nothing to
serialize for free lists/generations) does still hold — the report's rewording suggestion is the
right fix. CONFIRMED.

---

### Finding 7: `let _ = reason;` dead-binding dance in `check_serializable` — CONFIRMED (LOW)

**Original**: The save_blocked branch binds `reason` only to discard it; honest form is
`if self.save_blocked.is_some()`.

**Verification**: Exact code at lib.rs:3501-3508 — `if let Some(reason) = &self.save_blocked {
/* comment */ let _ = reason; return Err(...) }`, with the comment explaining the detail rides
`save_blocked_reason()`. The suggested rewrite is behavior-identical and clearer. CONFIRMED.

---

## Undock reconciliation (C02 honesty-table row vs C10 verified Finding 1)

**Result: CONSISTENT about the apply body; C02's "not a no-op" phrasing is wrong at the behavior
level and its "HONEST … only the UI is absent" verdict is incomplete. Not a genuine
contradiction.**

- C02's row claims `apply_undock` (:7273) "is a full implementation (graph split, momentum stamps,
  seam/aero partition), not a no-op". Verified TRUE about the body: `apply_undock`
  (lib.rs:7273-7524) performs the graph split (`fragment_graph_of`/`subtree_set`, 7299-7307),
  momentum-conserving material-point stamps (7308-7325), aero table inheritance + retained-side
  rebuild (7318-7370), seam partition (7380-7403), plus thermal/tank/crew/reliability/science/
  deployed splits, roster reseat, and event scrubbing. It is emphatically not a stub.
- C10's verified HIGH claims the command is a **functional no-op on the live path**: the body
  never inserts the pair into `joint_refused` (verified by reading the full body — the only
  shadow interactions are `shadow_vessel`/`shadow_science`/`shadow_economy`/
  `shadow_events_whole_split`; no `joint_refused` write), so the tick loop's
  `latch_pass`+`lifecycle` (lib.rs:4741/4762) re-captures the still-mated, zero-rel-speed pair the
  next tick and re-merges it the tick after — reproduced empirically through the real command
  path (2 ticks ≈ 31 ms at 64 Hz) in C10's /tmp worktree.
- Reconciliation: C02's claim is scoped to the apply body (it was rebutting a *suspected stubbed
  apply*), C10's to end-to-end behavior. "Complete-then-recaptured" satisfies both. However, C02's
  row verdict should be amended: the annotation's own text ("MCP/benchmarks only today") implies
  the command works when issued through the channel it names, and C10 proved it self-undoes within
  2 ticks even there. The ui-unreachable annotation remains honest about the *UI* absence, but
  C02's table row missed the live-path defect (missing `joint_refused` insert) that makes the
  shipped command behaviorally inert — a scope-boundary miss in C02 (which reviewed L1-3634 plus
  spot reads), not a false counter-claim about the code's content.
- The TickShadow counter-claim portion of the task is covered under Finding 5 above: ~30 Option
  fields (actual: 26), macro-generable, both `#[allow]` suppressions present — all verified.

## WorldSnapshot ↔ World CLEAN diff — spot-check CONFIRMED

Three fields traced through all four sites (World field → `snapshot()` → WorldSnapshot field →
`restore()` constructor), plus one config channel end-to-end:

1. `tracked_vessels` (plain state): World:2223 → snapshot():3567 → WorldSnapshot:1409 →
   restore():3418. Round-trip clean.
2. `notifications` (bounded log arena): World:2418 → snapshot():3597 → WorldSnapshot:1544 →
   restore():3480. Round-trip clean.
3. `colony_env` (config channel — the reinstall path): correctly **absent** from WorldSnapshot;
   `restore()` starts it empty with an explanatory comment (lib.rs:3449-3451);
   `install_config` (lib.rs:2715-2725) reinstalls exactly **7 channels** (experiments, parts,
   tech_tree, sites, contracts, colony_env, recipes) from the originating world's `WorldConfig`
   (`config()`, ~2690-2703 — the "both directions" claim); `missing_config_channels`
   (lib.rs:2742+) is the load-bearing postcondition; the production loader
   (`rebuild_from_snapshot`, csp-view lib.rs:1584-1591) calls `install_config` then surfaces the
   missing-channel warnings. Clean.

Deliberate exclusions are each documented at the exclusion site: `pending`/`command_log`/
`script_log` (fresh at restore, 3411-3412/3477; queue-empty enforced by §7.4 at save time),
`joints` (restore comment 3422-3428 — §7.4 refuses the boundary while any live joint exists),
`save_blocked` (session state, 2424-2431 and 3483-3487), `orbits`/`physics` (restore arguments),
`script_engine`/`autopilot_driver` (host objects, reinstalled by the host at csp-view
lib.rs:1585-1586). Nothing settable-but-unrestored found — the report's §7.2 coverage verdict
("WIRED except `active_vessel`") is accurate.

## Final verdict

7 confirmed / 0 dismissed. All three deep-verification targets (SetVesselTracked opt-out,
active_vessel, BuildVessel) hold in full; the Undock counter-claim reconciles with C10 as
"complete-then-recaptured" (C02 right about the body, C10 right about behavior); the
WorldSnapshot↔World diff is clean as claimed. Line citations in the report were accurate
throughout (worst deviation: "~30 Option fields" is actually 26; csp-app comment at 1251-1253 vs
cited 1257).
