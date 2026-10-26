# PRD-05 — Orbit layer & transfers

**Files reviewed:** `crates/csp-sim/src/orbit.rs` (2536), `transfer.rs` (2556), `transfers.rs` (1687), `maneuver.rs` (362), `bubble.rs` (406), plus call sites in `crates/csp-sim/src/lib.rs`, `crates/csp-view/src/{lib,map}.rs`, `crates/csp-ui/src/lib.rs`, `crates/csp-server/src/mcp.rs`, `crates/csp-script/src/lib.rs`.
**Spec:** PLAN.md §2.8 (orbit layer), §2.6.6 (SOI scan), §3.5 (`ResourceTransfer`/crew), M5/M6 exit rows, §2.11 *Transfer* job, §2.4 (20 km bubble).

## Verdict

The sim internals are **strong** — universal-variable solver with bracketed Laguerre–Conway iteration, a carefully-validated Lambert solver (residual gate + propagation check + landing polish), the §2.6.6 SOI scan with stride/Brent/budget/suppress machinery, a thrust-integrated maneuver executor, and a complete transfer-*process* implementation. **But the player path is broken at the root:** nothing in the shipped game ever puts a vessel `OnRails`, which transitively dead-ends maneuver nodes, the executor, SOI transitions, and the entire interplanetary loop. The transfer planner itself is callable only from Rust test code — even MCP agents cannot invoke it.

## Player-path reachability

| Command / function | Sim apply | UI producer | MCP path | Verdict |
|---|---|---|---|---|
| `ToRails` | `lib.rs:5615` | **none** | `sim.command` raw passthrough | Player: **unreachable** |
| `ToBubble` | `lib.rs:5908` | `recover_control` (`csp-view/lib.rs:1946`) | `sim.command` | Dead in practice — nothing is ever on rails to recover |
| `SetManeuver`/`DeleteManeuver` | `lib.rs:5974`, `6074` | map maneuver panel (`csp-ui/lib.rs:1027`) | `mission.nodes` (`mcp.rs:463`) | Dead — requires `OnRails`; panel filters `on_rails` vessels, list always empty |
| `WatchTransferWindow` | `lib.rs:5234` | **none** | `sim.command` only | Dead for player; agents get a bare Clamping event, no display |
| `plan_transfer`/`porkchop_scan`/`departure_node`/`predict_encounter` | pure fns in `transfer.rs` | none | **none** (not a `SimCommand` — can't reach via `sim.command`) | Called only from `lib.rs` tests (`~16466–16860`) |
| `TransferResource`/`TransferCrew`/`BoardCrew`/`JoinColonyInventory`/`LeaveColonyInventory`/`TransferColonyResource`/`TransferColonyCrew` | `lib.rs:5163–5201` | **none** | `sim.command`, `csp-script` API | Dead — and `BoardCrew` unreachable means crew can never exist |
| `SetAlarm` | `lib.rs:5208` | **none** | `sim.command` | Dead |

## Gaps

### G1 — CRITICAL: no player path to `OnRails` exists; the whole M5 player loop is dead
- **Problem:** `SimCommand::ToRails` is constructed only in `csp-bench/src/scenarios.rs:549` and `csp-sim` tests. No UI, app, or view code issues it. `bubble_radius_m` exists in `BalanceDef` (`csp-data/src/balance.rs:67`) and `data/balance.ron:8` (20 000) but is **never read** by csp-sim/csp-view/csp-app/csp-phys — the PLAN §2.4 "vessels go on rails at 20 km, rejoin at 18 km" auto-transition is unimplemented.
- **Evidence:**
  - Launch produces `InBubble` (`SpawnPhysicalVessel`, `csp-view/lib.rs:2213`); `Unlatch` → `InBubble`; nothing else transitions residence upward.
  - `apply_set_maneuver` requires `v.orbit()` — `Recoverable("maneuver: vessel not on rails")`, `lib.rs:6018–6020`.
  - Map panel offers only on-rails vessels: `csp-view/lib.rs:665–669` (`.filter(|(.., on_rails)| *on_rails)`, tagged P09-F2); `maneuver_panel` early-returns on empty list — `csp-ui/lib.rs:1035–1036`. In the shipped game the list is **always** empty → the "Maneuver node" window never appears.
  - `schedule_next_soi` no-ops when `v.orbit()` is `None` — `lib.rs:4729–4740`. An `InBubble` vessel publishes no `SoiTransition`/`SoiRescan` → it can never enter Duva's SOI. The bubble is a terrin-centered frame; an escaping vessel just integrates outward under 1/r² forever.
  - `View::recover_control` (`csp-view/lib.rs:1917–1958`) issues `ToBubble` only for `to_bubble_eligible` (on-rails, dominant-parent, craft-bearing) vessels — dead end (`"NO VESSEL TO RECOVER"`).
- **Impact:** Everything downstream of `OnRails` is display-only for the player: maneuver nodes, the executor, `predict_maneuver` map preview (`map.rs:1076`), SOI transitions, `rebase`, encounter prediction, debris decay, `OrbitAchieved` contract triggers on rail-stamp. A coasting vessel in space *does* warp correctly (rails-warp refusal only checks thrust/contact/atmosphere — `lib.rs:3370–3423`), so orbital flight works, but it works at 64 Hz physics ticks for the vessel's entire life. Duva is unreachable for the player, full stop.
- **Suggestion:** Minimal honest fix: implement the §2.4 auto-transition — a boundary pass converting `InBubble` vessels past `bubble_radius_m` from the active/dominant frame to `OnRails` (with 10% hysteresis), or, simpler for v1, issue `ToRails` when a vessel is above atmosphere-top + unpowered + uncontested and the player opens map view / requests rails warp. Either restores the intended residence cycle. (Note the MCP/agent path is already complete: `sim.command` → `ToRails` → `SetManeuver` → executor → SOI — only the *player* side is missing.)

### G2 — HIGH: transfer planner produces output only tests reach
- **Problem:** `plan_transfer`, `porkchop_scan`, `departure_node`, `predict_encounter`, `lambert`, `lambert_multi` are pure functions whose sole callers are `csp-sim` test code (`lib.rs:16466–16860`, `duva_reachable_in_sim_end_to_end`). No UI panel, no MCP tool (`mission.nodes` covers only Set/Delete; `sim.command` accepts `SimCommand` values, and the planner isn't a command), no `csp-script`/`csp-ai` orchestrator (`csp-ai` is 222-line ascent-only; `EngageAutopilot` also unreachable).
- **`WatchTransferWindow`:** the only planner-adjacent *command*. Its apply (`lib.rs:5234–5257`) schedules `EventKind::TransferWindow`; firing is `Ok(())` (`lib.rs:7662`) — a bare notification surfaced only through `fired_events` → §2.6.5 Clamping warp demotion (`csp-app/lib.rs:931–934`). No UI displays "transfer window: terrin→duva"; no player can issue it; an agent can schedule it but must compute the window tick itself (the planner is unreachable to agents too).
- **Impact:** PLAN M5's "Lambert/porkchop planner; transfer-window events" and the §2.11 *Transfer* job ship as dead code from the product's perspective. `departure_node`'s documented limitation — the node does NOT compose to arrival without a `predict_encounter`→re-plan correction loop (`transfer.rs:35–43`, `1529–1537`) — is doubly moot: there is no orchestrator to run the loop, and no caller at all.
- **Suggestion:** Smallest viable slice: a map-side "transfer" panel calling `plan_transfer` + `departure_node` against the focused vessel's orbit and issuing `SetManeuver` + `WatchTransferWindow`. Expose `predict_encounter_excluding(plan.conic, …)` in the same panel so the missed-approach correction loop is at least player-visible. Gated on fixing G1 first — the panel would otherwise always show an empty vessel list.

### G3 — HIGH: resource/crew transfers and colony joins unreachable
- **Problem:** `transfers.rs` (1687 LOC) implements the §3.5 process faithfully — debit-at-apply with inbound reservations, `TransferComplete` `Clamping` event, best-effort landing, colony store re-anchors, roster pairing, latch-merge remap, breakup partitioning — but the entire command family (`lib.rs:5163–5201`) is reachable only via MCP `sim.command` (`mcp.rs:532–563`) and `csp-script`'s `ControlApi` (which is itself unreachable — no script editor/console).
- **Evidence:** zero hits for `TransferResource|TransferCrew|BoardCrew|JoinColony|LeaveColony|TransferColony` in `csp-ui`, `csp-view`, `csp-app` (other than mirrors).
- **Impact:** §3.5's "ResourceTransfer is a sim process" and M6's "crew transfer" don't exist for the player. Worse, `BoardCrew` is also unreachable → `vessel.crew` is always empty → `TransferCrew` is unreachable *and* inapplicable, life support (toggled ON at boot, `csp-app/lib.rs:565`) is inert, and `TransferColonyCrew`'s roster requirements can never be met. The Rask colony's resupply path (`ColonyDelivery` trigger, `transfers.rs:1047–1064`) is unreachable both by colony UI absence and by the vessel side.
- **Suggestion:** Vessel-context UI (map focus or flight) with "transfer" affordance when `transfer_linked` is true; a crew manifest surface once `BoardCrew` gets a producer (roster/hiring UI per cluster PRD-06's `HireCrew` gap).

### G4 — MEDIUM: maneuver node UI produces a burn but no *usable* transfer
- **Problem:** `maneuver_panel` (`csp-ui/lib.rs:1027–1090`) is three `DragValue`s (pro/nrm/rad) plus "T+ s". The map does draw `predict_maneuver`'s post-burn conic (`csp-view/src/map.rs:1076`) — the one useful preview — but there is no encounter prediction display, no burn-time estimate, no Δv-vs-remaining readout, no node placement on the orbit, and no transfer planning. Even with G1 fixed, this panel alone can't take a player to Duva — `SetManeuver` also rejects `orbit.parent != dominant` (`lib.rs:6021–6025`), so **no burns inside another body's SOI at all** (documented v1 scope, `maneuver.rs:37–43`; and `ToBubble`/`fire_maneuver_start` have the same dominant-parent requirement, `lib.rs:5925`, `6148`).
- **Impact:** the maneuver machinery is honest and tested (`maneuver_executor_delivers_commanded_dv`, the Duva end-to-end test), but the player-facing slice is a stub against the PRD's maneuver-node mechanic.
- **Suggestion:** after G1, surface `predict_encounter` on the predicted post-node conic (periapsis, `impacts`, `v_inf`) in the panel/map; it's one call against `PatchedConics`.

## Defects

### D1 — MEDIUM: `apply_to_rails_at` accepts atmosphere-dipping conics (gate weaker than `SetManeuver`'s)
- **Location:** `lib.rs:5656–5693` vs `lib.rs:5996–6049`.
- **Problem:** `apply_to_rails_at` rejects a vessel *currently* below `atm.top_m` (`5656–5660`) and requires `r_p > radius_m` (surface clearance, `5686–5693`) — but **not** `r_p > radius_m + atm.top_m`. A `ToRails` (or executor completion) can stamp a conic whose periapsis dips into atmosphere; on rails there is no drag and no re-entry event, so the vessel orbits forever through the atmosphere untouched. `apply_set_maneuver`'s `min_node_r` *does* include `atm_top` — and its comment (`5994–5997`) explicitly claims "the same bound `ToRails` applies", which is false.
- **Impact:** unphysical undecayed rails orbits; an inconsistency the code comments already acknowledge the wrong way. Currently reachable only via MCP `sim.command ToRails` and `finish_burns` (a burn ending on a thin-atmosphere-dipping conic — e.g., a suicide burn that doesn't quite hit the surface — leaves the vessel rails-resident on an aero-crossing conic).
- **Suggestion:** raise the stamp gate to `r_p > radius_m + atm_top` (matching `min_node_r`) or stamp and then schedule an atmosphere-entry transition — the former is the honest v1 answer.

### D2 — LOW: `apply_to_rails_at` samples ground contact at the wrong tick when called from `finish_burns`
- **Location:** `lib.rs:5666–5673`, called at `4506` with `epoch = t_next`.
- **Problem:** the contact predicate does `physics.backend.set_sim_time(self.clock)` — but inside `step_bubble_range` the clock is restored to range start `now` after the tick loop (`3713–3715` doc), while the body's state is `t_next`'s. On a rotating body the heightfield contact surface is body-fixed-phase; the check evaluates terrain at `now`'s phase against the `t_next` pose. Phase error = `ω·(t_next − now)` — up to a few % of a rotation for a full frame budget.
- **Impact:** the post-burn "live contact" refusal can false-positive/false-negative near the surface by a small terrain-phase offset. Same latent issue for the `com_world`-based conic — pos/vel are `t_next`'s (correct), only the *terrain sample phase* is off.
- **Suggestion:** pass the stamp `epoch` through to the `set_sim_time` call in `apply_to_rails_at` (it already knows `epoch`; use `SimTime::from_ticks(epoch.ticks().max(self.clock.ticks()))`).

### D3 — LOW: boundary-skimming `d == 0` arm in `scan_candidate` can report a spurious `Enter`
- **Location:** `orbit.rs:1358–1363`.
- **Problem:** `d == 0.0` is checked before `prev_d == 0.0`, so a sample sequence `prev_d == 0, d == 0` (riding exactly on the boundary) yields `entered = prev_d >= 0.0 → true` → `SoiEvent{Enter}` at `sample_t` on a boundary the conic merely grazes. Requires a physically exact tangency — `d` hitting exactly `0.0` in f64 — so practically unreachable, but the arm's direction inference is wrong for the degenerate case.
- **Impact:** theoretical only; the `tick > now` gate and wrong-side tolerance already bound the blast radius.
- **Suggestion:** treat `prev_d == 0` first (direction from the *next* nonzero sample), or require `d < 0` after the touch to call it `Enter`.

### D4 — LOW: `departure_node`'s output provably misses arrival — and nothing runs the required correction loop
- **Location:** `transfer.rs:36–43`, `1529–1537`, `1646–1737`.
- **Problem:** documented honestly: the burn can fire up to one parking period before `departure_tick` and SOI egress is modeled as instantaneous, so the vessel misses `r2` at `arrival_tick` without mid-course correction; the doc states the `predict_encounter`→re-plan loop is "REQUIRED, not optional". No orchestrator exists (the §2.11 Transfer job was never built; `csp-ai` is ascent-only).
- **Impact:** even for an agent driving `ToRails`/`SetManeuver` via MCP, a planner-composed transfer can't land an `Enter{duva}` without hand-rolled correction logic. The ejection math itself is sound (cone∩plane, prograde-consistent selection, fixed-point `r_b`, stale-burn refusal at `1631`).
- **Suggestion:** acceptable v1 scope *if* G2 lands a player-visible `predict_encounter` loop; otherwise mark the planner test/bench-only in the public API docs rather than letting its silence imply readiness.

### D5 — NOTE: `SetManeuver`/`ToBubble`/`fire_maneuver_start` are dominant-parent only
- **Location:** `lib.rs:6021`, `5925`, `6148`.
- **Problem:** the executor and materialization require `orbit.parent == physics.dominant`. Vessels in another body's SOI can't burn or materialize — a hard ceiling on the interplanetary loop *even if* G1 were fixed (a vessel couldn't circularize at Duva; the M5 "landable" cut is documented, this extends it to "no non-dominant burns at all").
- **Impact:** consistent, documented v1 scope — but it means the fix list for interplanetary flight is: auto/issued `ToRails` → planner UI → *and* a second bubble world or dominant switching. Worth surfacing in PLAN status so the remaining work is correctly counted.
- **Suggestion:** document in `SimCommand::SetManeuver`'s doc-comment that dominant-parent is the *single-bubble* limitation, not a maneuver-system property; track multi-dominant as post-v1 like the Duva-landing cut.

## Correctness notes (verified, no action needed)

- **Solver:** `propagate`/`solve_universal`/`universal_f`/`initial_chi`/`stumpff` implement §2.8 literally — Δt mod-period (with the non-finite-period guard, `orbit.rs:368–385`), Stumpff series/closed-form split at 1e-6, Laguerre–Conway n=5 with maintained bracket and bisection/expansion fallback, 64-iter cap → `Recoverable`. `tick_delta_seconds` uses i128 (`804–811`). `cartesian_to_elements` handles circular/equatorial/parabolic conventions (`a := −p`); `periapsis_state` funnels every decode path.
- **SOI scan:** `stride_ticks` = `ceil(0.1·r_soi/v_rel_max·HZ)`, candidates = `{parent} ∪ children`, Brent on the tick axis to ≤1 tick, `SOI_SCAN_MAX_SAMPLES` budget → `Recoverable` (never silent `None`), `suppress`/`exclude` knobs for post-rebase artifacts, `scan_horizon` = 4·tightest-period clamped by the sample cap, `scan_strides` overlay parity. `resolve_soi_residence` (5799–5898) walks Enter/Exit with reverse-transition suppression for epoch>clock stamps.
- **`rebase`** (`orbit.rs:968–1037`): textbook patched-conics stitch — Enter computes `rv−rb` vs child elements in the old-parent frame; Exit composes `vessel_in_parent + parent_in_grandparent`. `predict_encounter` reuses the same seam (prediction ≡ live transition).
- **Executor:** `fire_maneuver_start` (6119) materializes at the node tick, freezes `burn_dir`, arms `ActiveBurn`; `drive_node_burn` (8412) books `throttle·|F_net|·dt/m` against the post-debit mass with a stalled-burn terminal arm; `finish_burns` (4479) consumes the node and best-effort converts to rails with the `SoiRescan` republish fallback on post-stamp failure. Degraded `ManeuverNodeStart` drops the node (7492) — no zombie intent.
- **Transfers:** `debit`/`credit` apply the §2.4 contents quantum exactly (`apply_bucket_delta` — delta at write time, idempotent, inertia scaled), `rebody` preserves the CoM-vel convention (`vel + ω×rot·Δcom_local`), link clauses match §3.5 (jointed / in-bubble ≤ `transfer_radius_m` / latched same-body chord), inbound-reservation accounting prevents overbooking, `TransferComplete` landing is total (dest → source fallback → honest loss), latch merge remaps in-flight ends, colony store re-anchors + `ColonyDelivery` trigger are gated on `colony_live`.
- **`bubble.rs`:** `surface_pose_capture`/`restore` round-trips `(lat,lon,alt,R(−ωt)·rot)`; `vel` stamps the **CoM** co-rotation (`pos + rot·com_local` — the material-point rule), `surface_upright` builds a level ENU pose with pole handling; `CraftDef`/`CraftHashMemo` (clone-clears, eq-always-true, `serde(skip)`) is a correctly-guarded cache.

Minor non-defects: `linked` clause 2 measures body *origins* (`pos`), not CoM/hull extent — documented convention; `latched_pos` uses the dominant env radius, correct since latch only exists in the dominant frame (`vessel_colony_linked` properly uses `colony_env[co.body]`); `hash_transfer` omits `id` — the id is the arena key, hashed by the arena walk.

## Summary

| # | Severity | Issue |
|---|---|---|
| G1 | Critical | No player path to `OnRails` — `ToRails` unissued, `bubble_radius_m` auto-transition unimplemented → maneuver nodes/executor/SOI/interplanetary all dead (`lib.rs:6018`, `4729`; `csp-view/lib.rs:669`; `csp-ui/lib.rs:1035`) |
| G2 | High | Transfer planner + `WatchTransferWindow` unreachable — pure fns called only by tests; no MCP/UI hookup (`transfer.rs`; `mcp.rs:463–563`; `lib.rs:7662`) |
| G3 | High | `TransferResource`/`TransferCrew`/`BoardCrew`/colony joins unreachable (`lib.rs:5163–5201`); crew can never board → LS + crew transfers inert |
| G4 | Medium | Maneuver panel is bare (T+ s + 3 DragValues) — no encounter prediction, no burn estimate, dominant-parent-only executor (`csp-ui/lib.rs:1027`; `lib.rs:6021`) |
| D1 | Medium | `apply_to_rails_at` stamps atmosphere-dipping conics — missing `atm_top` term its own comment claims (`lib.rs:5689` vs `5999`) |
| D2 | Low | Post-burn contact check samples terrain at range-start phase, not `t_next` (`lib.rs:5668`, `4506`) |
| D3 | Low | `scan_candidate` `d==0` arm infers `Enter` on boundary-skimming degenerate (`orbit.rs:1358`) |
| D4 | Low | `departure_node` can't compose to arrival; required correction loop has no owner (`transfer.rs:1529`) |
| D5 | Note | Executor/materialization are dominant-parent only — hard ceiling beyond G1 (`lib.rs:5925`, `6021`, `6148`) |

**Bottom line:** the review question's answers are: transfer planner — *no player path exists* (test-only, and even agents get only `WatchTransferWindow` + raw `sim.command`); maneuver node UI — produces correct burns *in principle* but is unreachable because no vessel is ever on rails, and is minimal regardless; resource/crew transfers — unreachable (MCP/script only, and crew can't board); bubble→rails→bubble — incomplete for the player (one-way dead: `ToBubble` exists, `ToRails` has no issuer and the §2.4 auto-transition isn't implemented); SOI/encounter correctness — solid machinery, correct stitch and prediction parity.
