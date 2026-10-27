# Verified report C18 — map view + planning surface (`crates/csp-view/src/map.rs`)

Verifier: independent re-derivation against the working tree (HEAD 7dd4b86). Every finding
re-traced in source; the orbit-sampling math re-derived numerically. **Result: 6/6 findings
CONFIRMED** (two with corrected details noted inline). No false positives found. The
non-findings and the PRD table were also re-checked and hold.

---

### Finding 1: Ap/Pe quick-alarm dead clicks on open orbits — CONFIRMED
**Original**: `ArmApsis` returns silently on `Err`/`Ok(None)`; Ap is `Ok(None)` on every open
conic; Pe is `Ok(None)` once outbound past Pe; no `plan_error`, no notice.

**Verification**:
- `crates/csp-view/src/map.rs:1145-1153` — `let Ok(Some(tick)) = resolve_apsis_ticks(...) else
  { return; }` collapses `Err` and `Ok(None)` into a bare return. No `SimCommand` is pushed, no
  `plan_error` set, so the player sees literally nothing (the `plan_error` channel exists and
  renders at `csp-ui/src/lib.rs:1633-1635`).
- `crates/csp-sim/src/scripts.rs:702-703` — apoapsis on any `e >= 1` conic → `Ok(None)`
  (unconditional). `scripts.rs:705-707` — periapsis → `Ok(None)` when `m >= 0` (outbound).
- Deep-check "is Pe still settable on an escape orbit?": YES while inbound (`m < 0` returns a
  tick, `scripts.rs:708`), which is the common post-SOI-entry flyby state — so the defect is
  exactly Ap-on-any-open-conic and Pe-when-outbound (e.g. pressing Pe right after a prograde
  escape burn at Pe: the vessel sits at `m = 0+` → `Ok(None)`, dead click).
- Button gating asymmetry confirmed: `csp-ui/src/lib.rs:1548-1556` renders Pe/Ap whenever
  `focus_vessel.is_some()`, while the adjacent "node" button is properly capability-gated via
  `add_enabled(input.focus_has_node)` (`:1557-1559`).
- "Every other failure path surfaces a string" confirmed: `FindWindow` Err → `plan_error`
  (map.rs:1120), `CreateNode` stale vessel → `plan_error` (map.rs:1235), `departure_node` Err →
  `node_error` (map.rs:1252-1255, rendered `csp-ui:1687`). `ArmApsis` is the only silent one.
- One framing caveat: the quoted contract "an unservable vessel is never a dead click"
  (`csp-ui/src/lib.rs:1454-1455`, staging comment `csp-view/src/lib.rs:917-921`) is scoped to
  the TRANSFER vessel picker, not a panel-wide contract. The citation is textually accurate and
  the same design philosophy, but it is an appeal, not a violated spec line. The defect stands
  on its own.
- Secondary claim confirmed: for InBubble vessels the focused orbit is a per-frame osculating
  refit (`capture_sync_input`, map.rs:2360-2377, `epoch: sim.clock()`), so the apsis tick is
  computed against a conic a burn is actively moving, with no "approximate" hint. (`ArmApsis`
  has no rails/bubble gate, unlike the transfer picker's eligibility staging.)

### Finding 2: Closed-orbit polyline undersamples periapsis (e >= 0.8) — CONFIRMED
**Original**: uniform-in-mean-anomaly sampling leaves a ν-gap up to ~83° at e=0.9; the Pe marker
(exact apsis) detaches from the drawn line; sagitta ~13% of r_pe at e=0.9.

**Verification** (numerically re-derived, Kepler solver, N=128, worst gap across the periapsis):
- The closed branch (map.rs:2437-2447) is uniform in time = uniform in M:
  `t = epoch + period·i/N`. Confirmed by reading.
- `dν/dM` at periapsis = sqrt((1+e)/(1−e)³) = **43.6** at e=0.9 — the report's "~43×" is exact.
- Measured ν-gaps (sample landing exactly at Pe, the phase the report's numbers correspond to):
  **e=0.6 → 14.0°, e=0.8 → 39.3° (sagitta 3.3% r_pe), e=0.9 → 82.6° (14.8%), e=0.95 → 118.6°**
  — matching the report's 14 / 39 (3%) / 83 (13%) / 119. The worst-case grid phase (Pe midway
  between samples) is even worse: 106° at e=0.9, 192° at e=0.95. The report's figures are
  conservative, not exaggerated.
- Marker detachment confirmed: Pe marker is placed at the exact conic apsis via `apsis_point`
  (map.rs:1469-1477) while the line comes from the same `sample_orbit`; at e=0.9 the chord
  passes ~15% of r_pe inside the true periapsis. Same sampler feeds body lines (map.rs:1361),
  the plan route (map.rs:1742), vessel lines (map.rs:1938), and node-preview lines (map.rs:1964).
- The doc comment map.rs:2413-2416 does acknowledge the inverse principle for open orbits
  ("mean-anomaly sampling would pile every point at periapsis" → uniform-ν there).
- **Correction to the suggested fix's numbers** (finding unaffected): uniform-in-E sampling
  bounds the ν-gap at dν/dE|Pe · 2π/N = sqrt((1+e)/(1−e)) · τ/N ≈ **12.2° at e=0.9** (17.5° at
  e=0.95), not the claimed "~(1+e)·τ/N ≈ 5.3°" — the report confused dν/dE with dM/dE (=1+e).
  The fix is still correct in kind: 83° → ~12° at e=0.9, sagitta drops to well under 1% of
  r_pe, same 129 `state_at` calls, no cache-key changes. MEDIUM severity is right
  (presentation-only, but visibly wrong trajectory + detached marker at routine player states).

### Finding 3: Plan route polyline keyed only on departure_tick — CONFIRMED
**Original**: a re-plan with the same optimal departure tick but different duration/conic
reuses the stale polyline; panel shows new numbers while the map draws the old conic.

**Verification**:
- `map.rs:1740-1744` — the polyline re-samples only when
  `scene.plan_line_key != Some(plan.departure_tick)`; the key field is
  `plan_line_key: Option<u64>` (map.rs:592-594). `departure_tick` does not determine the conic
  (`duration_ticks` does too — each cell is a distinct Lambert leg).
- The new plan stages fresh `planning.plan` + `planned.encounter` (map.rs:1111-1117), and the
  encounter marker is drawn from the NEW plan against the OLD cached line (map.rs:1745-1750),
  while `plan_row` reads the new numbers — the mismatch scenario is real whenever the key
  collides.
- Trigger plausibility confirmed: `park_r` is the picked vessel's semi-major axis when it
  orbits the origin (map.rs:1063-1068), so switching the departure vessel changes the Oberth
  pricing and can move the optimal cell in duration while leaving the departure cell — and
  therefore `departure_tick` — unchanged. The grid is anchored at `now`
  (`porkchop_query`, map.rs:1015-1016), so two clicks at the same sim tick (the standard
  paused compare flow) share the grid exactly. Same-tick collision is realistic, not contrived.
- Minor same-class detail (not separately reportable): if `sample_orbit` fails, the key is
  still set (map.rs:1741-1743), so the line is never retried until the tick changes.

### Finding 4: Same-label manual alarms fire-match the wrong row — CONFIRMED
**Original**: matching is by label only, first-armed-first-matched, so "burn" at T+1 h armed
before "burn" at T+1 min makes the earlier row mark FIRED first.

**Verification**:
- `map.rs:876-894` — `matches` requires only `a.fired.is_none() && a.label == label` for
  Manual entries; `self.alarms.iter_mut().find(matches)` takes the first in vector order, and
  rows are pushed in arm order (`ArmAlarm` map.rs:1124-1134). Two distinct `SetAlarm` commands
  carry distinct ticks, both events eventually fire, and each fire consumes the earliest
  unfired same-label row — the association is swapped in between. Both rows end FIRED;
  transient-only, LOW is right.
- Cross-kind collisions are correctly impossible (Window rows match by origin+destination on
  `TransferWindow` events, Manual rows on `PlayerAlarm` labels), but the label string
  "X → Y window" from `ArmWindowAlarm` (map.rs:1212) is a Manual row, so it collides with any
  other Manual alarm sharing that text — and auto labels collide by construction
  ("vessel N apoapsis", map.rs:1154-1158; "vessel N maneuver node", map.rs:1175).
- Script/MCP path: any script `set_alarm` with a player-shared label fires `PlayerAlarm{label}`
  which marks the player's row. Structurally confirmed.

### Finding 5: Load/revert does not reset MapInput::focus_vessel — CONFIRMED
**Original**: `rebuild_from_snapshot` resets every sim-keyed mirror but not the map's vessel
focus; `sync_map` re-anchors by id, and restored worlds reuse VesselIds, so the camera can
silently lock onto a different vessel.

**Verification** (full trace):
- `csp-view/src/lib.rs:1594-1601` — resets `LatchedWeldCache`, `StagePlans`, `MapPlanning`,
  `ScriptLogMirror`, `notice_cursor`, `clear_flight_focus_override()`, then
  `set_mode(Flight)`. `MapInput` is inserted only at world construction (lib.rs:1981) and is
  never touched here — despite the function's own doc (lib.rs:1572-1573): "the restored world
  is a different vessel set, so no stale binding may survive". `set_mode` (lib.rs:2540-2569)
  flips `MapInput.mode` only; it never clears `focus_vessel`.
- `sync_map` (map.rs:523-532): id present in the restored snapshot → `focus` re-anchors to that
  vessel silently; id absent → drops to `table.default_focus`, which IS the star
  (map.rs:218-221: `parent.is_none()` → `default_focus`) — the report's "drop to the star ...
  handled" wording is exact.
- Id-reuse trace: `VesselId` comes from a per-world monotonic counter `next_vessel_id`
  serialized in `WorldSnapshot` and restored with it (csp-sim lib.rs:276, 1401-1402, 1415).
  Ids are unique only within one world, so two different saves overlap heavily (both allocate
  from small integers): **loading a different save while focused on vessel N binds the camera
  to the loaded world's vessel N immediately — a different vessel.** That is the clean trigger.
  For F9/revert specifically the chain is one step longer: the pre-launch snapshot is captured
  BEFORE the spawn lands (lib.rs:1560-1562), so the just-launched focused vessel is absent →
  handled drop; the counter is restored with the snapshot, so the NEXT launch after revert
  re-issues the discarded timeline's id and the stale `focus_vessel` silently binds to the new
  vessel. Focusing an older vessel that predates the launch is retained correctly (benign).
  The report's mechanism and fix are correct; the F9 wording glosses the relaunch step.
- Suggested one-line fix is right and matches the surrounding resets' style.

### Finding 6: porkchop_query doc "now + 1 day" vs 6-hour constant — CONFIRMED
map.rs:982 (doc: "departure window `[now + 1 day, now + 0.9·synodic]`") vs map.rs:1015
(`21_600 * TICK_HZ` ticks = 21,600 s = 6 h). Pure doc/code mismatch, NIT is right.

---

## Non-findings re-verified (report C18 §"Non-findings") — all hold
- **note_fired ordering**: `note_fired(&report.fired_events)` runs inside `sim_step`
  (csp-view lib.rs:2210-2212); `alarm_rows` is built at lib.rs:961 and consumed at lib.rs:979-989
  in `end_frame`'s UiBuild, which the comment at lib.rs:2217 places strictly after SimStep. A
  fire at tick T is folded the same frame it happens, before the 2-s lapse grace
  (`ALARM_LAPSE_TICKS = 2 * TICK_HZ`, map.rs:125) can trip. Panel gating confirmed.
- **Clamping / graft / eviction**: unmatched fires self-graft FIRED (map.rs:893-901); the cap
  loop (map.rs:907-914) removes only fired/lapsed rows, one per iteration, and exits when none
  qualify — armed rows are never evicted, no infinite loop.
- **DismissAlarm**: indices generated (lib.rs:961, `index: i`) and consumed (map.rs:1259-1269)
  within one UiBuild pass; the arm re-checks fired/lapsed; earlier commands in the same batch
  only append (indices stable).
- **Sim-side alarm wiring**: `apply_set_alarm`/`apply_watch_window` clamp to `clock+1`
  (csp-sim lib.rs ~5731-5783) and fire at the carried tick; the heap is serialized
  (`WorldSnapshot.event_heap`, csp-sim lib.rs:1415); `PlayerAlarm` is always `Clamping`
  (events.rs:447), and `fired_events`' Clamping entries are what csp-app's `WarpController`
  demotes warp on (csp-sim lib.rs:1327-1330). Toast "ALARM — {label}" at csp-app lib.rs:3256.
- **Planner honesty**: `predict_encounter_excluding(..., Some(&plan.origin))` (map.rs:1101-1108);
  `departure_node` refusals → `node_error`; `CreateNode` re-resolves against current
  orbit/clock (map.rs:1238-1251).
- **InBubble on map**: osculating refit confirmed (map.rs:2331-2335, 2360-2377);
  SurfaceLatched skip asserted by the same staging comment and immaterial either way —
  `sync_map`'s drop-to-default for any absent id is directly verified.

## PRD rows re-verified — all hold
- §4 alarms WIRED: arm → `SimCommand::SetAlarm` → `PlayerAlarm` (Clamping) → `fired_events` →
  `note_fired` book row + "ALARM — label" toast + warp demote. Repeat alarms: no `repeat`
  anywhere in the alarm code (grep) — correctly listed as KAC-parity gap, not PRD-required.
- Transfer planner: sibling pairs WIRED; planet→moon honestly refused — `shared_frame`
  requires `o.parent == d.parent`, else "porkchop: bodies share no parent"
  (csp-sim/transfer.rs:895-903; the same refusal staged view-side at map.rs:997-1002).
- Maneuver nodes WIRED: numeric pro/nrm/rad editor + Set/Delete (csp-ui lib.rs:1181-1244),
  `DeleteManeuver` routed (csp-view lib.rs:911-914), map label "node BURNING Δv left …"
  (map.rs:1435-1443); map-drag editing absent as stated.
- Porkchop/encounter/window-events/map view/§2.8 rows: mechanism spot-checked (staged plan +
  drawn conic map.rs:1738-1753; SOI rings; `vessel_lines` keyed by quantized OrbitState digest
  incl. parent, map.rs:556-565, so a §2.6.6 SOI re-base re-samples).

## Verdict
**All 6 findings CONFIRMED, 0 dismissed.** Severity ladder (0 CRITICAL / 2 MEDIUM / 3 LOW /
1 NIT) is appropriate. Two corrections folded in: F2's fix bound is 12.2° at e=0.9 (not 5.3°),
and F5's F9 trigger requires a post-revert relaunch (the immediate different-vessel re-bind is
the cross-save load path). F1's "never a dead click" citation is scoped to the vessel picker.
