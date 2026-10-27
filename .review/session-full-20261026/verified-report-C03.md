# Verified Report C03 — verification of review-report-C03.md against source

Verifier: independent pass over `/home/didi/workspace/Clanker-Space-Program` (working tree @ 7dd4b86).
Every file:line below was opened and read in context; `cargo test -p csp-sim --lib auto_rails` re-run → 2/2 pass (matches the report's empirical claim).

**Result: 5 confirmed, 0 dismissed.** Two findings carry minor evidence corrections that do not change their verdicts (noted inline).

---

### Finding 1: SOI rebase produces surface/atmosphere-intersecting conics with no re-check — CONFIRMED

**Original summary**: `apply_to_rails_at` refuses conics whose periapsis doesn't clear `radius + max(atm_top, bubble_radius)` and is the only site that schedules `DebrisDecay`; `apply_soi_transition` rebases with no clearance check and no decay rescheduling, so rebased debris/vessels can orbit through the body forever.

**Verification**:
- Gate exists and is as quoted: `crates/csp-sim/src/lib.rs:6265-6281` computes `rp` and rejects `rp <= floor` where `floor = radius_m + max(atm_top, bubble_radius_m)` — with the exact rationale comment "the vessel would orbit through the atmosphere untouched forever".
- Decay scheduling is stamp-only: `grep -n schedule_debris_decay` → definition `lib.rs:5064`, sole call `lib.rs:6323` (inside `apply_to_rails_at`, gated on `is_debris` from the freshly stamped vessel). No other caller exists.
- `apply_soi_transition` (`lib.rs:8504-8571`): root-SOI special case → `self.orbits.rebase(...)` at :8560 → writes `VesselState::OnRails(new_orbit)` at :8561-8564 → `contract_notify` → return. No floor check, no `schedule_debris_decay`.
- Both call paths into the rebase share the hole:
  - Event path: `fire_event` `SoiTransition` arm (`lib.rs:8396-8419`) = `apply_soi_transition` then `schedule_next_soi`. `schedule_next_soi` (`lib.rs:5171-5216`) publishes only `SoiTransition`/`SoiRescan` — never decay.
  - Stamp path: `resolve_soi_residence` (`lib.rs:6462` and `:6489`) calls the same `apply_soi_transition` mid-walk; note this means even the gated stamp at :6323 can be immediately followed by an unchecked rebase that replaces the checked conic.
- `PatchedConics::rebase` (`crates/csp-sim/src/orbit.rs:968-1037`) validates only frame structure (target-is-child / event-body-is-parent); the geometry is a bare `cartesian_to_elements` conversion. Confirmed: structure-only validation.
- Reachability is real: the parent-frame conic clearing the parent's floor says nothing about periapsis relative to a child body (a Mun-transfer conic clearing Kerbin's atmosphere can pass under Mun's surface). After the Enter rebase the vessel is `OnRails` on that conic; nothing demotes rails→bubble on low periapsis (residence transitions are command/`ManeuverNodeStart`-driven only), and a bound orbit intersecting the body never publishes another crossing.
- PLAN §3.5 (PLAN.md:1226): "Debris with periapsis below `h_top` decays on a scheduled `DebrisDecay` event" — the report's spec citation is accurate. The bound claimed for the leak is also accurate: `max_debris: 64` (`data/balance.ron:16`) with eviction at `lib.rs:4631-4696` inside `step_bubble_range`, which early-returns when no vessel is `InBubble` (`lib.rs:3997-4004`) — i.e. the cap only bites while the bubble ticks.
- Severity check: debris case (a) is a direct §3.5 violation; vessel case (b) reintroduces exactly what the :6271 gate exists to prevent. MEDIUM is defensible (requires a child-SOI encounter on a low-periapsis conic, but that is the common first-flyby shape).

**Verdict**: CONFIRMED as written, including line numbers, the sole-caller claim, and the spec citations.

---

### Finding 2: A completed ascent autopilot permanently suppresses auto-rails — CONFIRMED (two evidence corrections)

**Original summary**: the auto bubble→rails detection skips any vessel with an engaged autopilot; ascent phase 4 ("Done — drift until disengaged") issues `set_throttle(0.0)` forever and never disengages, so the craft reaches orbit but stays `InBubble`: no orbit line, no maneuver planning, full bubble physics. Disengage is the only recovery and nothing tells the player it is required.

**Verification**:
- Exclusion is unconditional on `enabled`: `lib.rs:4802-4804` — `if self.autopilots.get(vid).is_some_and(|a| a.enabled) { continue; }`.
- Phase 4 never terminates and never disables the record: `crates/csp-ai/src/lib.rs:162-165` — `_ => { api.set_throttle(0.0); }`, doc at :24-25 "Phase 4 — done. Throttle 0; the record stays until `DisengageAutopilot`". The driver writes only `state.persist`; `run_autopilot_tick` (`lib.rs:7937-7980`) never clears `state.enabled`. Only `SimCommand::DisengageAutopilot` (`lib.rs:5507-5513`) removes the record.
- Player-visible impact, each verified:
  - Map draws no orbit: `crates/csp-view/src/map.rs:2360` gates on `if let Some(o) = v.orbit()` — `InBubble` vessels have no conic.
  - Maneuver planning blocked: `apply_set_maneuver` rejects at `lib.rs:6649-6650` ("maneuver: vessel not on rails").
  - No manual rails escape: `SimCommand::ToRails` has no issuer in csp-view/csp-ui (only `crates/csp-bench/src/scenarios.rs:580`) — the player cannot rail the vessel by hand.
  - Cost: the vessel stays `InBubble`, so `step_bubble_range` steps it every integer tick at every warp.
  - No burn-completion rescue: the autopilot issues raw throttle/attitude commands, not maneuver nodes, so `finish_burns` never converts it.
- Recovery: after `DisengageAutopilot` (record removed, :5512), the very next tick the exclusion no longer applies and the auto-detection rails a coasting, clearing vessel — disengage (UI button `crates/csp-ui/src/program.rs:1626-1630`, or a Rhai script's `disengage_autopilot()`) is indeed the recovery. Confirmed.
- The manual-flight path works: `auto_rails_waits_for_coast` + `auto_rails_converts_a_coasting_orbiter` pass (re-run, 2/2) — the hole is specifically the engaged-autopilot one.

**Evidence corrections (do not change the verdict)**:
1. "the frozen `ControlApi` has no disengage verb" is factually wrong: `csp_script::ControlApi` declares `fn disengage_autopilot(&mut self)` (`crates/csp-script/src/lib.rs:131-132`, implemented :295) and `IoControlApi` — the very api object the ascent driver holds — carries it. The fix is therefore even simpler than the report suggests (phase 4 could call `api.disengage_autopilot()`), which strengthens the finding rather than weakening it.
2. "the path the tutorial/Program panel pushes players onto" overstates the tutorial: `crates/csp-ui/src/tutorial.rs` never mentions the autopilot (grep: zero hits). The Program panel's automation card does expose "engage ascent" prominently (`program.rs:1610-1649`). Framing nit only.

**Verdict**: CONFIRMED — behavior, impact chain, and recovery analysis all verified against source.

---

### Finding 3: `step_bubble_range` is a 915-line god-function with triplicated debris-retirement logic — CONFIRMED (one support-claim nit)

**Original summary**: one function under three clippy suppressions carries the whole per-tick pipeline with 11 loop-local accumulators; the impact-removal block (:4540-4608), the max-debris eviction block (:4650-4692), and `remove_vessel` (:4997-5046) repeat the retirement sequence, so any semantics change must be made in up to three places.

**Verification**:
- Span and suppressions exact: `#[allow(clippy::too_many_lines, clippy::too_many_arguments, clippy::cognitive_complexity)]` at `lib.rs:3982-3986`; `fn step_bubble_range` at :3987; closing `}` at :4901 → 915 lines.
- Accumulators present in the prologue/loop as claimed: `dead_vessels` :4017, `latched_total` :4024, `impacts` :4031, `burns_done` :4035, `rail_break` :4048 (`failures`/`impacted`/`latched_now`/`rel_post`/`rel_burns`/`rel_rebody` declared in the tick loop below).
- Triplication verified line by line:
  - Impact removal :4540-4608: heap-scrub `debug_assert` (:4552-4558) → `research::drop_vessel_projects` (:4564) → burn-exit credit loop (:4585-4596) → `lose_aboard_crew` (:4600-4604) → `physics.aero.remove` (:4606) → `dead_vessels.push` (:4607).
  - Eviction :4650-4692: same `debug_assert` (:4654-4660) → `drop_vessel_projects` (:4661) → burn-credit loop (:4671-4682, near-verbatim copy of :4585-4596) → `lose_aboard_crew` (:4685-4690) → `aero.remove` (:4691) → `dead_vessels.push` (:4692).
  - `remove_vessel` :4997-5046: `drop_vessel_projects` (:5009) → arena remove + `lose_aboard_crew` (:5016-5027) → tracked/autopilots scrub (:5028-5034) → `aero.remove` (:5035-5037) → real heap scrub (:5040-5045).
  The "the heap-scrub difference is assert-only" observation for the first two sites is accurate (the assert documents that the scrub is provably dead there); the eviction/impact bodies are otherwise the same sequence.
- Extraction seams sensible: each proposed boundary (`retire_debris_vessel`, consequences pass, `detect_auto_rails_candidates` for :4778-4829, propellant pass) sits at an existing documenting-comment boundary — checked at :4534, :4697, :4770, and the propellant pass region.
- PLAN §8.6 complexity gates: the report's premise that the project mandates CRAP/complexity gating is consistent with the codebase's pervasive `#[allow]` discipline; the specific §8.6 text was not re-read (immaterial to the duplication fact).

**Support-claim nit**: "three bug-fix commits in the last five landed here" is not accurate for the last five commits — `dd3612a` touches no csp-sim file, and the csp-sim hunks of `6cc2cb4`/`b698eb2` land at lib.rs:1614 and :3640 (outside 3987-4901). The churn claim itself still holds: `git log -L 3987,4901` lists 12+ commits touching the span (including four `fix(...)` commits: 81e9a1c, d03ca0e, 3a464a8, d0a3362). Highest-churn hotspot: yes; "in the last five": no.

**Verdict**: CONFIRMED — the structural facts (length, suppressions, triplicated retirement, seams) are all real; only the commit-count aside is wrong.

---

### Finding 4: Tracked opt-in promotes only `SoiTransition` — CONFIRMED as a spec-text deviation (with spec-ambiguity context)

**Original summary**: §2.6.5 says per-object opt-in "promotes that object's events `Silent → Clamping`", but `class_of` promotes only `SoiTransition`; a tracked vessel's `DebrisDecay` and `PartFailure` stay `Silent` forever. LOW / spec-text deviation.

**Verification**:
- PLAN.md:546 (§2.6.5): "Per-object opt-in ("track this vessel") promotes that object's events `Silent → Clamping`." — broad wording, as the report quotes.
- `crates/csp-sim/src/events.rs:449-477`: the only `tracked.contains` check is in the `SoiTransition` arm (:451-457); `DebrisDecay` (:459) and `PartFailure` (:474) sit in the fixed-`Silent` arm, with a comment for `PartFailure` ("surfacing rides the notification log") and none for `DebrisDecay` — exactly as reported.
- The heap rewrite at `lib.rs:5304-5316` re-`class_of`s only `EventKind::SoiTransition` entries for the toggled vessel — the narrow promotion is applied consistently.
- Added context the report could have stated: §2.6.5's own member table (PLAN.md:543-544) qualifies only `SoiTransition` by "focused/tracked vessel" and lists `PartFailure` and `DebrisDecay` under `Silent` unconditionally — the spec is internally ambiguous between the table (narrow) and the sentence (broad), and events.rs:445-448 documents the narrow reading as deliberate. The deviation is therefore a defensible reading of an ambiguous spec, which matches the report's own LOW/spec-text-deviation framing rather than a behavior bug.
- Impact assessment ("players rarely track debris; the load-bearing member is correctly promoted") is fair: the Silent members that name a vessel are exactly `SoiTransition`, `DebrisDecay`, `PartFailure`; everything else vessel-named is already `Clamping` unconditionally.

**Verdict**: CONFIRMED as a LOW spec-text deviation; the suggested remedies (extend one arm + one match arm, or record the deviation next to the `PartFailure` rationale) are proportionate.

---

### Finding 5: Duplicated occupancy scans and repeated heap-scrub idiom — CONFIRMED

**Original summary**: `spawn_site_occupied_by` and `spawn_site_occupant` are two ~40-line copies of the same overlap loop differing only in return type; the `shadow_events_whole → mem::take → filter(event_names_vessel) → collect` scrub idiom appears four more times.

**Verification**:
- `lib.rs:5888-5936` (`spawn_site_occupied_by`, returns `bool`) vs `lib.rs:5954-5990` (`spawn_site_occupant`, returns `Option<VesselId>`): the loop bodies are line-for-line identical (same `InBubble`/`OnRails`/`SurfaceLatched` match, same `pose.body != physics.dominant` skip at :5909/:5963, same `surface_pose_restore` re-materialization, same `volumes_overlap` predicate); the only differences are `values()` vs `iter()` and `return true` vs `return Some(*id)`.
- Divergence risk is concrete as claimed: the latched-pose rebuild and the dominant-body skip each exist twice; the launch path calls `spawn_site_occupant` (recover/replace) while the pre-flight/drain-time gate calls `spawn_site_occupied_by` (`spawn_site_occupied` wrapper :5946-5948 → apply-time check) — a fix applied to one copy can silently miss the other.
- All four scrub-idiom sites verified: `lib.rs:3932-3937` (unlatch), `lib.rs:5040-5045` (`remove_vessel`), `lib.rs:6587-6592` (`ToBubble`), `lib.rs:6829-6834` (`fire_maneuver_start`) — same five-line shape each time.
- Suggested refactor (`site_overlaps` iterator + two folds; `scrub_vessel_events` helper) is mechanically sound: nothing in the four scrub sites varies except the vessel id.

**Verdict**: CONFIRMED.

---

## PRD row spot-verification (as tasked)

| Row | Verdict | Evidence checked |
|---|---|---|
| §3.5 manufacture order/stock/funds atomicity — WIRED | **Correct** | `manufacture.rs:118-255`: every validation (site/launch :131-138, arena row :139-143, BOM shape :147-176, funds :177-180, mass finiteness :181-184, vessel-cap slot :189-191, per-resource stock :194-207) precedes the first mutation; the commit (:209-254: stock debits, funds charge, build record, `ConstructionComplete` event) runs under the command's shadow. Dispatch at `lib.rs:5412-5421`. Atomicity claim holds. |
| §2.4/§3.1 parachute-via-staging — WIRED | **Correct** | `apply_deploy_part` `lib.rs:7214-7259` (parachute check :7232, `PanelJam` gate :7243, deployed-record insert :7253-7257); view side: `StageActions` `csp-view/flight.rs:602-616` and `View::fire_stage` `csp-view/lib.rs:3189-3302` enqueues `DeployPart` per staged deployable (:3279-3285) before the `JettisonPart`s (:3286-3292). Matches commit 7dd4b86's stated feature. |
| §2.6.5 tracked promotion — PARTIAL (→ Finding 4) | **Correct as documented** | See Finding 4 above. |

## Corrections register (for the report author, none verdict-changing)

1. Finding 2: "`ControlApi` has no disengage verb" is wrong — `csp-script/src/lib.rs:131-132` declares `disengage_autopilot` on the trait the autopilot's `IoControlApi` implements; the driver simply never calls it. "UI-only" is also loose (Rhai scripts can issue it). The behavior conclusion is unaffected; the simplest fix is even more local than proposed.
2. Finding 2: the tutorial does not push the autopilot (`csp-ui/tutorial.rs`, zero references); only the Program panel's automation card does.
3. Finding 3: "three bug-fix commits in the last five landed here" — the last five commits' csp-sim hunks land at lib.rs:1614/:3640 or not in csp-sim at all; the span's churn claim should cite the `git log -L` history instead (12+ commits, four of them `fix(...)`).
