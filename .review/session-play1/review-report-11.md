# Review Report — P11: Flight HUD, navball & SFX cues

Scope: `crates/csp-ui/src/lib.rs` (FlightHudInput/flight_hud/help_panel/crash_dialog/PropellantGauge),
`crates/csp-ui/src/navball.rs`, `crates/csp-view/src/flight.rs` (FlightUiData, flight_present,
stage_attitude, sfx_edges, vessel_tanks, StagePlan HUD mirrors), `crates/csp-view/src/lib.rs`
(build_ui roster, PanelFlags, fire_stage/recover_control notices), cross-checked against
`crates/csp-app/src/audio.rs::cue_shot` (the literal cue-vocabulary contract) and
`csp-sim::{orbit::cartesian_to_elements, scripts::resolve_apsis_ticks}`.

## Verified-clean areas (checked per focus list)

- **Edge re-baseline on focus switch:** `sfx_edges`'s `c.focus == p.focus` guard (flight.rs:1235)
  correctly suppresses all six per-focus cue types (ignition, cut, jettison, contact, landed,
  liftoff) when the newest staged vessel changes; the three count edges are global by design.
  `(Some→None)` "VESSEL LEFT BUBBLE" and `(None→_)` first-frame baseline are both honest.
- **Latched readouts:** `stage_latched` (flight.rs:760-776) stamps `vel` = co-rotation velocity at
  the CoM (`pos + rot·com_local`); `stage_attitude` computes `v_rel = vel − ω×r(com)` → ~0 while
  parked, `v_surf > 0.1` gate → `nav_prograde = None`. SPD honestly reads ~0, prograde marker
  hides. `vspeed = (ω×r)·û ≈ 0` too.
- **NaN handling:** heading/pitch/roll default NaN, set only on non-degenerate projections
  (flight.rs:1328-1347); HUD prints "—" via `is_finite()` (lib.rs:590-601); `heading_tape`
  early-returns on non-finite (navball.rs:332).
- **Parabolic-sliver gating:** Ap/T/period gated on `a > 0 && e < 1−E_PARABOLIC_TOL`, Pe on
  `|e−1| > tol` (flight.rs:1190-1205) — matches the sim's `a := −p` encoding and the mirrored
  constant is mechanically equal (`map.rs:97` = `orbit.rs:131` = 1e-9). `resolve_apsis_ticks`
  is called with `epoch = clock = now`, which is correct for elements stamped at the current
  state (dt_epoch = 0 → M = current mean anomaly); `tk ≥ now+1` so `tk − now` can't underflow.
- **Navball far-hemisphere rim clamp:** letters clamp to 0.97, prograde/retrograde to 0.95,
  dead-ahead/astern skipped (navball.rs:179-197, 294-301). Ground-cap XOR decomposition and
  seam-split pitch-ladder runs are regression-tested.
- **Propellant bars are real:** `vessel_tanks` sums `vessel.tank_contents` (the sim's debit
  ledger) over live graph nodes against `WeldPart::tanks` capacity (flight.rs:787-814);
  jettisoned nodes leave with their tanks.
- **STG readout:** `stages_total == 0 → "single stage"`, else "N firing(s) left" — honest
  expended-vs-single-stage split (lib.rs:617-626).

## Findings

### F1 — MEDIUM: SFX edge baseline and audio voices track `focus_vessel` (newest), not the pin-aware HUD focus

- **File:** `crates/csp-view/src/flight.rs:1217-1226` (`sfx_edges` calls `focus_vessel(vessels)`
  = `vessels.last()`); the actual focus at `flight.rs:1128-1131` prefers
  `FlightFocusOverride.target`. Same divergence at `crates/csp-app/src/audio.rs:509-514`
  (continuous engine/plasma voices read `snap.vessels.last()`).
- **Problem:** While a `recover_control` pin is active on a vessel that is *not* the newest
  staged one (its `VesselId` predates any later spawn — e.g. a debris fragment or a later
  launch), the per-focus edge detector keeps baselining on the newest vessel. The pinned
  vessel's ignition/cut/jettison/contact/latch/liftoff transitions never fire cues, while the
  background newest vessel's real transitions flash cues on a HUD that is showing the pinned
  craft. The audio engine voice likewise follows the newest vessel's throttle, not the
  controlled one.
- **Evidence:** `fn sfx_edges(sfx, vessels, debris_count)` computes `cur` solely from
  `focus_vessel(vessels)`; `flight_present` resolves `focus` from the pin first
  (L1128-1131) *after* calling `sfx_edges` (L1113). The doc on `sfx_edges` claims "the focus
  rule is `focus_vessel`'s — the newest staged vessel", but that is no longer the HUD's rule
  since the pin landed.
- **Impact:** Player recovering control of an older vessel while a newer vessel is staged
  hears/sees cues for the wrong craft and misses their own ignition/touchdown cues — the
  exact "honest feedback" contract the cue channel exists for.
- **Suggestion:** Resolve the pin-aware focus first in `flight_present`, then pass the resolved
  `focus` (id + throttle/nodes/contact/latched fields) into `sfx_edges` instead of letting it
  re-derive `focus_vessel`. Apply the same focus to `AudioBackend::update`'s continuous-voice
  selection (`snap.vessels.last()` → the `FlightUiData.focus` id).

### F2 — MEDIUM: Cue vocabulary drift — ~15 `flight_notice` strings land unmapped in `cue_shot`

- **File:** `crates/csp-app/src/audio.rs:105-130` vs push sites
  `crates/csp-view/src/lib.rs:1511, 1523, 1798-1800, 1814, 1821, 1826, 1838, 1857` and
  `crates/csp-app/src/lib.rs:356, 450, 1041, 1055, 1619, 1819`.
- **Problem:** `cue_shot` maps the 10 `sfx_edges` strings plus 5 refusal notices, but most of
  the newly-landed `flight_notice` texts are absent: `"ALL STAGES FIRED — NOTHING LEFT"`,
  `"SINGLE-STAGE CRAFT — NOTHING TO STAGE"`, `"STAGING NOT READY — LAUNCHED CRAFT NOT SPAWNED"`,
  `"NOT STAGED — PARKED ON THE SURFACE (THROTTLE TO LIFTOFF)"`, `"NOT STAGED — VESSEL IS ON
  RAILS"`, `"NOTHING TO STAGE — LAUNCHED CRAFT GONE"`, `"STAGED — {n} PART(S) JETTISONED"`
  (dynamic), `"VESSEL NOT RECOVERABLE — NOT ON RAILS HERE"`, `"NO VESSEL TO RECOVER"`,
  `"SETTINGS — {n} KEY BINDING(S) MIGRATED TO POSITIONAL KEYS"` (dynamic), plus the read-only
  refusals `"…— WARP DEMOTION NOT APPLIED"`, `"…— LAUNCH REFUSED"`, `"…— MANEUVER NOT APPLIED"`,
  `"…— STAGING NOT APPLIED"`, `"…— RECOVER-CONTROL NOT APPLIED"`.
- **Evidence:** `cue_shot`'s own comment calls these "the fixed `flight_notice` refusals the
  app's input layer pushes" and maps five of them to `UiConfirm` "so a dead keypress still
  says it was heard" — the new refusal strings (the same bug-class fix) get no blip and each
  trips the warn-once drift detector at `audio.rs:498-501` ("cue vocabulary drifted").
- **Impact:** Inconsistent acknowledge feedback — some dead keys blip, most are silent — and
  the drift log fires on intended cues, desensitizing the detector meant to catch real drift.
  The dynamic `format!` notices can't be literal-matched at all.
- **Suggestion:** Map the refusal family to `OneShot::UiConfirm` — either exhaustively, or by
  prefix class (`text.starts_with("SESSION IS READ-ONLY")`, `starts_with("NOT STAGED")`, etc.).
  For the two dynamic confirmations (`STAGED —`, `SETTINGS —`), either map by prefix or
  replace with a fixed cue + separate detail line. Keep `sfx_edges`'s ten strings literal.

### F3 — LOW: "STAGE — PART JETTISONED" fires on *any* node-count decrease, not just staging

- **File:** `crates/csp-view/src/flight.rs:1241-1243` (`c.nodes < p.nodes → push("STAGE — PART
  JETTISONED")`); `nodes` = `craft.graph.nodes.len()` (flight.rs:691).
- **Problem:** Any shrink of the focused vessel's part graph — impact destruction, ablation,
  any future removal path — flashes "STAGE — PART JETTISONED". During a crash the HUD can show
  "CONTACT" + "BREAKUP" + "STAGE — PART JETTISONED" simultaneously, claiming a staging event
  the player never commanded. The `audio.rs` mapping (`StageJettison`) plays the separation
  sound for the same false edge.
- **Impact:** Mislabeled cue text in exactly the chaotic moments where the cue channel should
  be most trustworthy.
- **Suggestion:** Retitle the edge to the mechanism it actually detects (e.g. "PART(S) LOST"),
  or correlate with a just-issued `JettisonPart` command/plan pop before naming it staging.
  Given the plan pop lands its own "STAGED — n PART(S) JETTISONED" notice (lib.rs:1857), a
  generic graph-shrink text is the cheap honest fix.

### F4 — LOW: T-Ap/T-Pe suppressed on open conics though the resolver supports inbound-open periapsis

- **File:** `crates/csp-view/src/flight.rs:1193-1205` — `t_apo`/`t_pe` are computed only inside
  `a > 0.0 && e < 1.0 − E_PARABOLIC_TOL`; Pe *altitude* is published on open conics
  (L1190-1192) but its time-to-apsis is not.
- **Problem:** `resolve_apsis_ticks` already handles open conics correctly — `Ok(None)` for
  apoapsis, `Some(tick)` for periapsis while still inbound (scripts.rs:657-666). A hyperbolic
  flyby therefore shows "PE  123.4 km" but "T-A/P — /—", hiding the single most useful apsis
  number on an escape/flyby.
- **Impact:** Honest-but-incomplete readout; inconsistent with the Pe row the same pass
  publishes. Player gets no time-to-periapsis precisely on trajectories where it matters.
- **Suggestion:** Compute `t_pe_s` outside the closed-conic gate (`resolve_apsis_ticks` returns
  `None` outbound → "—" anyway); keep `t_apo_s`/`period_s` closed-only.

### F5 — LOW: "ON PAD" latch text is wrong off-pad, and `hint()` vs `hint_labeled()` glyphs can disagree

- **File:** `crates/csp-ui/src/lib.rs:551-560` (`"ON PAD — [{}] to launch"`), also 530-537.
- **Problem:** Two issues. (a) `input.latched` is true for *any* §2.6.4 surface-latched vessel —
  a craft that landed and settled in the field also reads "ON PAD". (b) The latch line and the
  no-vessel hint line call `keys.hint(...)`, which uses the QWERTY `physical_fallback` glyph;
  the bottom hint bar and help panel use `hint_labeled`/`display_label` with observed glyphs.
  A player on AZERTY sees "[W] to launch" (fallback) beside a hint bar that correctly prints
  the observed glyph — until the key is pressed once and `key_labels` fills in.
- **Suggestion:** (a) Rename to "PARKED" or "SURFACE-LATCHED" (matches the
  `"LANDED — SURFACE RAILS"` cue and `"NOT STAGED — PARKED ON THE SURFACE"` refusal).
  (b) Pass `input.key_labels` to both `hint` sites → `hint_labeled`.

### F6 — LOW: "DEBRIS IMPACT" fires on any debris-count decrease; same-frame double events can eat "VESSEL REMOVED"

- **File:** `crates/csp-view/src/flight.rs:1255-1265`.
- **Problem:** `c.debris < p.debris` is assumed to mean impact removal ("it never latches",
  L1259-1260), but any other exit path that drops a debris-flagged vessel out of the staged
  set (a `ToRails` conversion, a scripted removal) fires "DEBRIS IMPACT" for a non-impact.
  Additionally, the `VESSEL REMOVED` edge is suppressed whenever debris decreased in the same
  frame (`c.debris >= p.debris` guard), so a simultaneous debris impact + clean-vessel removal
  loses the second cue.
- **Impact:** Small text-accuracy misses in multi-vessel scenes.
- **Suggestion:** If the sim can distinguish removal causes, gate on that; otherwise rename to
  "DEBRIS LOST". For the suppression, fire both cues when both deltas are observed.

## Nits (not flagged as findings)

- `flight_present` no-focus early return (flight.rs:1132-1139) clears `focus`/`propellant`/
  `stages_*` but leaves `latched`, `alt_m`, `vspeed_mps`, `throttle`, speeds, attitude mirrors
  stale; harmless while `has_vessel` gates every reader, but the mirror lies if a future
  consumer reads it.
- `FlightHudInput.vessel_count`/`debris_count` (lib.rs:415-417, populated at view lib.rs:405-406)
  are carried into the HUD input but never drawn.
- `flight_present` (and thus `sfx_edges` TTL decay) runs every frame in every mode — cues
  pushed while in map/VAB expire unseen visually (audio still plays). Reads as intended, worth
  knowing.
- `ui.orbital_speed_mps` on a latched pad vessel shows the co-rotation |ω×r| (~hundreds of m/s
  on a spinning body) — documented as deliberate; honest but reads oddly next to "SPD 0.0".

## Cross-cluster note

The pin-vs-`focus_vessel` divergence (F1) is the P11 half of P04's focus-pin lifecycle — the
pin's three release paths themselves looked correct (flight.rs:1121-1131, view
lib.rs:1161-1164, launch_craft clear). One residual worth noting for P04: a pin whose
`ToBubble` is rejected at drain despite `to_bubble_eligible` (e.g. re-weld failure) leaves a
pending pin that never stages — it survives indefinitely on `seen == false` and would steal
focus if that vessel is ever bubbled later.
