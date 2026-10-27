# Review Report C19 — Flight presentation + part picking (`csp-view/src/flight.rs` + 3 test files)

Session: session-full-20261026 · Reviewed against the current working tree (HEAD past 6cc2cb4, uncommitted W2.6/UX pass in scope).

Scope read in full: `crates/csp-view/src/flight.rs` (code L1–2942, tests L2943–3293), `tests/flight_focus.rs`, `tests/flight_plume.rs`, `tests/part_silhouettes.rs`, plus the wiring in `crates/csp-view/src/lib.rs` (pick call site L700–726, `stage_program` L3575–3676, `end_frame` L2221–2286, `fire_stage` L3189+), the consumer `csp-ui/src/program.rs::part_menu` L2432–2530, and the physics the plasma driver reads (`csp-phys/src/thermal.rs` L110–241). Tests executed: all 8 pass (5 inline + flight_focus 1 + flight_plume 1 + part_silhouettes 2).

## Verified-correct (no finding, checked before flagging)

- **Pick targets the vessel the player sees.** `build_ui` (lib.rs:710–714) resolves the pick with the same `FlightFocusOverride::resolve` + `FlightSnapshot` the camera solved this frame; `stage_program` (lib.rs:3597–3601) resolves the menu's `mirror.vessel` from the same `FlightSyncInput` in the same `end_frame`. Pick node ids and the menu's `PartRow`/`ChuteRow`/experiment node ids both come from `craft.graph.nodes` keys of the same vessel.
- **Collider freshness after staging/jettison/breakup.** `apply_jettison_part` (csp-sim lib.rs:7108–7125) re-parents children and re-welds, changing the graph → `craft_fingerprint` (flight.rs:877) changes → `LatchedWeldCache.bubble_parts` re-derives (flight.rs:1010–1012). Breakup removes the vessel id; the retain at :989 drops its entry. No stale-AABB path found.
- **Stale open menu degrades safely.** A jettisoned node's open menu falls to "no actions on this part" (program.rs:2476–2499); a vessel-id change closes it (lib.rs:729–733).
- **Plume honesty.** Emitters stage only live-rated sites (`live_plumes`, `rated_n>0`), dim by `throttle·delivered`, dry-engine case test-verified (flight_plume.rs).
- **Behind-eye projection.** `project_into` rejects `w ≤ eps`; `emit_tri` is all-or-nothing (test-asserted stream alignment).

## Findings

### [SEVERITY: HIGH] Finding 1: Plasma display window fires on every ascent — ascent glows brighter than reentry (§3.7 "reentry plasma" honesty)
**File**: crates/csp-view/src/flight.rs:46–48 (window), :1022–1028 (driver), :2875–2877 (normalization)
**Problem**: `PLASMA_ONSET_W_M2 = 5e3` corresponds to ρv³ = 2e6 (η=0.005). Against Terrin's shipped atmosphere (data/systems/home/terrin/body.ron: p0=101325, H=5600, Terrin-like temperature knots), computed with the game's own formula `η·½ρv³`:
- 200 m/s at sea level ("fast low pass"): flux 2.45e4 → **i = 0.26**
- ascent 8 km @ 300 m/s: 2.37e4 → **i = 0.25**; 10 km @ 350 → 0.30; **12 km @ 450 m/s: 4.36e4 → i = 0.51**
- reentry from the 75 km home orbit at 40 km @ 2200 m/s: 2.97e4 → **i = 0.33**

**Evidence**: the intensity ordering inverts — a transonic mid-ascent glows ~50% while an actual orbital reentry at 40 km glows 33%. Real reentry saturates only below ~35 km (8.49e4 at 35 km @ 2300).
**Impact**: every launch shows a persistent orange sheath from ~7 km up, reading as "burning up" during the normal max-Q regime; the effect's §3.7/M4c identity is *reentry* plasma ("Reentry plasma … ship in full", PLAN:1344). The author comment at :42–44 ("first glow ≈ a fast low pass") shows the calibration is deliberate, but it contradicts the effect's PRD identity and makes ascent/reentry visually indistinguishable in the mid band.
**Suggestion**: raise the window to the regime the sim's own thermal tick treats as a heat pulse — e.g. onset ~3e4 (ρv³ ≥ 1.2e7, unreachable below ~hypersonic in thin air) and full ~3e5, so ascent max-Q stays dark and the 48→35 km reentry traversal keeps its full dynamic range.

### [SEVERITY: HIGH] Finding 2: Flight view goes dead — or the camera pops to an unrelated vessel — when the focused vessel auto-rails
**File**: crates/csp-view/src/flight.rs:1059 (OnRails not staged), :1965–1967 (`focus_vessel` = `vessels.last()`), :1772–1781 (no-focus branch invalidates the camera)
**Problem**: OnRails vessels are never staged, so the moment the Phase1 auto bubble→rails conversion fires (csp-sim lib.rs:4036 — a *clearing conic* at the end of an insertion burn), the focus falls to the next-newest staged vessel or `None`. No reframe, no auto hand-off to Map (verified: nothing in csp-app reacts to focus loss beyond `throttle_blocker` refusals; `CycleFocus` works in Map mode only, lib.rs:2693).
**Evidence**: with another vessel staged (the parked starter craft), the chase camera teleports from the ascending craft at 40 km to the pad; with none staged, `cam.0.valid = false` kills the terrain pass too and the frame degrades to the near-black gradient sky fallback (:2685–2703, :2704–2707) with `has_vessel = false`.
**Impact**: this is the natural end state of the flagship M2 flow (reach orbit). The player gets an "ON RAILS" toast and a dead flight view, and must know to press M. The cue channel compounds it: the staged-count drop fires "VESSEL REMOVED" (:1938–1940) for a vessel that is alive on rails — an honesty defect in the same channel (§2.13's honest-refusal spirit; P12-F2 fixed this exact conflation for the tutorial's `has_vessel`).
**Suggestion**: on the rails edge, either keep the focused vessel flight-renderable via `OrbitModel::state_at` (PLAN §2.5's own rails-presentation rule for map view extends naturally: pose per frame, no interpolation) or explicitly hand focus to Map with a cue ("VESSEL ON RAILS — MAP VIEW [M]"). Fix the "VESSEL REMOVED" edge to consult `live_vessels` (which P12-F2 already stages for exactly this distinction, :1062–1071).

### [SEVERITY: MEDIUM] Finding 3: Right-click falls through non-interactive HUD panels — the pick runs on parts hidden behind the movable readout/navball
**File**: crates/csp-view/src/lib.rs:700–726 (flight pick gated only on `egui_wants_pointer_input`), :817–819 (`ui_masks` consumed in VAB mode only)
**Problem**: the flight pick's only UI guard is `ctx.egui_wants_pointer_input()`, which is false over non-interactive labels. The W2.6 pass made the readout block and navball movable `Area`s (complaint #2, C23) whose bodies are mostly labels; the `ui_masks` click-through set that C14-F2 built for the VAB — and which `flight_action_bar`/`part_menu` rects are already pushed into (:688, :735) — is consumed by nothing in flight mode.
**Evidence**: right-clicking the flight-data panel or navball body (not a button) resolves a scene pick for whatever part projects behind the panel, opening a part menu anchored inside the HUD.
**Impact**: the new affordance (complaint #1's discoverability fix) mis-fires exactly over the other new affordance (movable panels); the player can open a menu for a part they cannot see.
**Suggestion**: in flight mode, treat `ui_masks.contains(pos)` as `over_ui` — the rects are already collected in `build_ui`; move the flight pick after the panel pass (it already runs there) and check the set. One-line predicate change plus reordering.

### [SEVERITY: MEDIUM] Finding 4: Chase camera never reframes on focus change — and the doc claims it does
**File**: crates/csp-view/src/flight.rs:40–41 ("Default camera distance (m) — reframed on focus changes"), :85, :1491–1493 (the only `distance_m` write is zoom)
**Problem**: `FLIGHT_DEFAULT_DIST` is used exactly once — `FlightInput::default()`. No code resets yaw/pitch/distance when `flight_present`'s focus resolution moves to a different vessel id.
**Evidence**: zoom in to 3 m on a canister, stage, have the craft break up or a second launch take focus — the camera keeps the 3 m framing of a vessel it is no longer looking at (or 60 m framing of a 0.5 m fragment).
**Impact**: every focus transition (breakup fragment, second launch, recover-control pin) can arrive at an absurd framing; the doc comment promises behavior that does not exist, which is worse than no comment for the next maintainer.
**Suggestion**: track the previous focus id in `FlightInput` (session state); on change, reset `distance_m` to `FLIGHT_DEFAULT_DIST` (keep yaw/pitch — the player's orbit orientation is a preference, KSP-style). Fix the comment either way.

### [SEVERITY: MEDIUM] Finding 5: Per-frame deep-clone chain of the whole fleet (cache → staged → snapshot), ~3 copies of every part's heap data per frame
**File**: crates/csp-view/src/flight.rs:1013 (`entry.1.clone()` — deep `Vec<FlightPart>` incl. every lathe `profile: Vec<(f64,f64)>`), :1139–1140 (latched `parts`/`plumes` clones), :1506–1509 (`sync_flight`'s `snap.vessels.clone_from` — second full copy), plus `ui.propellant = focus.tanks.clone()` (:1792); produced every `end_frame` at lib.rs:2236–2241
**Problem**: `capture_sync_input` deep-clones the cached per-part render data into each `FlightBody`, then `sync_flight` deep-clones the entire staged list into `FlightSnapshot`. `FlightPart::model` is already `Arc` — the shapes are not.
**Evidence**: 5-part starter craft ≈ tens of small allocations/frame today; scales as vessels × parts (a 100-part station × 3 copies = ~300+ allocations and ~100 KB memcpy per frame at 60 fps), all to reproduce identical topology that the fingerprint already proved unchanged.
**Impact**: presentation-frame allocation churn and cache pressure on exactly the path the §2.5 RenderExtract budget cares about; the C12-F3 cache work is undercut by cloning its product twice anyway.
**Suggestion**: (a) store `Arc<Vec<FlightPart>>` in both caches and clone the Arc (the vec is immutable per fingerprint); or minimally (b) `sync_flight` `mem::take`s the staged vec — `FlightSyncInput` is rebuilt at the next `end_frame` and read by nothing after `stage_program` (verified: only lib.rs:3597 reads it, before `sync` runs).

### [NIT] Finding 6: HUD "STG" readout can show another vessel's plan while the Δv walk correctly ignores it
**File**: crates/csp-view/src/flight.rs:804–815 (`hud_plan` falls back to newest non-empty plan), :1793–1795 vs. :1862–1866 (stage-propulsion drops filter is focus-bound only)
**Problem**: flying a vessel with no plan (a recovered/pinned older craft) while a newer launch's plan is live shows the newer plan's `stages_left/total` on this vessel's HUD, and SPACE would indeed fire that plan (`active_index` uses the same fallback) — jettisoning *another* vessel's parts while the camera sits on this one. Consistent between HUD and key, but surprising; documented as "single-plan behaviour is unchanged". Only arises on focus ≠ newest launch, so nit severity.

### [NIT] Finding 7: Breakup hands focus to a debris fragment by construction
**File**: crates/csp-view/src/flight.rs:1958–1967
`focus_vessel` = `vessels.last()` with no non-debris preference; `apply_breakup` replaces the vessel with higher-id fragments, so the camera/HUD/`SetThrottle` route land on an uncommandable fragment. Documented in the doc comment, so intentional — noting divergence from KSP expectation (focus follows the command pod) as a playability nit.

## PRD coverage table

| §3.7 / M4c / M2 item | Status | Evidence |
|---|---|---|
| Silhouette-correct kitbashed parts (§3.7 M4c) | **WIRED** | `vessel_parts` + `part_mesh::classify_weld_part`; box fallback for craft-less bodies is honest degradation; `part_silhouettes.rs` asserts distinct pod/tank/engine lathes end-to-end |
| Engine plume VFX (§3.7 M4c) | **WIRED** | `emit_plumes` two-shell column; intensity = throttle × delivered (factor·coverage); dry-but-throttled stages no emitter — test-verified |
| Reentry plasma (§3.7 M4c, "ships in full") | **WIRED, DEGRADED calibration** | `emit_sheath` + convective driver are honest (same `flow_sample` as the thermal tick); the display window is miscalibrated (Finding 1) |
| Ground-contact dust (§3.7, §2.9 predicate → puff) | **WIRED** | `contact`-gated ring anchored to the baked heightfield floor (`surface_floor_radius`, P07-F2/P08-F3 fix verified in code) |
| Basic SFX (§3.7 M4c) | **WIRED per the sanctioned note** | `FlightSfx` cue channel + `notice_overlay`; audio-device deferral is the PRD's own M4c as-shipped note (PLAN:1338–1345) — not a finding |
| M2 flight camera/focus | **PARTIAL** | local-level orbit chase cam with terrain floor: WIRED; rails↔bubble hand-off: GAP (Finding 2); focus-change reframe: MISSING despite doc (Finding 4) |
| Right-click part pick → part menu (complaint #1) | **WIRED** | `pick_part` → `PartMenuState` → ProgramCommands against the same mirror; core correctness verified (see "Verified-correct"); UI fall-through gap (Finding 3) |

**Top 3 issues**: Finding 1 (plasma window — visible on every launch, inverts ascent vs reentry), Finding 2 (flight view dead/pops at the auto-rails edge — end of the flagship M2 flow), Finding 3 (right-click falls through the new movable HUD panels).
