# Fix Plan — session-play1 (playability track)

~60 confirmed findings across 15 clusters. Grouped into implementer tracks by file ownership. The dominant bug class the user keeps reporting: **controls that refuse silently** — notices route to `flight_notice`/`FlightSfx` which only renders in `flight_hud` (mode==Flight), so refusals from Map/VAB die unseen.

## Track A — Notice channel + silent refusals (user's #1 complaint)
- `flight_notice`/FlightSfx cues must render in ALL modes (global notice area or map_overlay channel), not Flight-only.
- Migrate every refusal that can fire outside VAB off `vab_event(Notice)` (invisible there): launch_refused, ThrottleFull/Cut, stage, RecoverControl, maneuver, craft IO, warp demotion.
- Add `throttle_blocker` analog to `attitude_blocker`: rails/no-vessel throttle press → notice; stop throttle_axis squash to 0 on no-body.
- P09-F2: don't offer InBubble vessels in the maneuver ComboBox (no SetManeuver → silent refuse) OR surface the refusal.
- Stage gated `mode==Flight` → Space is dead in Map; widen or notice.
- P04-F3: `TakeControl{Player}` preempts Script/Agent lease even when ToBubble rejects — issue lease transfer only on pin `seen` transition.

## Track B — fuel + sim correctness (determinism-load-bearing)
- P03-F1: coverage normalized to actual throttle demand, not full-throttle needs (under-delivery + denormal residue at partial throttle).
- P03-F3: burn Δv bookkeeping uses post-debit mass.
- P03-F2: bump SAVE_VERSION for WeldPart.engine → clean UnsupportedVersion not cryptic TrailingBytes.
- P06-F1 [HIGH]: launch spawn occupancy check — second launch onto occupied pad spawns inside parked craft (reproduced: two SurfaceLatched at identical pose → ghost-overlap).
- P06-F2: pre-check funds/vessel-cap before Flight switch (empty flight, no notice).
- P15-F1: add pooled-coverage + starvation + jettison-mid-burn fuel tests.

## Track C — focus-pin consistency
- Shared pin-aware focus resolver: sfx_edges, audio voices, tutorial, throttle/attitude all use `vessels.last()` not the FlightFocusOverride pin → wrong-vessel cues/controls during recover_control (P04-F4, P07-F1, P11-F1, P12-F1).
- P04-F2: to_bubble_eligible misses state_at/weld/SOI-TOCTOU rejects; pending pin survives forever → hijacks focus on later staging.
- P05-F1: stage-plan (min_id,fingerprint) can't disambiguate same-craft mints ≥min_id → permanent wrong-vessel bind.
- P12-F2: tutorial has_vessel conflates OnRails with destroyed → false "vessel lost" after burn.

## Track D — VAB + settings
- P10: dead-click paths (pick in flight, out-of-bounds release) → emit Pick(None).
- P14-F1: `graphics.quality` is a DEAD CONTROL (zero consumers) — wire it or remove it; a lying control.
- P14-F2: keymap forward-compat — backfill missing KeyAction verbs (upgraders get 6 dead verbs); don't silently full-reset on unknown variant.
- P14-F3: NaN/inf literals bypass every clamp (set_zoom_factor, master_volume).

## Track E — map + terrain
- P09-F1: set_mode refocus ignores vessel focus → snaps to system distance; use refocus's 3·|a|.
- P08-F1: horizon cull uses bare datum not terrain-displaced → missing limb relief + popping.
- P08-F2: sim contacts datum but terrain ±2.4km → off-pad landings embed/float (scope — document or terrain-aware contact).
- P07-F2/P08-F3: dust ring anchored to datum not terrain → use surface_floor_radius.

## Track F — audio
- P13-F1: writer lead-cap is a proxy (written-vs-wall), saturates to ~185ms and can't drain — real occupancy bound or drop-oldest.
- P13-F2: cue_shot covers ~1/3 of flight_notice vocabulary (15 unmapped + format! families) → audio-silent + warn-spam.
- P13-F4: NaN throttle/plasma permanently kills a voice (NaN>LEVEL_EPS → gain 0 forever).

## Track G — test gaps (P15)
- Dead-stick recovery (recover_control/pin/eligibility) — zero tests.
- AZERTY event→token path (key_press/action_of physical-first) — zero coverage.
- StagePlan::bound_to / stage_plan_of decoupler walk + 4/6 fire_stage refusals — uncovered.
