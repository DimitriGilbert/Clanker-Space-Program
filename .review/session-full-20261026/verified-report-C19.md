# Verified Report C19 — verification of review-report-C19 against the working tree

Verifier scope: re-read every cited site in the current tree, re-derived the plasma math from shipped constants, traced the auto-rails → focus → camera/extract chain end-to-end, and checked the egui 0.37 pointer semantics from the vendored crate source. `cargo test -p csp-view`: all suites pass (60 unit + demo 4 + flight_focus 1 + flight_plume 1 + pad_spawn 3 + part_silhouettes 2 + vab_latency 1), consistent with the report.

Result: 6 CONFIRMED, 1 DISMISSED (Finding 3).

---

### Finding 1: Plasma display window fires on every ascent — ascent glows brighter than reentry — CONFIRMED

**Original**: `PLASMA_ONSET_W_M2 = 5e3` makes a normal mid-ascent glow (i ≈ 0.25–0.5 through 7–24 km) while an actual 40 km reentry glows less (i ≈ 0.33); the §3.7 effect's identity is *reentry* plasma.

**Verification**: Re-derived from shipped constants, independently of the reviewer.
- Constants: `PLASMA_ONSET_W_M2 = 5.0e3`, `PLASMA_FULL_W_M2 = 8.0e4` (flight.rs:46–48); driver `η·½ρv³` with `CONVECTIVE_HEAT_FRACTION = 0.005` (thermal.rs:120, flight.rs:1022–1028); density `ρ = p·M/(R·T)`, `p = p0·exp(−h/H)` (simple.rs:495–546), Terrin `p0=101325, H=5600, M=0.02897`, knots 288.15/216.65/250.35/200.15 K at 0/12/40/70 km (data/systems/home/terrin/body.ron).
- Computed with the game's own formula (i = clamp((flux−5e3)/7.5e4)):
  - sea level 200 m/s: flux 2.450e4 → i = 0.260
  - 8 km @ 300: 2.375e4 → 0.250; 10 km @ 350: 2.776e4 → 0.303; 12 km @ 450: 4.355e4 → **0.514**
  - 40 km @ 2200 (reentry): 2.967e4 → **0.329**; 45 km @ 2400: 1.632e4 → 0.151; 35 km @ 2300: 8.485e4 → 1.000 (saturated)
  - Onset speed at sea level: 117.7 m/s. Glow ceiling: 17.2 km at 300 m/s, 23.8 km at 450 m/s.
- All of the reviewer's numbers reproduce to the shown precision. The intensity ordering does invert (mid-ascent 0.51 > reentry-at-40 km 0.33), and every launch carries a visible sheath through the 7–24 km band: `emit_sheath` alpha at i=0.25 is (0.05+0.42·0.25)·glow ≈ 0.16 leading-side (flight.rs:2555–2577) — clearly visible. (Terrin is Kerbin-scaled: R=600 km, μ=3.5316e12, LEO speed ≈ 2287 m/s, so 300–450 m/s at 10–12 km is the normal stock ascent regime.)
- Spec status: PLAN:1332–1336 lists "reentry plasma" among M4c's exit criteria; PLAN:1344–1345 says "Reentry plasma (convective-flux-driven hull sheath) … ship in full"; the module doc (flight.rs:4) calls it a "**reentry plasma sheath**". The shipped window violates that identity in the mid band, though the PLAN sets no numeric bounds — this is a calibration/spec-fidelity defect, not a broken mechanic. The code comment (flight.rs:42–45) shows partial intent ("first glow ≈ a fast low pass", "Display normalization only") but does not claim mid-ascent should glow at 50%. HIGH is defensible: it is visible on every launch of the flagship flow and makes ascent and reentry indistinguishable in the mid band. Suggested fix direction in the original is sound.

### Finding 2: Flight view goes dead — or the camera pops to an unrelated vessel — when the focused vessel auto-rails — CONFIRMED

**Original**: OnRails vessels are never staged; the auto bubble→rails conversion drops the focused vessel from the staging set, so focus falls to the next-newest staged vessel (camera pop) or None (dead view), with a mislabeled "VESSEL REMOVED" cue and no Map hand-off.

**Verification**: Every link traced in code.
- `VesselState::OnRails(_) => {}` — flight.rs:1059: rails vessels stage nothing.
- Auto-rails: csp-sim lib.rs:4036–4048 and 4866–4899 — the first tick a *coasting* vessel's conic clears surface + atmosphere top forces a range break and `apply_to_rails_at`; that apply (lib.rs:6206+) rejects anything inside the atmosphere or with a dipping periapsis. This is exactly the end-of-insertion-burn "reach orbit" state.
- Launch flow: `launch_craft` clears the focus pin (lib.rs:2717–2725 doc: "a fresh launch reclaims the focus under the newest-spawn rule"), so the ascending craft holds focus only via `focus_vessel = vessels.last()` (flight.rs:1965–1967).
- Native sessions seed a parked starter craft on the pad at startup (csp-app lib.rs:409–415 → `seed_starter_craft`; nothing despawns it on a new launch), and latched craft are staged (flight.rs:1053–1058). So in the default session the post-conversion staged set is non-empty: focus resolves to the parked pad craft and `solve_chase_camera` (flight.rs:1782–1788) retargets — the chase camera jumps from the ascending craft to the pad, keeping the player's zoom framing (see Finding 4).
- With no other staged vessel: the no-focus branch (flight.rs:1772–1781) sets `cam.0.valid = false`, and `extract_flight` then draws only the near-black gradient sky fallback and returns (flight.rs:2685–2707); the HUD prints "FLIGHT — no vessel in the bubble" (csp-ui lib.rs:762–777).
- No hand-off: every `set_mode` in csp-app is user-input driven (mode key/buttons, launch, tests); nothing watches focus loss or OnRails to switch views. `CycleFocus` is a Map-mode action; outside Map it is a refusal (csp-app lib.rs:2693–2697, 4108–4110).
- Cue mislabel: `c.vessels < p.vessels && c.debris >= p.debris` → "VESSEL REMOVED" (flight.rs:1938–1940) fires for the rail conversion in the multi-vessel case, for a vessel that is alive on rails. `live_vessels` is staged specifically to make that distinction (flight.rs:1062–1071, P12-F2) and the tutorial consumes it (csp-view tutorial.rs:98–103, `has_vessel: live.0 > 0`), but `sfx_edges` does not. (In the single-vessel case the cue is "VESSEL LEFT BUBBLE" instead — also misleading, but the reviewer's cited case is the default-session one.)
- Two narrative corrections, neither defeating the finding: (a) the "ON RAILS" notice is a Program LOG line, not a HUD toast — `NotificationKind::OnRails` rides the reliability log (csp-sim lib.rs:6325–6337) and renders as "vessel N — ON RAILS" in the log panel (csp-view lib.rs:4340); (b) in the dead-view case the HUD does print the "[M] map" key hint, so the player is not entirely unguided — but in the default multi-vessel case there is no hint at all and the HUD silently shows the pad craft's telemetry. The end state of the M2 flagship flow is still a camera on an unrelated vessel or a near-black frame. HIGH confirmed.

### Finding 3: Right-click falls through non-interactive HUD panels — DISMISSED

**Original**: `egui_wants_pointer_input` is false over the label bodies of the movable readout/navball, so the flight pick resolves a scene pick behind them; `ui_masks` is consumed only in VAB mode.

**Verification**: The factual half is true — `ui_masks` is consumed only at lib.rs:817–819 (`if mode == ModeSelect::Vab`), and the readout block and navball are movable `Area`s with label/painter-only bodies (csp-ui lib.rs:753–756, 1018–1028). But the causal claim is false under the shipped egui 0.37:
- `Context::egui_wants_pointer_input` (egui-0.37.0/src/context.rs:3472) = `egui_is_using_pointer() || (is_pointer_over_egui() && !any_down())`.
- `is_pointer_over_egui` (context.rs:3442–3462) returns true whenever `layer_id_at(pos)` yields any non-Background layer.
- `layer_id_at` (memory/mod.rs:1285–1305) hit-tests only layers with `state.interactable == true` — and `Area::new` defaults `interactable: true`, with `.movable(true)` additionally forcing `interactable |= movable` (containers/area.rs:50, 142, 233). The flight readout, navball, program-status strip, and Actions window are all movable/Window surfaces, hence interactable layers.
- `secondary_clicked()` is a click-completion event ("released within a certain amount of time and distance"); on that frame `any_down()` is false ("Buttons released this frame are NOT considered down", input_state/mod.rs:1528–1534), so over any of these panels `egui_wants_pointer_input()` is true and the pick at lib.rs:706–709 is suppressed. No fall-through.
- This also explains why C14-F2's `ui_masks` exists at all: the VAB consumes `button_pressed`/`button_released` (vab.rs:2323–2328) — press-frame events where `any_down()` is true, making `egui_wants_pointer_input` false — so masks are needed there but not for a click-semantic pick. Residual micro-gap only: `notice_overlay` is `.interactable(false)` (csp-ui lib.rs:709), so a right-click exactly over a cue line could pick through it — a transient sliver, not the reviewer's claim.

### Finding 4: Chase camera never reframes on focus change — and the doc claims it does — CONFIRMED

**Verification**: `distance_m` is written in exactly two places: `FlightInput::default()` (flight.rs:85, `FLIGHT_DEFAULT_DIST = 60.0`) and zoom (flight.rs:1492, clamp 3.0..1e6). Nothing resets yaw/pitch/distance on a focus-id change, and `View::set_mode` (lib.rs:2540–2570) does not touch `FlightInput` either. The doc comment at flight.rs:40–41 ("reframed on focus changes") describes behavior that does not exist. Concrete scenario verified: zoom to 3 m on a canister, craft breaks up (higher-id fragments take focus via `focus_vessel` = `last()`) or a second launch takes focus — the camera keeps the 3 m framing of what it no longer looks at. Fix suggestion (reset distance on focus-id change, keep orientation; or fix the comment) is sound. MEDIUM confirmed.

### Finding 5: Per-frame deep-clone chain of the whole fleet — CONFIRMED

**Verification**: Chain verified at each link: `entry.1.clone()` deep-clones the cached `Vec<FlightPart>` into every staged `FlightBody` per frame (flight.rs:1013; latched equivalent :1139–1140); `FlightPart::shape: PartShape` is by-value and carries `profile: Vec<(f64,f64)>` heap data (flight.rs:177, part_mesh.rs:77) while only `model` is `Arc` (flight.rs:183); `sync_flight` then deep-copies the whole staged vec into `FlightSnapshot` via `clone_from` (flight.rs:1506–1509); `ui.propellant = focus.tanks.clone()` adds the tank copy (flight.rs:1792). `end_frame` rebuilds `FlightSyncInput` every frame (lib.rs:2236–2241). The "read by nothing after stage_program" claim is correct: the only read of `FlightSyncInput` is lib.rs:3597 inside `stage_program`, which runs at lib.rs:2256 before `schedules.sync.run` (:2259) — so the minimal `mem::take` fix is viable and the `Arc<Vec<FlightPart>>` suggestion matches the fingerprint-immutability argument (flight.rs:1000–1012). Magnitude estimate (tens of allocations for the starter craft, ~hundreds for a 100-part station) is the right order. MEDIUM confirmed as presentation-frame allocation churn.

### Finding 6: HUD "STG" readout can show another vessel's plan while the Δv walk correctly ignores it — CONFIRMED (NIT)

**Verification**: `hud_plan(Some(focus))` falls back to the newest non-empty plan when the focused vessel holds none (flight.rs:804–815), and the HUD stages_left/total use it (flight.rs:1793–1795), while `stage_propulsion`'s drop sets filter `p.vessel == Some(focus.id)` (flight.rs:1862–1866). `active_index` uses the same fallback (flight.rs:786–799) and `fire_stage` routes through `select_for_fire(focus)` (lib.rs:3196–3200), so SPACE fires the other vessel's plan — jettisoning parts of a vessel the camera is not on. The fallback is documented ("else the newest plan that still holds stages", lib.rs:3190–3195), consistent between HUD and key, and only arises on focus ≠ newest launch. NIT confirmed as described.

### Finding 7: Breakup hands focus to a debris fragment by construction — CONFIRMED (NIT)

**Verification**: `apply_breakup` inserts each fragment under `VesselId(*next_vessel_id)` with the monotonic counter and stamps `debris_since: Some(tick)` (csp-sim lib.rs:11390+); fragments therefore carry higher ids than the retained vessel, sort last in the ascending-id staged list, and `focus_vessel`/`resolve` do not filter debris (flight.rs:392–394, 1965–1967) — the camera/HUD/`SetThrottle` route land on an uncommandable fragment. Documented in the doc comment (flight.rs:1958–1964: "a new breakup fragment takes the camera"). Divergence from KSP's follow-the-pod expectation is a playability nit, as graded.

---

## PRD coverage rows (per coordinator item 5)

- **Reentry plasma "WIRED, DEGRADED calibration"** — supported: the driver and sheath are honest (same `flow_sample`/`v_rel` as the thermal tick, flight.rs:1018–1028), the calibration defect is confirmed Finding 1. Row accurate.
- **M2 flight camera/focus "PARTIAL"** — supported: the local-level chase cam with terrain floor is wired (flight.rs:1524–1568), the rails↔bubble focus hand-off gap is confirmed Finding 2, and the missing focus reframe is confirmed Finding 4. (PLAN's M2 row, PLAN:1496, is "launch→orbit→reentry→land"; the camera/focus framing is the reviewer's own decomposition — the PARTIAL verdict is consistent with the confirmed findings.) Row accurate.
- **Basic SFX "WIRED per the sanctioned note"** — accurate: PLAN:1338–1345 explicitly ships "Basic SFX" as the cue channel and defers the audio device to M10; `FlightSfx` + `notice_overlay` implement it, and csp-app/src/audio.rs now additionally maps cue texts to `OneShot` sounds — at or beyond the sanctioned note. Not a finding, as the report says.

## Spot-checks of the report's "Verified-correct" section

- **Fingerprint refresh after jettison** — CONFIRMED: `apply_jettison_part` re-parents children with composed transforms and removes the node, then re-welds (csp-sim lib.rs:7107–7129); `craft_fingerprint` hashes node ids, parents, transforms, effective indices, part count and per-part mass/CoM (flight.rs:877–906), so any jettison changes the key and `bubble_parts` re-derives (flight.rs:1010–1012); breakup's vessel removal is dropped by the retain at flight.rs:989–994. No stale-AABB path.
- **Pick correctness** — CONFIRMED: the flight pick resolves `scene.focus.resolve(&scene.snap.vessels)` — the same pin-aware resolver and same snapshot the camera solved this frame (lib.rs:710–714, flight.rs:1763–1788) — and `stage_program` resolves the menu's mirror vessel from the same `FlightSyncInput` in the same `end_frame` (lib.rs:3597–3602), so `PartRow` node ids and pick ids come from the same vessel's `craft.graph.nodes`. (The pick's UI-mask gap asserted in Finding 3 is the dismissed part, not this.)

## Summary

- Findings 1, 2, 4, 5, 6, 7: CONFIRMED (2 HIGH, 2 MEDIUM, 2 NIT).
- Finding 3: DISMISSED — egui 0.37's `egui_wants_pointer_input` is layer-based over interactable layers; the movable HUD panels are interactable, so the right-click pick is suppressed over them and no fall-through occurs.
