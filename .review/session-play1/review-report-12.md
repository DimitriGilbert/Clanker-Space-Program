# Review Report — P12: Tutorial (first-flight checklist)

Scope: `crates/csp-ui/src/tutorial.rs` (TutorialStep/STEPS/STEP_LAUNCH/CLIMB_ALT_M/
SPACE_FALLBACK_ALT_M, TutorialInput/TutorialState::observe, `{key}` substitution,
tutorial_overlay), `crates/csp-view/src/tutorial.rs` (TutorialRes, tutorial_drive,
osculating_apsides), `crates/csp-view/src/lib.rs` (TutorialRes plumbing L1323-1342,
m10_panels L623-664, top-bar re-open L432-436, presentation scheduling L1040-1049),
`crates/csp-view/tests/tutorial.rs`. Cross-checked against `csp-view/src/flight.rs`
(focus convention, latched staging, HUD conic gating), `csp-view/src/vab.rs`
(VabInput.open, dirty-gated analysis), `csp-sim/src/lib.rs` (ToRails paths,
debris_since, SetManeuver residence gate), `csp-app/src/lib.rs` (starter seed).

## Verified-clean areas (checked per focus list)

- **Predicates read staged mirrors only:** `tutorial_drive` builds `TutorialInput`
  from `FlightSnapshot` (sync-staged), `VabInput.open`, and `VabSessionRes`'s
  dirty-gated analysis — never the sim (§2.5). Every step's predicate
  (`step_complete`, csp-ui/src/tutorial.rs:141-152) is over real outcomes:
  VAB-open flag, resolved thrust analysis, a staged vessel, commanded throttle,
  real altitude, closed-conic periapsis. No button-press credit.
- **Debris exclusion is correct and intentional:** `snap.vessels.iter().rev()
  .find(|v| !v.debris)` (csp-view/src/tutorial.rs:70). Breakup marks *every*
  fragment (`debris_since = Some(tick)`, csp-sim/src/lib.rs:9785,9883), so a
  shattered craft leaves only debris → `has_vessel=false` → rewind fires
  (pinned by `debris_only_snapshot_rewinds_as_vessel_lost`). `JettisonPart`
  spawns no fragment at all (csp-sim/src/lib.rs:6066-6069), so jettison can't
  hijack focus. `vessels` is `BTreeMap`-ordered ascending `VesselId` with a
  monotonic counter — `.rev().find()` is the newest non-debris spawn, matching
  `focus_vessel`'s convention minus wreckage, as documented.
- **`{key}` hints are staleness-proof by construction:** `hint_for` resolves
  against `session.keys` — the *live* (applied) keymap mirrored by
  `set_live_keymap` (lib.rs:1224-1230), not the settings panel's staged edit
  copy — at draw time, per frame. `hint_labeled` (settings.rs:653) joins all
  bindings or prints `unbound` → `[unbound]`; observed layout labels
  (`set_key_label`) flow through. A rebind+Apply can never leave the coaching
  line naming a dead key.
- **Dismiss/Restart/re-open semantics:** `observe` folds progress while
  `active=false` (tutorial_drive runs unconditionally in Presentation), so a
  re-opened tutorial resumes truthful state; `start()` preserves progress,
  `restart()` clears step/finished/vessel_lost and keeps `best_*` history.
  `finished` is a true latch — rewind is unreachable past it (early return
  before the rewind check, csp-ui/src/tutorial.rs:227-240). Tests cover all of
  this.
- **Catch-up advance is honest:** the `while step_complete` pass skips legit
  bypasses (stock fallback, loaded craft, `already_flying_skips_early_steps`)
  without crediting unsatisfied steps.
- **osculating_apsides core math + guards:** correct closed form
  (`p = h²/μ`, `a = −μ/2ε`, `e = √(1−p/a)`, `ra = p/(1−e)`, `rp = p/(1+e)`).
  `energy ≥ 0` rejects escapes (an escape can't falsely credit the orbit step);
  `e ≥ 1` rejects the radial/degenerate plunge (incl. `vel = 0` on a
  non-spinning pad: `h=0 → p=0 → e=1 → None`). NaN can't falsely complete
  anything downstream — all consumers use `>` predicates and `f64::max`
  preserves the non-NaN best.
- **Ordering: step-0 `in_vab` reads `VabInput.open`**, the same flag `View::mode()`
  and `build_ui`'s anchor derive from (lib.rs:308-314,1393-1401) — the VAB step
  completes on the real mode switch, and the overlay anchor can never disagree
  with the predicate.
- **NaN/edge safety of `observe`:** `alt_m`/`throttle`/periapsis predicates all
  fail on NaN; `best_alt_m` can't be NaN-poisoned (`f64::max` returns the
  non-NaN operand). `e2e` test (`tests/tutorial.rs`) drives the real
  VAB→launch→throttle→climb→space path through `View` and passes.

## Findings

### F1 — MEDIUM: the tutorial's focus ignores `FlightFocusOverride` — a pinned vessel makes the checklist watch the wrong ship

- **File:** `crates/csp-view/src/tutorial.rs:70` — `let focus =
  snap.vessels.iter().rev().find(|v| !v.debris);` — versus the real focus rule
  at `crates/csp-view/src/flight.rs:1128-1131`: `focus_override.target
  .and_then(find) .or_else(|| focus_vessel(&snap.vessels))`.
- **Problem:** The documented "flight focus convention" is *pinned target else
  newest*; `tutorial_drive` implements *newest non-debris*, dropping the pin.
  `recover_control` (map-mode dead-stick recovery) sets the pin
  (`lib.rs:1542-1544`) — and the pinned vessel's `VesselId` typically predates
  a later spawn, so it is *not* `last()`. While the pin is active the HUD,
  chase camera, and `SetThrottle`/`SetAttitude` routing all follow the pinned
  vessel, but the tutorial measures throttle/altitude/apsides off the newest
  non-debris vessel — a different ship.
- **Evidence:** `flight_focus()` (lib.rs:1581-1586) returns `FlightUiData.focus`
  — the pin-aware target the player's commands reach — while the tutorial reads
  its own rule. `recover_control` is reachable from map view whenever an
  eligible on-rails vessel exists (`to_bubble_eligible`, lib.rs:1552-1563);
  on-rails vessels exist in shipped scenarios (on-rails `SpawnVessel`, burn
  completions). This is the same divergence P11-F1 already found for
  `sfx_edges`/audio voices — the tutorial is a third consumer repeating it.
- **Impact:** Player recovers an older vessel while a newer non-debris vessel
  is staged → throttling/climbing/orbiting the *controlled* vessel never
  satisfies steps 3-6 (the watched vessel sits at throttle 0 / alt ~0) — the
  exact "stuck step despite doing the thing" failure this cluster is meant to
  exclude. Not reachable on a stock single-vessel first flight; requires
  multi-vessel play + recovery.
- **Suggestion:** Have `tutorial_drive` take `Res<FlightFocusOverride>` (or
  read `FlightUiData.focus`, which is already the resolved id) and prefer the
  pinned vessel when staged and non-debris, falling back to newest non-debris.
  Fix jointly with P11-F1 — the right shape is one shared pin-aware resolver.

### F2 — MEDIUM: "vessel lost" rewind conflates removal with residence — a live vessel that goes OnRails reads as a crash

- **File:** `crates/csp-view/src/tutorial.rs:70,87` (`has_vessel` = staged
  non-debris presence) driving `crates/csp-ui/src/tutorial.rs:230-233`
  (`step > STEP_LAUNCH && !has_vessel` → rewind + `vessel_lost` banner
  "vessel lost — build or load a craft and LAUNCH again", L335-340).
- **Problem:** The staged set covers only `InBubble` + `SurfaceLatched`
  (`VesselState::OnRails(_) => {}`, flight.rs:710). A vessel that transitions
  bubble→rails *while alive* disappears identically to an impact removal.
  Reachable paths: `finish_burns` → `apply_to_rails_at` at every scheduled
  maneuver-burn completion (csp-sim/src/lib.rs:4182), and any scripted/driver
  `ToRails` (csp-bench issues it; the command exists for scripts/agents).
- **Evidence:** after a burn completing into a *closed* orbit — the exact
  outcome step 6 asks for — the vessel leaves `snap.vessels`, `has_vessel`
  goes false, `observe` rewinds to `STEP_LAUNCH` and shows "vessel lost". The
  orbit can't be verified either: `TutorialInput` has no on-rails channel, so
  `periapsis_m` is `None` until the vessel is `ToBubble`d back. Mitigating:
  the rewind is honest only if no *other* non-debris vessel is staged
  (a surviving second craft suppresses it entirely); and `SetManeuver`
  requires the vessel to be OnRails first (csp-sim/src/lib.rs:5587-5589),
  so a stock always-in-bubble tutorial craft can't reach this — it needs a
  multi-vessel/scenario flow or script control.
- **Impact:** In those flows the checklist rewinds to "Launch from the pad"
  and banners a loss that didn't happen at the moment of success; it
  self-heals only if the player recovers the vessel (map → recover control),
  which the tutorial never teaches.
- **Suggestion:** Give `TutorialInput` a residence-aware bit — e.g. stage the
  count/id of live non-debris vessels (arena-wide, not just the flight set) —
  so `observe` can distinguish "vessel removed" (rewind + lost banner) from
  "vessel on rails" (hold step, coach recovery). At minimum, don't print
  "vessel lost" when a live vessel still exists anywhere.

### F3 — LOW: `tutorial_drive` computes osculating apsides for latched vessels — the HUD explicitly declines to

- **File:** `crates/csp-view/src/tutorial.rs:73-78` — no `latched` check;
  `flight_present` gates the same derivation on `!focus.latched`
  (flight.rs:1180-1182: "Latched bodies ride pure co-rotation — no honest
  conic to report").
- **Problem:** A surface-latched vessel stages `vel` = CoM co-rotation
  velocity (flight.rs:758-761). On a spinning body the closed form then
  solves a conic for a parked craft — `Some(...)` junk feeding
  `apoapsis_m`/`periapsis_m`, which `observe` folds into `best_periapsis_m`.
  It can *not* falsely credit the orbit step (periapsis ≤ current radius ⇒
  below surface ⇒ `p < space_alt`), so the damage is a bogus "best periapsis:
  −N km" line — but it contradicts the HUD's documented honesty rule and
  silently depends on that geometric bound for correctness.
- **Suggestion:** Skip the apsides when `v.latched`, mirroring `flight_present`.

### F4 — LOW (nit): `osculating_apsides` degenerate guard doesn't reject NaN

- **File:** `crates/csp-view/src/tutorial.rs:36-48`. `r <= 0.0` doesn't
  reject `r = NaN`; the NaN then flows through `p`/`energy` (`NaN >= 0.0` is
  false → passes the escape gate), `(1.0 − p/a).max(0.0)` maps NaN → 0 →
  `e = 0` → returns `Some((NaN, NaN))` instead of `None` — violating the
  documented "`None` while open/unknown" contract of `TutorialInput`.
- **Impact:** None today — every downstream use is a `>` comparison or
  `f64::max`, both NaN-safe. But the "degenerate" guard is one token short of
  complete; `!r.is_finite()` (and `e.is_finite()` before `Some`) closes it.

### F5 — LOW (nit): `tutorial_drive` is not fully ordering-independent inside the schedule — one-frame-stale analysis read

- **File:** `crates/csp-view/src/tutorial.rs:79-86` reads
  `vab.0.analysis()` — the dirty-gated cache `vab_present` recomputes
  (`vab.rs:229`) in the *same unordered* `presentation` tuple
  (lib.rs:1041-1049). The comment at tutorial.rs:55-57 claims ordering
  independence, which holds for `FlightSnapshot` but not the VAB cache.
- **Impact:** Benign direction only — an edit's analysis lands one frame
  later for the tutorial if `tutorial_drive` runs first (step-1 tick a frame
  late). No stuck state; noting because the comment overclaims.

## Observation (not a defect)

- Production `App::new_inner` seeds a starter pad vessel
  (`seed_starter_craft`, csp-app/src/lib.rs:317-325), so the very first
  `observe` sees `has_vessel` and auto-completes steps 0-2 — the checklist
  opens already on "Open the throttle". That is the documented outcome-skip
  semantics ("the stock fallback"), but worth naming: on the stock first run
  the VAB/assemble/launch coaching never displays as the *current* step, and
  Restart can never actually show step 0 while any non-debris vessel is
  staged (the catch-up loop re-derives it instantly). If onboarding through
  the VAB is the intent, the seed and the outcome-skip rule are in tension.
