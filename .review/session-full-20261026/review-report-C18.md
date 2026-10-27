# Review report C18 — Map view + planning surface (`crates/csp-view/src/map.rs`)

Scope: map.rs code L1–2813 (tests L2814–3610 skimmed), verified against `csp-sim` (`lib.rs`
SetAlarm/WatchTransferWindow applies, `transfer.rs`, `scripts.rs::resolve_apsis_ticks`), the
`csp-ui` planning/maneuver panels, and the `csp-view/src/lib.rs` glue (`map_panels`,
`rebuild_from_snapshot`, `sim_step`'s `note_fired`). No CRITICAL/HIGH findings: the alarm book
and planner are genuinely wired end-to-end (arm → SimCommand → Clamping event → `fired_events`
→ book row → app toast), and nothing drops a legitimate alarm or corrupts sim state.

### [MEDIUM] Finding 1: Ap/Pe quick-alarm buttons are dead clicks with no feedback on open orbits
**File**: `crates/csp-view/src/map.rs:1145–1153` (also `crates/csp-ui/src/lib.rs:1551–1556`)
**Problem**: `C::ArmApsis` resolves via `csp_sim::scripts::resolve_apsis_ticks`, which returns
`Ok(None)` for apoapsis on any open conic (`scripts.rs:702–703`) and for periapsis once past
periapsis outbound (`scripts.rs:705–707`). The resolver treats `Err` and `Ok(None)` identically
with a bare `return` — no `plan_error`, no notice, nothing.
**Evidence**: the "Pe"/"Ap" buttons render whenever a vessel is focused, regardless of conic
type (`csp-ui/src/lib.rs:1548–1556`); the sim returns `Ok(None)` for Ap on every hyperbolic
conic — i.e. any vessel on an escape/ejection trajectory, exactly the state the planning
surface exists for.
**Impact**: pressing "Ap" on an escape orbit (or "Pe" outbound) silently does nothing. This
violates the panel's own contract — "an unservable vessel is never a dead click"
(`csp-ui/src/lib.rs:1454–1455`, `map.rs` staging at `csp-view/src/lib.rs:919–921`). Every other
failure path in `resolve_planning_command` surfaces a string (`node_error`, `plan_error`).
Secondary: for an InBubble (thrusting) vessel the apsis is computed from a per-frame osculating
refit — a burn moves the apsis mid-count — also with no "approximate" hint.
**Suggestion**: on `Err`/`Ok(None)`, set `planning.plan_error = Some("no apoapsis ahead — open
orbit")` (a one-line arm change, same channel the planner already uses); optionally gate the
buttons on `focused_conic(...).elements.eccentricity < 1` for Ap.

### [MEDIUM] Finding 2: Closed-orbit polylines are badly undersampled at periapsis (e ≥ 0.8)
**File**: `crates/csp-view/src/map.rs:2429–2447` (`sample_orbit` closed branch), `:51` (`ORBIT_LINE_SAMPLES = 128`)
**Problem**: closed conics are sampled uniform in **mean anomaly** (= uniform in time). True
anomaly advances up to ~43× faster than M at periapsis for e = 0.9, so the polyline is nearly
straight through the periapsis lobe. Measured (128 segments): max ν-gap 14° at e=0.6, 39° at
e=0.8 (chord sagitta ≈ 3% of r_pe), 83° at e=0.9 (13%), 119° at e=0.95 (25%).
**Evidence**: `map.rs:2413–2415` already acknowledges the principle for **open** orbits
("mean-anomaly sampling would pile every point at periapsis" → they sample uniform in ν there)
but keeps uniform-M for closed ones, which *starves* periapsis instead. High-e ellipses are
routine player states: every raise-Ap-toward-SOI-edge intermediate orbit (the standard
KSP-style escape path) has e ≳ 0.9–0.97.
**Impact**: the drawn trajectory visibly clips the periapsis corner, and the Pe marker — placed
at the exact conic apsis via `apsis_point` (`map.rs:1473–1477`) — sits visibly **off the drawn
line** for the focused orbit. Same sampler feeds node-preview and plan lines. Sim state is
unaffected (presentation only), but this is exactly the "wrong-looking trajectory" class.
**Suggestion**: sample uniform in eccentric anomaly (E = τ·i/N → M = E − e·sinE → tick), which
bounds the ν-gap near periapsis at ~(1+e)·τ/N ≈ 5.3° at e=0.9; same 129 `state_at` calls, no
cache-key changes.

### [LOW] Finding 3: Staged plan's route polyline keyed only on `departure_tick`
**File**: `crates/csp-view/src/map.rs:592–594`, `:1740–1744`
**Problem**: `plan_line_key = Some(plan.departure_tick)` is the sole invalidation key for the
drawn transfer route. A re-plan that keeps the same optimal departure tick but selects a
different **duration**/conic reuses the stale polyline.
**Evidence/Impact**: realistic trigger — "Find window" twice with a different picked vessel:
`park_r` changes (`map.rs:1063–1068`), the Oberth pricing shifts the optimal cell in duration
while the departure tick can land unchanged; the panel then shows the NEW Δv/encounter numbers
while the map draws the OLD conic (the fresh `planned.encounter` marker can sit off the line).
**Suggestion**: key on `(departure_tick, duration_ticks)` or reuse `orbit_line_key(&plan.conic)`.

### [LOW] Finding 4: Same-label manual alarms can fire-match the wrong book entry
**File**: `crates/csp-view/src/map.rs:876–894`
**Problem**: Manual alarms dedupe/match by **label only**, first-armed-first-matched. Arming
"burn" at T+1 h and then "burn" at T+1 min makes the T+1 min `PlayerAlarm` mark the T+1 h row
FIRED (both eventually fire; the association is swapped). Auto-generated labels collide by
design ("vessel N apoapsis" for every apsis alarm of that vessel).
**Impact**: transient wrong countdown/row state; a script/MCP alarm sharing a player label can
prematurely "fire" the player's row. Low — rows are view-session state and both end FIRED.
**Suggestion**: among same-label armed entries, match the one with the largest tick ≤ event
tick (nearest-deadline match).

### [LOW] Finding 5: Load/revert does not reset the map's vessel focus — stale id re-binds silently
**File**: `crates/csp-view/src/lib.rs:1594–1600` (`rebuild_from_snapshot`), `crates/csp-view/src/map.rs:273`
**Problem**: the restore path resets `MapPlanning`, `StagePlans`, the focus pin "so no stale
binding may survive" — but not `MapInput::focus_vessel`. `sync_map` re-anchors by id, and
restored worlds reuse VesselIds, so the map camera can silently lock onto a *different* vessel
after F9/revert (or drop to the star if absent — that half is handled).
**Suggestion**: `map_input.focus_vessel = None` (+ focus to default) in `rebuild_from_snapshot`,
one line alongside the other resets.

### [NIT] Finding 6: `porkchop_query` doc says "now + 1 day", code uses 6 hours
**File**: `crates/csp-view/src/map.rs:983–984` vs `:1015` (`21_600 * TICK_HZ` = 21 600 s = 6 h).
Fix the comment (or the constant, deliberately).

## Non-findings verified (so nobody re-derives them)
- **Alarm lifecycle is sound**: `SetAlarm`/`WatchTransferWindow` clamp to `clock+1` and
  virtually never reject (`lib.rs:5734–5783`), the sim heap is serialized in `WorldSnapshot`
  and restored (so alarms survive save/load), `note_fired` runs every `sim_step` before
  `alarm_rows` is read (fire is always observed before the 2-s lapse grace can trip), and
  unmatched fires (script/MCP alarms) self-graft as FIRED rows. Cap eviction only removes
  fired/lapsed rows — armed rows are never evicted, and the `while … && let Some(i)` cannot
  loop forever. `DismissAlarm` indices are generated and consumed within one UiBuild pass —
  no staleness window.
- **`WatchTransferWindow` ticks match**: the sim fires `TransferWindow` AT the carried tick
  (`lib.rs:5774`), which is exactly `plan.departure_tick` the book recorded.
- **Planner rendering is honest**: `predict_encounter_excluding(…, Some(&plan.origin))`
  matches transfer.rs:1370–1379's documented egress-exclusion contract; `departure_node`
  refusals surface as `node_error`; `CreateNode` re-resolves against the current orbit/clock.
- **Bubble vessels on the map** (re: the parallel autopilot/auto-rails finding): `capture_sync_input`
  fits an osculating conic for `InBubble` vessels (`map.rs:2363–2377`), so a suppressed-auto-rails
  ascent still shows marker + refit line + focus; `cartesian_to_elements` fails only on exactly-
  radial/non-finite states, so no blink-out. SurfaceLatched vessels are skipped entirely and
  `sync_map` cleanly drops focus to the default body.
- Presentation-tick rewind handling (`present_tick > now_tick` reset), zoom-toward-cursor sign,
  and the f64-relative-then-f32 projection rule are all correct as written.

## PRD coverage table (M5 / §4 / §2.8)

| Requirement | Status | Evidence |
|---|---|---|
| §4 KAC row: alarms | **WIRED** (naming ✓, fire→warp-demote ✓ per §2.6.5 Clamping, app toast "ALARM — label" `csp-app/lib.rs:3256`, honest Lapsed state) | `map.rs:769–941`; repeat alarms ✗ (KAC-parity gap, not PRD-required) |
| §4 KAC row: transfer-window planner | **WIRED** for sibling pairs (terrin↔duva); **refused honestly** for parent-child (terrin→rask: "bodies share no parent") — the common planet→moon transfer has no planner; M5's exit criterion (Duva) is met | `map.rs:985–1025`, `transfer.rs:895–903` |
| M5: maneuver nodes + executor | **WIRED** — create (plan-derived AND arbitrary numeric pro/nrm/rad), edit (Set replaces), delete (`DeleteManeuver` via maneuver panel, `csp-view/lib.rs:912`), executor sim-side with map "node BURNING Δv left" label; **map-drag editing absent** (numeric-only; no PRD line mandates dragging) | `map.rs:1416–1452`, `csp-ui/lib.rs:1181–1244` |
| M5: Lambert/porkchop planner | **WIRED** — staged plan with Δv depart/arrive, T− countdowns, drawn route conic | `map.rs:1054–1123` |
| M5: encounter prediction | **WIRED** — live conic + predicted post-burn conic + plan conic, Pe/v∞/IMPACTS readouts, SOI rings | `map.rs:1652–1778` |
| M5: transfer-window events | **WIRED** — `WatchTransferWindow` → Clamping event → book row + toast | `map.rs:1188–1207` |
| M1/§5.1 R0: map view | **WIRED** — camera/focus/picking/orbit lines/apsides/sites, f64 precision discipline per §2.3 | whole file |
| §2.8 patched conics, seam at SOI | **WIRED** — SOI re-base changes `OrbitLineKey.parent` → clean resample; whole-conic silhouettes ride parent positions (KSP-style); no seam artifact beyond the intended instantaneous re-base | `map.rs:633–682, 1930–1952` |

## Top 3 issues
1. Finding 2 — eccentric-orbit polyline clips periapsis (e ≥ 0.8): the Pe marker detaches from the drawn line; uniform-eccentric-anomaly sampling fixes it for free.
2. Finding 1 — "Ap"/"Pe" quick-alarm dead clicks on escape trajectories, violating the panel's own never-a-dead-click contract.
3. Finding 5 — load/revert leaves the map's vessel focus bound to a reused VesselId (the exact stale-binding class `rebuild_from_snapshot` exists to clear).

Severity counts: 0 CRITICAL / 0 HIGH / 2 MEDIUM / 3 LOW / 1 NIT.
