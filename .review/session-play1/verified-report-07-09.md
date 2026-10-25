# Verified Report — P07 / P08 / P09 (session-play1)

Verifier read the actual code for every finding in `review-report-07.md`,
`review-report-08.md`, `review-report-09.md`. Verdicts: **CONFIRMED** = real as
stated; **DISMISSED** = false positive / wrong mechanism; **PARTIAL** = real
issue but materially mis-described. Cross-check: P07-F1's `sfx_edges` half was
already confirmed independently as P04-F4 in `verified-report-04-06.md`.

---

## P07 — Flight visuals (`review-report-07.md`)

| # | Severity | Verdict | One-line verdict + evidence |
|---|----------|---------|------------------------------|
| F1 | MEDIUM | **CONFIRMED** | `sfx_edges` baselines on `focus_vessel(vessels)` = `vessels.last()` (flight.rs:1218, 1290-1292) while `flight_present`'s real focus is pin-first (`focus_override.target.and_then(find).or_else(focus_vessel)`, flight.rs:1128-1131); the pin set by `recover_control` (lib.rs:1542-1544) releases only when the pinned vessel leaves the staged set after `seen` (flight.rs:1121-1127), and `AudioBackend::update` drives engine/plasma voices off `snap.vessels.last()` (audio.rs:509) — so after `recover_control` with any newer co-staged vessel, cues and audio track the wrong vessel permanently for that flight. Same mechanism already confirmed as P04-F4; this report adds the audio-voice half. Real. |
| F2 | MEDIUM | **CONFIRMED** | Dust base is `env.body_center + dir * (env.radius_m + 0.3)` (flight.rs:1763) — the bare datum — while rendered ground sits at `radius_m + h·amplitude_m` (terrain.rs:306, `AMPLITUDE_FRAC = 0.004` at terrain.rs:35/87 → ±~2.4 km at 600 km); `surface_floor_radius` (terrain.rs:260) already answers the same question for the camera floor (flight.rs:1041-1047) and `terrain_cfg`/`terrain_cache` are already in `extract_flight` scope (flight.rs:1556-1557) — the fix's plumbing exists and was simply not applied. Real. |
| F3 | LOW | **CONFIRMED** | `plasma_w_m2` rewrites `CONVECTIVE_HEAT_FRACTION * 0.5 * ρ * v³` inline (flight.rs:675-681) rather than sharing a helper with the two copies in csp-phys/thermal.rs — inputs are shared via `flow_sample`, the transform is a third textual copy; drift risk is real though parity holds today. |
| F4 | LOW | **CONFIRMED** | `PLASMA_ONSET_W_M2`/`PLASMA_FULL_W_M2` literals duplicated at audio.rs:96-98 vs flight.rs:46-48 (5.0e3 / 8.0e4 — equal today, no mechanical tie), and the sun fallback `(0.4, 0.3, 0.86)` appears at flight.rs:593 and :1621; real duplication-of-constants class, cosmetic. |
| F5 | LOW | **CONFIRMED** | `part_kind` probes `wp.engine.is_some() || has Thrust site` (flight.rs:822-827) while `classify_weld_part` checks Thrust sites only (part_mesh.rs:196-201); they agree today only because resolve.rs:1697-1722 sets both in one arm — divergence-prone as described, no current-content failure. |
| F6 | INFO | **CONFIRMED** | `stage_latched` (flight.rs:705-709, 727-761) restores `SurfacePose` records without comparing `pose.body` (bubble.rs:147) to `dominant_body()` — the invariant appears to hold by construction, and the flag asks only that it be confirmed; cheap-guard observation is accurate. |

## P08 — Terrain & atmosphere (`review-report-08.md`)

| # | Severity | Verdict | One-line verdict + evidence |
|---|----------|---------|------------------------------|
| F1 | MEDIUM | **CONFIRMED** | `select_patches` computes `horizon = acos(R/(R+alt))` on the datum sphere (csp-render/terrain.rs:312) and culls when `acos(sub·dir0) > horizon + atan(cube_size·0.75)` (:329-331) — no amplitude term exists anywhere (`LodConfig` fields are only `max_level`/`split_factor`/`max_patches`, :250-267; signature takes only `radius_m`), and the `continue` drops the whole subtree, so terrain elevated up to `amplitude_m` (±2.4 km → ~`sqrt(2·amp/R)` ≈ +5.1° past the datum limb at Terrin) inside a culled patch never draws and pops at the boundary. Real. |
| F2 | MEDIUM | **CONFIRMED** | Sim ground contact is the bare datum sphere — `depth = self.env.radius_m - dist` in `generate_contacts` (csp-phys/simple.rs:1373), same in `ground_contact` (:1242), module doc states it outright ("sphere of `radius_m` centred at `body_center`", :65) — while the renderer draws `radius_m + h·amplitude_m` ±2.4 km; only the pad site is pinned to the rendered surface (`launch_site_sits_on_rendered_ground`, tests/pad_spawn.rs:127-140, ±5 m), so off-pad touchdowns embed into positive relief or float over dips. Real (and consistent with P07-F2's datum-anchored dust). |
| F3 | LOW | **CONFIRMED** | Same defect as P07-F2, correctly re-flagged in this cluster's scope: `base = env.body_center + dir·(radius_m + 0.3)` (flight.rs:1763) ignores `surface_floor_radius` (terrain.rs:260) already used at flight.rs:1042. Real — duplicate of P07-F2, not a false positive. |
| F4 | LOW | **CONFIRMED** | `loaded.legend.entries.iter().take(16)` (csp-view/terrain.rs:164) silently truncates; `slot_of` entries for ≥17th biomes stay 255 → shader falls through `slot < 255u` to the ramp palette (terrain.wgsl:260) with no warn — verified `check_biomes` bounds uniqueness/coverage only, not count. Real data-driven silent-loss class. |
| F5 | LOW | **CONFIRMED** | `patch_might_hold_sea` (csp-view/terrain.rs:224-242) gates ocean emission on a 5×5 direction grid minimum + `max(amp·0.25, 250 m)` margin — a sub-grid deep basin between samples emits no ocean while the terrain patch still draws → dry hole below sea datum; heuristic bound is documented but the miss produces a real hole as described. |
| F6 | LOW | **CONFIRMED** | `TerrainCache::field` keys a single `(body_id, seed)` slot (csp-view/terrain.rs:180-207) — an A→B→A dominant switch re-bakes the 1024×512 field (~524k `height_normalized` evals) per hop; `flight_present` runs the camera solve in every mode (flight.rs:1096+, no `MapMode` gate) so the first bake lands on the first staged frame regardless. Real bounded-hitch observation. |

## P09 — Map view & camera (`review-report-09.md`)

| # | Severity | Verdict | One-line verdict + evidence |
|---|----------|---------|------------------------------|
| F1 | MEDIUM | **CONFIRMED** | `set_mode` refocuses via `bodies.get(focus)` (csp-view/lib.rs:1414-1421) — a vessel focus (`focus = bodies.len() + i`, map.rs:238-240, 326-327) returns `None` → `refocus_dist = 6.0e10` system fallback, while the vessel-aware `MapInput::refocus` frames `3·|a|` clamped to `[MIN_DISTANCE_M, MAX_DISTANCE_M]` (map.rs:377-387); `MapKeyEvent::ToggleMode` — the only caller of `refocus` on mode flip — is produced by **no production code** (grep: enum def map.rs:137, apply arm :286, tests :1883/1885/2072 only); every player toggle routes `KeyAction::ToggleMap` → `view.set_mode` (csp-app/lib.rs:1755-1761, top-bar :938-944). M-out-M-in on a vessel focus snaps to 6×10¹⁰ m. Real; secondary hardcoded `1.0e5`/`2.0e11` vs private `MIN/MAX_DISTANCE_M` (map.rs:83-84) also confirmed. |
| F2 | MEDIUM | **CONFIRMED** | `capture_sync_input` stages InBubble vessels into `MapSyncInput::vessels` via a fitted osculating conic (map.rs:1202-1214, `v.body()?` arm) — the `(VesselId, OrbitState, Option<ManeuverNode>)` tuple carries no residence flag; `map_panels` maps **all** staged vessels to `ManeuverVessel{id, node}` (csp-view/lib.rs:487-494, struct has no residence field at csp-ui/lib.rs:780-785) and the ComboBox offers every one (csp-ui/lib.rs:833-838); "Set node" emits `SetManeuver` (csp-ui:866-872 → csp-view/lib.rs:499-505) which rejects `v.orbit() == None` with `Recoverable("maneuver: vessel not on rails")` (csp-sim/lib.rs:5587-5588), and rejections surface only as `log::warn!` in the `report.rejected` drain (csp-app/lib.rs:836-842) — no `flight_notice`. Silent-refusal class exactly as described. |
| F3 | LOW | **CONFIRMED** | `clicked` gate in `map_pointer_input` evaluates `egui_wants_pointer_input()` at the release frame (csp-view/lib.rs:552-615); the `drag_over_ui` press-origin latch (:570-575) covers only Middle/Secondary pan buttons, so a primary press landing on a panel edge pixel + release a few px off (inside egui's click threshold) passes the gate and fires `FocusBody`/`FocusVessel` — hole in the documented "clicks on panels are egui's" contract, narrow window. Real. |
| F4 | LOW | **CONFIRMED** | Focus resolution uses `scene.vessel_pos.get(i)/scene.body_pos.get(i)` `.flatten().unwrap_or(DVec3::ZERO)` (map.rs:683-688) — a transient `state_at` failure on the focused target lands the look-at at the system origin (the InBubble osculating fit, map.rs:1202-1214, can produce elements that fail to solve); no last-good-focus fallback exists. Real rare camera-jump class. |
| F5 | LOW | **CONFIRMED** | `map_present` sets `ui.mode` then early-returns in Flight mode without clearing `labels`/`picks`/`orbit_info` (map.rs:619-622) — stale until next map frame; inert today because all consumers gate on mode, and `MapKeyEvent::ToggleMode` is test-only per F1's grep. Both halves accurate. |

---

## Summary

| Finding | Severity | Verdict |
|---------|----------|---------|
| P07-F1 | MEDIUM | CONFIRMED — wrong-vessel SFX edges + audio voices under the recover_control pin (sfx half already confirmed as P04-F4) |
| P07-F2 | MEDIUM | CONFIRMED — dust ring at datum `radius_m + 0.3`, terrain-displaced ground ±2.4 km; `surface_floor_radius` tap exists in scope but unused |
| P07-F3..F6 | LOW/INFO | CONFIRMED — all four minor items accurate as described |
| P08-F1 | MEDIUM | CONFIRMED — datum-only horizon cull drops whole subtrees; ~+5.1° of elevated limb relief invisible, pops at boundary |
| P08-F2 | MEDIUM | CONFIRMED — sim contact = bare datum sphere (simple.rs:65, 1373); only pad site pinned to rendered ground; off-pad embeds/floats |
| P08-F3 | LOW | CONFIRMED — same datum-vs-terrain dust defect as P07-F2 (duplicate flag, still real) |
| P08-F4..F6 | LOW | CONFIRMED — all three accurate as described |
| P09-F1 | MEDIUM | CONFIRMED — `set_mode` refocus hits the body half only; vessel focus → 6.0e10 m snap; `ToggleMode` dead in production |
| P09-F2 | MEDIUM | CONFIRMED — maneuver panel offers InBubble vessels; `SetManeuver` rejects "not on rails" → `log::warn!` only, silent to the player |
| P09-F3..F5 | LOW | CONFIRMED — all three accurate as described |

**No dismissals.** All six focus findings and every secondary finding in the
three reports hold up against the actual code. Note P07-F2 and P08-F3 are the
same defect flagged twice (dust at datum), and P08-F2 is its sim-side parent
issue — the three should be tracked as one fix thread.
