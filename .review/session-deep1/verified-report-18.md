# Verified Report — Cluster C18 (csp-ui)

Verification of `review-report-18.md` against the actual source in
`crates/csp-ui/src/*`, `crates/csp-view/src/{lib,vab,flight,demo}.rs`,
`crates/csp-app/src/{lib,settings_io}.rs`, plus downstream consumers in
`crates/csp-phys`/`crates/csp-sim` where a finding's claimed impact depends on
them. Verdicts: **11 CONFIRMED, 0 DISMISSED.** Line-number and mechanism
corrections are noted inline.

---

### [HIGH] Finding 1: navball ground-cap fan paints sky as ground at positive pitch — **CONFIRMED**

Source verified: `crates/csp-ui/src/navball.rs:133-163` (boundary at 135-148,
fan about `to_px([-ux,-uy,-uz])` at 149-163), exactly as reported.

**Independent reproduction.** I re-implemented the boundary + fan
point-for-point in Python and ran a 40k-sample Monte-Carlo per pitch angle
(`up = (0, cos p, sin p)` — the convention `flight.rs:740` `conj.rotate(up)`
produces for a level-roll pitch-up; `uz = sin p` there, matching the report).
Percentages below are normalized to disc area (the report's convention); raw
square-sample figures are ~0.79× smaller.

| pitch | sky falsely painted (mine) | (report) | true ground (mine) | (report) |
|-------|----------------------------|----------|--------------------|----------|
| +15°  |  9.1% | 10.6% | 37.3% | 36.5% |
| +30°  | 21.1% | 23.2% | 24.6% | 25.3% |
| +45°  | 31.7% | 33.3% | 14.9% | 14.7% |
| +60°  | 39.1% | 40.7% |  6.6% |  6.6% |
| +85°  | 46.7% | 48.6% |  0.2% |  0.1% |

Same magnitudes within Monte-Carlo noise — at +85° the ball renders ~half brown
when it should be ~all sky. Nose-down attitudes show ~0.5–1.2% missed ground
**and ~0.9–2.4% overpaint** (the report only counted the missed sliver; both
exist, from boundary discretization plus the chord artifacts described below).

**Mechanism — confirmed with one correction.** The report's claim "the fan
center sits on the sky side whenever `uz > 0`" is right for `uz > 1/√2`
(pitch > 45°, center inside the projected horizon ellipse = the sky cap) but
the "between chord and arc" sub-claim for `uz < 1/√2` is geometrically wrong:
the fan center sits at distance `rho = cos p` on the nadir side while the arc
only dips to `sin p`, so for `0 < p < 45°` the center is *inside* the ground
crescent. The overpaint at small positive pitch (9–21%) therefore comes from
the region not being star-shaped about the center (the sky cap is a concave
bite; triangles to the arc's flanks cross it), not from a fully-outside center.
The conclusion — the "always star-shaped" comment is false and the fill is
wrong at every positive pitch — stands. Suggested decomposition (nadir
half-disc + half-ellipse cap painted `ground` iff `uz < 0`) is sound.

**Additional defects found while verifying (same code, unreported):**
- `indices.windows(2)` over 80 boundary verts emits 79 triangles — the closing
  edge `boundary[79] → boundary[0]` is never fanned, leaving a thin unfilled
  wedge across the chord.
- The polygon path itself runs the rim arc left→bottom→right, same direction
  as the ellipse arc (navball.rs:144-147), so edges `b[40]→b[41]` and the
  implicit close `b[79]→b[0]` are two nearly-coincident chords crossing the
  disc; even-odd fill makes the enclosed region approximately right anyway
  (the double-covered sliver between the chords is thin), but it's a latent
  fragility the "correct boundary" claim glosses.

Compass-letter edge (navball.rs:196-198): confirmed — dead-astern letters
(`v[2]<0`, `l→1e-9` → `v0/l·0.97 ≈ 0`) draw at ball center over the reticle;
the prograde clamp at 219-223 does `continue` on `l < 1e-9`. Flying due-south
puts "S" on the reticle.

---

### [MEDIUM] Finding 2: settings X-close leaves rebind armed — **CONFIRMED**

Every link verified:

- `csp-ui/src/settings.rs:463-557` — `egui::Window::open(&mut open)` →
  `state.open = open` at line 556. No `listening` cleanup anywhere in the
  function. `m10_panels` (csp-view/src/lib.rs:470-471) only calls
  `settings_panel` while `settings_state.open`, so once closed nothing else
  runs that could clear it.
- `csp-view/src/lib.rs:1043-1051` — `toggle_settings_panel` does disarm on
  close (`if !r.0.open { r.0.listening = None; }`), confirming the asymmetry:
  the keybinding-toggle path cleans up, the window's own X does not.
- `csp-view/src/lib.rs:1056-1058` — `rebind_target()` returns `listening`
  unconditionally (does not gate on `open`).
- `csp-app/src/lib.rs:1117-1131` — `key_press` checks
  `self.view.rebind_target().is_some()` before all dispatch and `return`s:
  Escape → `set_rebind_target(None)` (consumed, never reaches
  `OpenSettings` dispatch); any other bindable key → `bind_key(action, b)`
  which writes into the *staged* keymap and sets a `notice` only visible
  inside the closed window (csp-view lib.rs:1069-1077). The rogue binding
  goes live on the next Apply — matching the report exactly.

Corollary confirmed: `m10_panels` restages `Settings::default()` on
`reset_defaults` at csp-view/src/lib.rs:473-476 without clearing `listening`.
Suggested fix (`if !open { state.listening = None; }` before line 556) is
correct and minimal.

---

### [MEDIUM] Finding 3: LAUNCH consumes the dirty-gated `analysis` cache — **CONFIRMED**

Mechanism verified end-to-end:

- `recompute` production call sites: `vab_editor` (csp-ui/src/vab.rs:717 —
  UiBuild, VAB mode only via build_ui:341-343) and `vab_present`
  (csp-view/src/vab.rs:223 — Presentation, gated `if !input.open` at 218).
  Nothing recomputes while the VAB is closed; `dirty` survives a mutation
  made on the last VAB frame if the user exits before the next Presentation
  pass (mode switch goes through `key_press` → `dispatch` → `set_mode`
  outside the schedules).
- `launch_craft` (csp-view/src/lib.rs:1268-1369): `v.analysis().cloned()` at
  1271 (no recompute), fresh `session.to_part_graph()` at 1279, `analysis?`
  at 1280 → `let Some(craft) = craft_def else { return };` at 1302 — silent
  no-op with no notice *and* no mode switch. LAUNCH is drawn every frame in
  every mode (top_bar, csp-ui/src/vab.rs:675-677, called unconditionally at
  build_ui:317). Empty session still launches via `stock_flight_craft`
  (1276-1277), so the silent path is reachable only with a non-empty session
  whose cache is `None` — i.e., an `analyze` failure or a recompute that
  never ran. Confirmed.
- The codebase itself admits the contract: `demo.rs:189-193` — *"The launch
  path resolves the craft from the session's cached analysis — recompute now
  (the VAB's own frames would do it, but the seed launches before the editor
  ever opens)"* — the seed path works around exactly the defect the finding
  describes; the LAUNCH button does not.

**Downstream consequence — confirmed, more precisely than the report stated:**
- Attach-then-launch (fresh graph N+1, stale `effective` N): the new node
  gets `effective_index = N` (`to_part_graph` assigns ascending-`PartNodeId`
  positions, csp-phys session.rs:1363-1380) → `weld::validate_structure`
  returns `EffectiveIndexOutOfRange` (weld.rs:363-369). `launch_craft`'s own
  weld at lib.rs:1332 fails (z_min falls back to 0), and the enqueued
  `SpawnPhysicalVessel` fails in `apply_spawn_physical`
  (csp-sim lib.rs:4579-4580) → `Recoverable` → `report.rejected` → only a
  `log::warn!` (csp-app lib.rs:714-716) — invisible in-game. Meanwhile
  `set_mode(ViewMode::Flight)` at lib.rs:1368 still runs, so the user is
  dumped to flight view with nothing spawned.
- Remove / scale / variant / stage edits (fresh graph ≤ N, stale `effective`
  N): every `effective_index` stays in range, so the weld *succeeds* with the
  `effective` list positionally misassigned to shifted nodes — a silently
  wrong vessel (wrong colliders/mass/costs/reliability), or stale pre-edit
  part data. Confirmed via the positional `effective_index` contract
  (session.rs:1355-1376; the same convention csp-view/vab.rs:226-229 relies
  on).
- Corollary confirmed: `selected_spec` (csp-ui/src/vab.rs:1209-1214) does
  `a.effective.get(idx)` positionally; stale analysis misattributes specs.

Suggested fix (call `v.recompute()` in `launch_craft`; push a
`VabEvent::Notice` on failure — the channel exists at csp-view/vab.rs:159-161)
is correct.

---

### [LOW] Finding 4: stale/hardcoded key names — **CONFIRMED**

- `help_panel` doc claims "the live `Keymap`" (csp-ui/src/lib.rs:409-412) but
  is fed `&settings_state.0.settings.keys` — the **staged** copy — at
  csp-view/src/lib.rs:**326** (report said 328; off by two lines).
- `stage_settings` (csp-view/src/lib.rs:1010-1012) is called only at
  csp-app/src/lib.rs:284 (`assemble`) plus a test at 1724 — never on open, so
  an unapplied rebind misleads the card for the whole session. After Apply,
  staged == live, so the card is wrong only for pending edits — as reported.
- Hardcoded hints verified: csp-ui/src/lib.rs:228 (`[M] flight [Tab] focus
  WASD: pan`), 345 (`ON PAD — [Z] to launch`), 388 (`[F1] controls · [Z]/[X]
  throttle · [M] map [V] VAB`), vab.rs:1355 (`[V] exit`), tutorial.rs:62
  (`[Z] for full throttle`). **One more unlisted**: lib.rs:328 (`[V] VAB ·
  [M] map · [F1] controls`, the no-vessel hint).

### [LOW] Finding 5: hand-edited settings.ron bypasses validation — **CONFIRMED**

`#[serde(transparent)]` on `KeyBinding` (settings.rs:271-273) →
`ron::from_str::<Settings>` in `load_settings_from`
(csp-app/src/settings_io.rs:83-92) takes raw strings with no `parse` gate and
no dedup. `key_token` (settings_io.rs:134-190) can only emit
`NAMED_KEY_TOKENS`-valid tokens, so a file token like `"arrowup"` (which
`parse` rejects, settings.rs:323-330) binds an action that can never fire —
yet displays as bound in the grid (settings.rs:568-584) and help card.
Duplicate tokens across actions: `action_for` resolves first in
`KeyAction::ALL` order (387-393) while `bound_actions` (395-406) and the
panels show it on both rows. Confirmed.

### [LOW] Finding 6: staged `Settings` not re-synced on reopen — **CONFIRMED**

`stage_settings` runs only at startup (csp-app lib.rs:284); nothing restages
on `open` transitioning true (`toggle_settings_panel`,
csp-view/src/lib.rs:1043-1051, only flips `open`/`listening`). The doc at
csp-view/src/lib.rs:1007-1009 ("the window always opens on the applied
state") is false after discarded edits — confirmed as reported, including the
"design call" caveat.

### [NIT] Finding 7: `NavballInput::heading_deg` dead field — **CONFIRMED**

navball.rs:34-36 declares it; csp-view/src/lib.rs:364 populates it; `navball()`
reads only `up`/`north`/`east`/`prograde` — never `heading_deg`. The tape is
fed `input.heading_deg` (the `FlightHudInput` field) at csp-ui/src/lib.rs:400-
405. Confirmed dead weight.

### [NIT] Finding 8: module-doc contradiction on the dirty contract — **CONFIRMED**

vab.rs:28-30 says "a stage op or a pick does not re-resolve" — but
`VabAction::SetStage` sets `dirty = true` at 437 (and `InsertStage`/
`RemoveStage`/`CompactStages` at 481/484/489 likewise). Picks
(`Select`, 430-432) alone match the doc. Confirmed.

### [NIT] Finding 9: hardcoded `64.0` ticks/s — **CONFIRMED**

`/ 64.0` at csp-ui/src/lib.rs:551, `* 64.0` at 573. `csp_core::TICK_HZ = 64`
(csp-core/src/time.rs:9); csp-ui's Cargo.toml has csp-core as a dev-dep only —
the duplication is layering-forced exactly as the report notes.

### [NIT] Finding 10: `best_periapsis_m` recorded, never displayed — **CONFIRMED**

Field at tutorial.rs:156-158, folded at 193-195, never read in
`tutorial_overlay` (244-310 shows only `best_alt_m`, 294-298). Confirmed.

### [NIT] Finding 11: tutorial overlay overlaps the VAB Craft inspector — **CONFIRMED**

tutorial.rs:246 anchors `RIGHT_TOP, vec2(-8,30)` default width 290;
vab.rs:848-851 anchors `RIGHT_TOP, vec2(0,30)` default 320 — same corner.
`m10_panels` runs after `vab_editor` in `build_ui` (csp-view/src/lib.rs:341-
343 vs 375-381), so the checklist paints over the inspector's top-right.
Draggable, so self-recoverable — as reported.

---

## Notes on the report's "Verified clean" section

Spot-checked, consistent with source: `KeyBinding::parse` rejects
`"arrowup"`/`"ab"`/`""` (settings.rs:323-330, tests 695-706); `Keymap::bind`
evicts (412-422); `recompute`'s clear-then-compute gating is sound
(vab.rs:364-379) — the defect is indeed in the caller's gating (Finding 3);
`circle_front_points` returns empty for degenerate `up` (navball.rs:84-101);
`heading_tape` wrap math is clean; `VabEvent::Notice` plumbing exists as the
Finding 3 suggestion assumes.

## Verdict summary

| # | Severity | Verdict |
|---|----------|---------|
| 1 | HIGH | CONFIRMED — reproduced: ~9→47% of the disc falsely ground at +15→+85° pitch; minor geometric-detail correction noted |
| 2 | MED  | CONFIRMED — X-close leaves `listening` armed; next keypress swallowed into staged keymap |
| 3 | MED  | CONFIRMED — stale `effective` vs fresh graph → weld rejection (log-only) or positionally misassigned part data; `analysis=None` → silent no-op. `demo.rs:189-193` documents the very workaround LAUNCH lacks |
| 4 | LOW  | CONFIRMED — help_panel fed staged keymap (line 326, not 328); all hardcoded hints verified + one more instance |
| 5 | LOW  | CONFIRMED — transparent serde → no validation on load |
| 6 | LOW  | CONFIRMED — `stage_settings` never runs on open |
| 7 | NIT  | CONFIRMED |
| 8 | NIT  | CONFIRMED |
| 9 | NIT  | CONFIRMED |
| 10 | NIT | CONFIRMED |
| 11 | NIT | CONFIRMED |
