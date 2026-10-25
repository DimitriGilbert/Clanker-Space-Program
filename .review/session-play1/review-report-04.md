# Review Report — P04: RecoverControl / ToBubble & focus lifecycle

Scope: `csp-view/src/lib.rs` (recover_control L1480–1546, to_bubble_eligible L1548–1563,
clear_flight_focus_override L1565–1573, pin release L1157–1164, flight_focus L1575–1586),
`csp-view/src/flight.rs` (FlightFocusOverride L236–268, pin lifecycle L1115–1131),
`csp-sim/src/lib.rs` (ToBubble L634, apply_to_bubble L5469–5533, TakeControl L4699–4741 +
L4743–4794, lease semantics L368–465, check_control_lease L4706–4739),
`csp-app/src/lib.rs` (RecoverControl dispatch L1816–1823),
`csp-view/tests/flight_focus.rs`.

## Verified correct (no action needed)

- **Pin release paths cover the three documented cases.** Staged-then-left is handled by
  `flight_present` (flight.rs:1121–1127 — `seen` flips on first staged frame, a leave after
  that clears); gone-from-arena is handled by `end_frame` (lib.rs:1161–1164 — covers both
  pending and seen pins since removal drops the arena key either way); fresh launch clears
  via `clear_flight_focus_override` at lib.rs:1758, after the spawn enqueue and before the
  mode switch, so a recovery that drains in the same step as the new spawn still yields
  focus to the newest id. Every production spawn path funnels through `launch_craft`
  (`demo::seed_pad_craft`/`seed_starter_craft`/`new_demo` included), so the third path is
  airtight. `VesselId` is monotonic and never reused (csp-sim L266–272), so a stale pin can
  never alias a different vessel.
- **A pending pin does not break focus.** `flight_present` resolves
  `pin→staged-body .or_else(focus_vessel)` (flight.rs:1128–1131), so while the `ToBubble`
  is still draining, camera/HUD/`flight_focus()` follow the newest staged vessel.
- **Eligibility mirrors four of the drain's reject classes exactly** — unknown vessel,
  not-on-rails (`v.orbit()`), `orbit.parent != dominant` (`dominant_body()` is
  `physics.dominant`, the same field `apply_to_bubble` compares at sim:5494), `craft` —
  plus `environment().is_none()` ≡ `physics.is_none()` (sim:2442–2450 vs 5482–5487).
- **Map-focused vs fallback ordering is deliberate and right:** in Map mode an explicit
  ineligible `focus_vessel` is a surfaced refusal, not a redirect (lib.rs:1506–1513); the
  newest-eligible-on-rails fallback only applies with no map target (lib.rs:1514–1520).
- **`max()` for the fallback is correct:** `vessels()` is `&BTreeMap<VesselId, Vessel>`
  (sim:3178), ids are monotonic, so `max()` = most recently spawned eligible vessel.
  (Minor: "newest" means newest *spawn*, not most recently railed — consistent with the
  codebase's documented convention.)
- **The ToBubble→TakeControl pair is drain-atomic:** both ride `(Ui, PLAYER_SOURCE_ID=1)`
  and the drain order key is (tick, class, id, seq), so they apply adjacently in issue
  order — no other command can interleave between them.
- `apply_to_bubble`'s dead-event scrub (sim:5522–5530 via `event_names_vessel`, L8357+)
  correctly drops SoiTransition/SoiRescan/DebrisDecay/ManeuverNodeStart/dormant
  PartFailure, and `vessel_mut.maneuver = None` (L5518) discards the scheduled node as
  documented — while `fire_maneuver_start`'s own materialization keeps it.

## Findings

### F1 — HIGH: RecoverControl refusals are invisible in Map mode — the verb's primary mode

- **File:line:** `crates/csp-view/src/lib.rs:1511` and `:1523` (`flight_notice` → `FlightSfx`
  cue at :1868–1876); rendered only by `csp_ui::flight_hud`, gated `mode == Flight` at
  `crates/csp-view/src/lib.rs:383–420` (`csp-ui/src/lib.rs:497–516`); same hole in
  `crates/csp-app/src/lib.rs:1818–1819` (read-only refusal).
- **Problem:** `flight_notice` pushes onto the `FlightSfx` cue stack, which is mirrored into
  `FlightUiData.sfx` and drawn *only* inside `flight_hud` — and `flight_hud` only runs when
  `mode == Flight`. `map_overlay` has no notice channel (its signature takes no notices/sfx;
  csp-ui/src/lib.rs:244–252). Meanwhile `sfx_edges` decrements every cue's TTL each
  presentation frame regardless of mode (`flight.rs:1273–1278`), so the cue ages out unseen.
- **Evidence:** Pressing T in Map mode with an ineligible focused vessel
  (`"VESSEL NOT RECOVERABLE — NOT ON RAILS HERE"`), no eligible target
  (`"NO VESSEL TO RECOVER"`), or a halted session (`"SESSION IS READ-ONLY — RECOVER-CONTROL
  NOT APPLIED"`) produces zero on-screen feedback; switching to Flight even a second later
  shows nothing (TTL = 90 frames).
- **Impact:** Map mode is where RecoverControl is meant to be used — `focus_vessel` only
  feeds target selection when `mode() == Map` (lib.rs:1501–1505). The primary-mode refusal
  paths are dead keys, exactly the "every verb a dead key can reach must land a visible
  notice" violation class.
- **Suggestion:** Give the recover notices a map-visible surface — e.g. a cue/notice row in
  `map_overlay` fed from the same `FlightSfx` stack (or a session notice channel drawn in
  every mode, like the M10 panels), so refusal text is readable wherever the key fires.

### F2 — MEDIUM: `to_bubble_eligible` doesn't cover all drain reject classes — silent reject leaves a pending pin that can hijack focus/throttle much later

- **File:line:** `crates/csp-view/src/lib.rs:1552–1563` vs
  `crates/csp-sim/src/lib.rs:5488–5520`; stale-pin window at `flight.rs:1121–1131` and
  `lib.rs:1161–1164`.
- **Problem:** The pre-check mirrors only the *state-predicate* rejects (vessel, rails,
  parent, craft, env). Three reject classes can still fire after `eligible` returned true:
  1. `self.orbits.state_at(orbit_state, self.clock)?` (sim:5502) — a conic that fails to
     evaluate at the drain tick;
  2. `csp_phys::weld(&craft.graph, &craft.parts)` failure (sim:5504–5505) — the stored
     craft is never re-validated;
  3. TOCTOU — an on-rails vessel's `SoiTransition` event can re-parent `orbit.parent`
     between the keypress-time eligibility check and the next boundary drain
     (sim:5494–5498; SOI events fire inside the same `step_frame`).
- **Evidence:** On a `Recoverable` reject the command is rolled back and lands only on
  `AdvanceReport::rejected`, which csp-app renders as a `log::warn!`
  (`crates/csp-app/src/lib.rs:836–842`) — no view notice in either mode. The pin then sits
  pending indefinitely: `flight_present`'s release requires `seen`-then-left
  (flight.rs:1121–1127) and `end_frame`'s requires the vessel gone from the arena
  (lib.rs:1161–1164); a still-OnRails vessel satisfies neither, and the only remaining
  release is a fresh launch.
- **Impact:** If the vessel *ever* stages later through another path — a scheduled
  `ManeuverNodeStart` auto-materializes on-rails vessels while keeping the node
  (`fire_maneuver_start`, sim:5714–5753), or another producer's `ToBubble` — the stale pin
  fires `seen` and retargets the chase camera *and* the `SetThrottle`/`SetAttitude` route
  (`flight_focus()` → `FlightUiData.focus`) to it mid-flight, potentially hours of game
  time after the press and while the player is flying something else. The player also got
  no signal the recovery ever failed.
- **Suggestion:** Two cheap fixes, either suffices — (a) after the next boundary, treat
  "pin still pending and its `ToBubble` drained" as a release + notice: since the command
  drains at the first tick boundary after enqueue, `end_frame` can clear a pending pin once
  the sim clock has passed the enqueue frame without the vessel staging (arm a one-frame
  deadline in `recover_control`), or (b) plumb `report.rejected` entries keyed to
  `(Ui, PLAYER_SOURCE_ID)` `ToBubble`s back into a notice + `clear_flight_focus_override`.
  Optionally extend `to_bubble_eligible` to also reject vessels whose conic won't solve —
  but the drain-time window for SOI re-parenting can't be closed by any pre-check, so a
  post-drain release is needed regardless.

### F3 — MEDIUM: The unconditional `TakeControl{Player}` steals a Script/Agent lease even when `ToBubble` rejects

- **File:line:** `crates/csp-view/src/lib.rs:1531–1541` (the "harmless when it doesn't"
  comment is wrong); `crates/csp-sim/src/lib.rs:4777` (`me == Lease::Player` preempts
  unconditionally), `:4785–4792` (transfer applies on any live vessel regardless of
  residence).
- **Problem:** `recover_control` enqueues `TakeControl{Player}` unconditionally alongside
  `ToBubble`. The pair drains adjacently ((Ui,1) channel, issue order), so whenever the
  `ToBubble` rejects — any F2 divergence, or the target already materialized by another
  path so ours hits "not on rails" — the `TakeControl` *still applies*: `Lease::Player` as
  target needs no holder (sim:4766), and a Player issuer preempts any current lease. A
  `Script(_)`/`Agent(_)` holder loses the vessel; every subsequent control command from it
  rejects `"lease: control-affecting command from a non-leaseholder"` (sim:4735–4737).
- **Evidence:** The doc's own justification — "a bound Script/Agent holder keeps issuing
  control commands after ToBubble converts the residence, so the recovered vessel would
  answer two masters" — is correct only on the success path. On the failure path the
  comment claims harmlessness that doesn't exist: the player recovered nothing and a
  third-party controller was silently evicted. (The same lease-steal also fires when the
  player launches a fresh craft after pressing T — `launch_craft` clears the pin but the
  already-queued `TakeControl` still lands on the abandoned vessel.)
- **Impact:** Latent in v1 (no shipped script/agent content may hold leases yet), but it
  directly contradicts the stated invariant and silently derails any external controller
  the moment a recover fails.
- **Suggestion:** Only transfer the lease once the conversion demonstrably lands — the
  pin's `seen` transition (flight.rs:1122–1123) is exactly that signal: enqueue
  `TakeControl` the first frame the pinned vessel appears in the staged set, rather than
  optimistically at press time. Alternatively issue the lease transfer inside
  `apply_to_bubble` for a Ui-class recover, but that conflates command semantics for
  script-issued `ToBubble`s — the `seen`-edge approach is the cleaner seam.

### F4 — LOW: `sfx_edges` edge-detection ignores the pin — cues fire on the newest staged vessel, not the focused one

- **File:line:** `crates/csp-view/src/flight.rs:1218` (`let cur = focus_vessel(vessels)`)
  vs the pin-aware focus resolution at `:1128–1131`.
- **Problem:** The §3.7 edge detector baselines and diffs `focus_vessel(vessels)` — the
  newest staged vessel — while the camera, HUD mirrors, and `flight_focus()` command route
  follow the pin when one is set and not the newest.
- **Evidence:** Recover vessel A while a newer vessel B is in the bubble: `ui.focus`/camera
  pin to A, but "ENGINE IGNITION/CUT", "STAGE — PART JETTISONED", "CONTACT", "LANDED",
  "BREAKUP" cues key off B's staged state. E.g., igniting the recovered vessel produces no
  cue while B's throttle edges flash them.
- **Impact:** Cosmetic but real — the SFX channel reports edges for a vessel the player is
  not looking at and cannot throttle, and misses edges on the one they are.
- **Suggestion:** Run `sfx_edges` against the resolved focus (move the pin/seen resolution
  ahead of the edge pass, or pass the focused `FlightBody` in) so the cue channel tracks
  the same vessel as the camera and command route.

## Minor notes (not findings)

- `flight_focus`'s doc (lib.rs:1575–1586) still describes only the newest-spawn rule — it
  does not mention the pin override it now returns. Stale doc, no behavior issue.
- The "NOT ON RAILS HERE" refusal text also fires when the map-focused vessel *is* on
  rails but around a non-dominant parent — message is technically accurate ("here" = the
  dominant body) but could read ambiguously.
- `end_frame`'s pin check (lib.rs:1161) runs after `capture_sync_input` and before the
  schedules — correctly ordered; a pinned vessel removed mid-step is released the same
  frame.
- No test coverage exists for the pin's release paths or the reject/refusal matrix —
  `tests/flight_focus.rs` only pins the newest-spawn rule.
