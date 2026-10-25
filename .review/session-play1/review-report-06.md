# Review Report — P06: Launch, pad spawn & starter seed

Scope: `crates/csp-view/src/lib.rs` (`launch_craft` L1588–1762, `PAD_CLEARANCE_M` L53–58, `stock_flight_craft` L1981–2080, `seed_starter_craft`/`new_demo` L912–954), `crates/csp-view/src/demo.rs`, `crates/csp-app/src/lib.rs` (L301–327, L946–1077), `crates/csp-view/tests/pad_spawn.rs`. Cross-checked against `csp-sim` (`apply_spawn_physical` L5016–5180, `bubble.rs::surface_pose_restore`/`surface_upright`/`surface_pose_capture`, `economy.rs`), `csp-phys` (`weld.rs`, `simple.rs` contact model, `session.rs`), `csp-ui/src/vab.rs`, and both `sites.ron`/`balance.ron` sets.

## Findings

### F1 — HIGH — Second launch onto an occupied pad spawns *inside* the parked craft; identical craft then ghost-overlap forever

- **File:** `crates/csp-view/src/lib.rs:1612–1762` (`launch_craft` — no occupancy query anywhere); `crates/csp-sim/src/lib.rs:5016–5180` (`apply_spawn_physical` — no overlap check either).
- **Problem:** Nothing checks whether the launch site is already occupied. A second `launch_craft` computes the identical site/`z_min`/phase pose and spawns a second vessel interpenetrating the first.
- **Evidence:** Verified empirically (scratch test, since removed): `seed_starter_craft` leaves the same craft loaded in the VAB session → calling `launch_craft` again → 10 s of sim. Both vessels end `SurfaceLatched` at the *exact same* pose record — `lat=-0.092, lon=2.916, alt=4.30` — with all 3 parts each. Coincident vertices produce depth-0 `point_penetration` hits, which the pair-contact rule rejects (`simple.rs` module doc: "a collider vertex of `a` *strictly inside* a facet volume of `b`"; generation requires positive depth), so no contact is ever generated and the two identical hulls sit perfectly overlapped indefinitely. For a *non-identical* craft (player edits before re-launching), the overlap is non-coincident → deep vertex penetration → violent pair impulses / joint failures at spawn.
- **Impact:** This is reachable from the *default first-session flow*: the starter craft is seeded on the pad AND pre-loaded in the VAB; the player opens the VAB, presses LAUNCH, and gets a second craft inside the first. Consequences: a silent duplicate vessel (arena count, recovery payout, HUD vessel/debris counts all double-count it), z-fighting render, throttle/stage commands targeting whichever holds focus, and — for edited craft — an explode-on-spawn event that reads as a physics bug. `stage_plan.rs:138–148` even exercises a second launch but only steps 1 tick, so the overlap is never observed.
- **Suggestion:** Pre-enqueue occupancy check in `launch_craft`: scan `self.sim.vessels()` for a `SurfaceLatched`/`InBubble` vessel whose pose is within a spawn-footprint radius (a few metres plus the weld's max hull extent in x/y) of the site at the current phase, and `launch_refused("pad occupied — recover or move the parked craft")`. A belt-and-braces overlap rejection in `apply_spawn_physical` would cover scripted spawns too.

### F2 — MEDIUM — Post-enqueue reject paths still land the player in Flight with no vessel and no notice

- **File:** `crates/csp-view/src/lib.rs:1707–1718` (weld pre-check), `:1741–1761` (enqueue + unconditional mode switch); `crates/csp-sim/src/lib.rs:5025–5111` (the remaining reject gates); `crates/csp-app/src/lib.rs:836–842` (rejects surface only as `log::warn!`).
- **Problem:** The "refuse BEFORE enqueue" fix only mirrors the weld check. `apply_spawn_physical` can still reject *after* `launch_craft` has enqueued and switched to `ViewMode::Flight`:
  - **Insufficient funds** (L5077–5082; debit at L5154) — real M7-C economy: `funds_micros` is finite (start 1e12 µ = 1M funds, `balance.ron:54`; starter craft ≈ 4,300 funds). A session that has spent down funds → LAUNCH → `Recoverable` at drain → empty flight view.
  - **`max_vessels` cap** (L5030, `vessel_slots_in_use >= caps.max_vessels`, cap = `max_tracked_vessels: 512`) — reachable in long sessions with debris.
  - `check_tank_masses` (L5048), aero-table build failure (L5101–5108), non-finite kinematics (L5035–5043).
- **Evidence:** The reject lands only on `AdvanceReport.rejected`, which csp-app drains to `log::warn!("command rejected on originating channel…")` — no `flight_notice`, no mode rollback. `fire_stage`'s "LAUNCHED CRAFT NOT SPAWNED" cue exists but only fires if the player happens to press the stage key.
- **Impact:** The exact bug class the weld pre-check was added for ("enqueueing the spawn anyway would land the player in Flight with no vessel", L1699–1701) — still live for the two most player-reachable remaining gates (funds, vessel cap). Player experience: LAUNCH button appears to work, mode flips, flight HUD shows `has_vessel: false` with zero explanation.
- **Suggestion:** Mirror the funds gate before enqueueing — `csp_sim::economy::craft_cost_micros(&craft) > self.sim.funds_micros()` is fully readable at L1707's weld site. The vessel-cap check needs `vessel_slots_in_use`/caps made public on `World` (currently `pub(crate)`), or a `caps` accessor; then refuse identically. Aero/tank-mass gates are lower-value mirrors — at minimum, the app could surface a `rejected` record carrying `SpawnPhysicalVessel` on the player's source id as a `flight_notice` fallback.

### F3 — LOW — Equator fallback spawn ignores the rendered heightfield

- **File:** `crates/csp-view/src/lib.rs:1685–1693`.
- **Problem:** A dominant body with no `launch: true` site falls back to `(lat, lon, alt) = (0, 0, 0)` — the datum. The pad-burial bug this cluster fixed applies here unchecked: rendered terrain at the equator can sit tens of metres over the datum.
- **Evidence:** The ±5 m pin (`pad_spawn.rs:107–141`) only covers the authored launch site; nothing measures the fallback point. Both shipped data sets have a `launch: true` site on terrin, so this only fires on broken/removed-site data — but the fallback exists precisely to keep that case playable.
- **Impact:** Spawn renders buried inside terrain or floating (sim still contacts the datum — correct physics, wrong visuals — the same confusing symptom as the shipped bug).
- **Suggestion:** Where a `terrain::TerrainConfig` exists for the dominant body, sample the baked field at the fallback point and either warn or clamp `site_alt` to it (with a comment that the sim still contacts the datum — the honest fix is authoring a site).

### F4 — LOW — `z_min.min(0.0)` converts a raised-origin craft into a spawn-day drop

- **File:** `crates/csp-view/src/lib.rs:1707–1717`.
- **Problem:** For a craft whose collider sits entirely above its origin (`z_min > 0` — a root part authored with an upward-offset collider, or re-rooted oddly), the guard places the origin at site altitude and the craft free-falls `z_min` metres before first contact — reintroducing, for that craft class, the impact-spike/joint-failure bug `PAD_CLEARANCE_M = 0.0` exists to kill (L53–58).
- **Evidence:** The comment documents the trade ("keeps an origin-below-parts craft from driving the origin itself under the datum"), and normal VAB output always has `z_min < 0` (stack grows −z from the root, `vab.rs:717–720`), so this only bites unusual craft. Deliberate choice, but the cost — a `z_min`-metre drop with full co-rotation velocity — deserves the note.
- **Impact:** Rare; a weak-jointed craft could shed parts on spawn like the shipped bug.
- **Suggestion:** Consider letting the origin go below datum (remove the clamp): the belly still rests exactly at `site_alt`, `SurfacePose.altitude_m` is signed (`bubble.rs:152`), and `surface_pose_capture` handles `r < radius` fine. If below-datum origins are unacceptable elsewhere, at least refuse with a reason rather than spawn-and-drop.

### F5 — NIT — Stale doc line on `UiOutbox::warp_step`

- **File:** `crates/csp-view/src/lib.rs:223–225`.
- **Problem:** `warp_step` carries a duplicated "The VAB's LAUNCH button — `csp-app` calls `View::launch_craft`…" doc line above its real doc — leftover copy-paste.
- **Impact:** None — doc noise.
- **Suggestion:** Delete the LAUNCH lines from `warp_step` (they correctly appear again on `launch` at L229–230).

## Verified correct (no finding)

- **Spawn pose math** (`lib.rs:1729` + `bubble.rs:235–261`): `altitude_m = site_alt + clearance − z_min` is consistent with the vertex-only datum contact (`simple.rs` module doc) — weld hulls are body-frame (`weld.rs:634–665`), `+Z` is the VAB up, and off-axis belly vertices float only ~(x²+y²)/2R ≈ µm. `min(0.0)` fold handles empty-hull `+INF` correctly.
- **`vel` is the CoM slot**: `vel = body_spin × (pos + rot·com_local − body_center)` (`bubble.rs:256–258`) matches `com_world()` and the integrator's CoM advance (`simple.rs:1344–1355`). `ang_vel = rot⁻¹·body_spin` (body frame) matches `WeldedBody` convention.
- **Spin phase at spawn**: `surface_pose_restore` is called at `self.sim.clock()`; `step_frame`'s `drain_boundary` runs at the *current* clock before any advance, and deferred-event flushing keeps the clock at T while it postpones drains — so enqueue-time phase == apply-time phase exactly (verified: second craft latched at the site's authored lat/lon to 1e-3 rad).
- **Weld-failure refusal before enqueue**: graph/analysis/`effective` resolution and `weld()` all run before `enqueue_command`; every `Err` path returns before `insert_resource(plan)`/`set_mode(Flight)` — no silent Flight-with-no-vessel for the covered cases (F2 covers the uncovered ones). `Err` return exists for no-notice-audience callers (`launch_refused` L1881–1885; `seed_pad_craft` propagates it, `demo.rs:206–211`).
- **Stage-plan/focus-pin interaction**: `StagePlan::for_launch` is captured before enqueue with `min_id = vessels().next_back()+1` — excludes all live vessels; `.rev().find()` newest-match is consistent with the newest-spawn rule; `insert_resource(plan)` replaces any prior plan so a stale plan can't fire a previous launch; `clear_flight_focus_override()` drops the T-pin before the mode switch (`lib.rs:1753–1758`). `stage_order()` descending + `remove(0)` pop order verified (`session.rs:1424–1430`).
- **Launch-site selection**: `config().sites.into_values().find(|s| s.body == dominant && s.launch)` — deterministic `BTreeMap` order, validator enforces ≤1 launch site per body, non-launch sites excluded. `dominant`/`env` consistency verified (`physics.dominant` ↔ `Environment::for_body(terrin, ZERO, …)`, `lib.rs:788–797`).
- **Terrain-vs-datum**: authored site at (−0.092, 2.916) sits where baked terrain is +0.02 m over datum; `pad_spawn.rs:107–141` pins |Δ| ≤ 5 m against the texel-exact `HeightField` mirror — the right pin for the class.
- **`PAD_CLEARANCE_M = 0.0`**: spawns at depth 0 → one tick of ~2 mm micro-sink then contact (the `depth > 0` strictly-inside rule means grazing generates nothing tick 1, caught tick 2 by design). Pad test confirms rest → latch and throttle-unlatch.
- **Starter seed**: `STARTER_CRAFT` ids exist in both data sets; `place_attach` re-selects the new node so pod→tank→engine stacks correctly (`vab.rs:686`); `recompute()` inside `launch_craft` makes the separate `v.recompute()` in `seed_pad_craft` redundant-but-harmless. Seed failure surfaces as `ViewError::Seed`/`Demo` → startup error.
- **Demo enforcement**: `part_limit` checked in `place_root`/`place_attach`/`load_bytes` (`vab.rs:434–441, 660, 683, 628`); `file_io` checked in `load_bytes` (L624); app outbox gates demo save/load with honest notices (`csp-app/lib.rs:946–983, 1061–1077`); read-only gates launch before `launch_craft` (L1029–1047) — all halves present.
- **Stock fallback** (`stock_flight_craft`): `engine: None` → `engine_can_draw` returns true (`sim/lib.rs:7846–7871`) so the unmetered 250 kN site keeps its throttle channel by the documented debug convention; `part_costs` parallel (4/4); funds check passes (25,800 ≤ 1M funds).

## Commands run

- `cargo test -p csp-view --test double_launch_scratch` (scratch test added then deleted): proved F1 — second launch on the occupied pad latches a second, perfectly co-located vessel.
