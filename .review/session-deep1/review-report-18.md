# Deep Review — Cluster C18 (csp-ui)

Files read in full: `lib.rs`, `vab.rs`, `settings.rs`, `tutorial.rs`, `navball.rs`, plus the
consuming sites in `csp-view`/`csp-app` needed to prove user-facing impact
(`csp-view/src/lib.rs` UiBuild + `launch_craft` + `toggle_settings_panel`,
`csp-view/src/vab.rs` `vab_present`, `csp-view/src/flight.rs` `stage_attitude`,
`csp-app/src/lib.rs` `key_press`/`bind_key`/`settings_apply`).

---

### [HIGH] Finding 1: navball ground-cap triangle fan paints large regions of sky brown
**File:** `crates/csp-ui/src/navball.rs:143-163` (fan center at line 151, triangles 155-162)

**Problem:** The ground-cap boundary polygon is correct (front-hemisphere horizon
arc + the nadir-side rim arc — verified analytically: `vz = −rho·cos θ`, endpoints
coincide at the antipodal pair `(±uy, ±ux)/rho`). But the fill is a triangle **fan
about the nadir's orthographic projection** `to_px([-ux,-uy,-uz])` = screen point
`(ux,−uy)·rho`. That point is inside the ground crescent only for steeply
nose-down attitudes. Whenever the nose is above the horizon (`uz > 0`), the fan
center sits on the *sky* side of the boundary — inside the projected horizon
ellipse for `uz > 1/√2`, between chord and arc below that — and every fan
triangle then covers the lens between the fan center and the boundary arc.
The comment "the cap is always star-shaped from there" is false.

**Evidence:** Monte-Carlo check of the constructed boundary+fan vs. the true
ground set `{v·u < 0}` on the front hemisphere (`up = (0, cos p, sin p)`):

| pitch | sky falsely painted ground | true ground fraction |
|-------|---------------------------|----------------------|
| +15°  | 10.6% | 36.5% |
| +30°  | 23.2% | 25.3% |
| +45°  | 33.3% | 14.7% |
| +60°  | 40.7% |  6.6% |
| +85°  | 48.6% |  0.1% |

At +85° the ball renders ~half brown when it should be ~all sky. Nose-down
attitudes also lose ~1.2–2% of true ground (a sliver the fan misses). The white
horizon arc (`boundary[..41]`) is drawn correctly, so the artifact reads as
"ground fill extends far above the horizon line."

**Impact:** The primary flight instrument is wrong in the exact regime it
matters most — a rocket climbing at 45–80° pitch shows a big false ground mass.
Pure-egui, no sim dependency; deterministic per frame.

**Suggestion:** Decompose, don't fan. The arc endpoints are antipodal on the
rim, so the region is exactly: the nadir-side **half-disc** (bounded by the
diameter through the arc endpoints), plus the convex **half-ellipse cap**
between the arc and that diameter — which is *sky* when the arc bulges into the
nadir half (`uz > 0`) and *ground* when it bulges into the zenith half
(`uz < 0`). Concretely: fill the half-disc brown (simple fan about center — the
half-disc is star-shaped from center), then fill the half-ellipse cap (convex —
trivial fan about the arc centroid) with `ground` when `uz < 0` and with `sky`
when `uz > 0`. Alternatively ear-clip the closed boundary.

Related edge (same file, minor): the compass-letter rim clamp at `navball.rs:196-198`
draws the letter at ball *center* when the direction is dead astern
(`v[2] < 0`, `l ≈ 0` → `v0/l·0.97 ≈ 0` → `to_px` = center, over the reticle).
The prograde clamp two dozen lines below correctly `continue`s on `l < 1e-9` —
do the same here.

---

### [MEDIUM] Finding 2: closing the settings window via its X button leaves the rebind capture armed — the next keypress is silently swallowed and rebound
**File:** `crates/csp-ui/src/settings.rs:468-557` (`settings_panel`), specifically `state.open = open` at line 556 with no `listening` cleanup.

**Problem:** `toggle_settings_panel` in `csp-view` (lib.rs:1043-1050) explicitly
drops an armed rebind on close ("a closed window drops an armed rebind so the
next key press is not swallowed by a panel the user can no longer see") — but
that cleanup only exists on the *keybinding-toggle* path. The window's own
close button goes through `egui::Window::open(&mut open)` → `state.open = open`
inside `settings_panel`, which clears **only** `open`, never `listening`.
`csp-app::key_press` (lib.rs ~1115) checks `self.view.rebind_target().is_some()`
*before* all normal dispatch and swallows the key: any bindable key is captured
into the staged keymap via `bind_key` (with an invisible notice, since the
window is closed), and Escape is consumed to disarm rather than toggling
settings.

**Evidence:** Click "rebind" on any action → close the window with its X →
press `m`: the map does not open; `m` is bound to the armed action in the
staged keymap, becoming live on the next Apply of *any* settings change.

**Impact:** One stolen keypress plus a latent rogue binding — classic
panel-state drift, invisible to the user.

**Suggestion:** In `settings_panel`, before `state.open = open`:
`if !open { state.listening = None; }`. (Also consider disarming on
`reset_defaults` — csp-view restages `Settings::default()` at lib.rs:473-476
without clearing `listening`.)

---

### [MEDIUM] Finding 3: LAUNCH consumes the dirty-gated `analysis` cache — stale `effective` list mismatches a fresh `to_part_graph`, and `None` makes LAUNCH a silent no-op
**File:** `crates/csp-ui/src/vab.rs:364-379` (`recompute` gating) interacting with
`crates/csp-view/src/lib.rs:1268-1290` (`launch_craft`) and
`crates/csp-view/src/vab.rs:210-224` (`vab_present`, gated on `input.open`).

**Problem:** `recompute` only runs inside `vab_editor` (UiBuild, VAB mode) and
`vab_present` (Presentation, gated `input.open`). A mutation applied during a
VAB frame (attach, stage change, `LoadBytes`) sets `dirty`; if the user leaves
the VAB before the next Presentation pass (press V/M — the mode switch lands in
the next Input phase), the flag stays set and `analysis` remains stale
*indefinitely* until the VAB is reopened. The top bar's LAUNCH button is
visible in every mode. `launch_craft` then does
`session.to_part_graph()` — fresh, N+1 nodes — but takes `parts`,
`part_costs`, and `reliability` from `analysis.effective` — stale, N entries —
producing a length-mismatched `CraftDef` (graph nodes vs. weld inputs vs.
per-part tables no longer align; the `effective_index` positional contract in
`session.rs:1361-1376` is violated downstream). Separately, `let analysis =
analysis?` makes LAUNCH silently return whenever the cache is `None` (analyze
failure, or a recompute that never ran) — no button feedback, no `notice`.

**Evidence:** Attach a part in the VAB → press `V` to exit → click LAUNCH.
`vab.0.analysis()` still describes the pre-attach craft while `to_part_graph`
includes the new node.

**Impact:** A spawned vessel built from mismatched graph/parts tables (wrong
weld data or a spawn failure), or a dead LAUNCH button with zero feedback for a
craft that fails to resolve — both reachable through normal play.

**Suggestion:** Call `v.recompute()` (cheap when clean) at the top of
`launch_craft` before cloning `session`/`analysis`, and push a
`VabEvent::Notice` (the channel already exists, `apply_vab_input` lib.rs:159-161)
when the resolve fails so the refusal is visible outside the VAB. The csp-ui
side could additionally mark `VabUiOutput::launch` handling as requiring a
clean readout.

Minor corollary: `selected_spec`/`stage_rows` read the positional `effective`
list; after a mid-session `Remove`, a stale analysis misattributes one part's
reliability/stats to another for a frame (transient, but the same contract).

---

### [LOW] Finding 4: the help card and all hint bars display stale/hardcoded key names after rebinding
**File:** `crates/csp-ui/src/lib.rs:413-461` (`help_panel` doc claims "the live
`Keymap`"), fed at `crates/csp-view/src/lib.rs:328` with
`settings_state.0.settings.keys` — the **staged** keymap; plus hardcoded hints
at `lib.rs:228` ("[M] flight [Tab] focus WASD: pan"), `lib.rs:345` ("[Z] to
launch"), `lib.rs:388` ("[F1] controls · [Z]/[X] throttle · [M] map [V] VAB"),
`vab.rs:1355` ("[V] exit"), `tutorial.rs:62` ("[Z] for full throttle").

**Problem:** Two drift sources. (a) `help_panel` reads the staged copy — a
rebind (or "Reset to defaults") lands in the staged map at `bind_key`/
`reset_defaults` time, so the controls card shows unapplied bindings as live
until Apply (and the staged copy is only re-synced at startup/file reload —
`stage_settings` at `csp-view/src/lib.rs:1010` — so an unapplied rebind keeps
the card wrong all session). (b) The fixed hint strings name the *default*
keys and never follow a rebind — rebind throttle to [T]/[G] and the HUD still
says [Z]/[X].

**Impact:** Post-rebind, the game's own help text lies about the controls —
doubly confusing for the tutorial's "[Z]" coaching of a user who rebound
throttle.

**Suggestion:** (a) Pass the *live* `Keymap` (or the applied `Settings`) into
`help_panel` — the staged-vs-live distinction already exists app-side; (b)
either render hint text from the live keymap (`action_for`/`bindings.get`) or
drop the key names from static hints. Noted as LOW because the defaults are
correct for unmodified installs.

---

### [LOW] Finding 5: a hand-edited `settings.ron` bypasses all binding validation — dead tokens and multi-action keys load silently
**File:** `crates/csp-ui/src/settings.rs:271-273` (`#[serde(transparent)]
KeyBinding`), `323-330` (`parse` is UI-side only), `387-393` (`action_for`
first-match), `412-422` (`bind` eviction only applies through the API).

**Problem:** `KeyBinding::parse` validates tokens and `Keymap::bind` maintains
one-action-per-key — but serde deserialization goes straight through the
transparent `String`: no `parse` gate, no dedup. A file can (a) bind a token no
producer can emit (e.g. `"arrowup"` — which `parse` itself rejects) that then
displays as a bound key that never fires — exactly the "silently dead-bound"
outcome the `parse` doc says is refused; (b) bind one token to two actions —
`action_for` resolves the first in `KeyAction::ALL` order while `bound_actions`
and the settings/help grids show it on both rows.

**Impact:** Only reachable via manual file editing, but the model documents
validation that the load path doesn't enforce; the symptom (dead binding,
ambiguous resolution) is user-visible in the panels.

**Suggestion:** Add `Keymap::sanitize(&mut self)` (run after load): filter each
list through `KeyBinding::parse`-equivalent validity and evict duplicate
tokens keeping the `KeyAction::ALL`-first winner — one function, no serde
surgery needed.

---

### [LOW] Finding 6: staged `Settings` is not re-synced when the window reopens — discarded edits reappear and look applied
**File:** `crates/csp-ui/src/settings.rs:433-446` (staged-copy model) +
`crates/csp-view/src/lib.rs:1010-1012` (`stage_settings` — "called at startup
and whenever the file is (re)loaded, so the window always opens on the applied
state").

**Problem:** The doc claim only holds if `stage_settings` runs on open — it
doesn't. Edit a slider → close without Apply → reopen: the unapplied value is
still shown, indistinguishable from an applied one. Whether drafts should
persist is a design call, but the current state silently violates the stated
contract.

**Impact:** User can't tell applied from pending settings across an open/close
cycle.

**Suggestion:** Re-stage live settings when `open` transitions to true (one
line in `toggle_settings_panel` or on the `outbox`), or document draft
persistence and drop the "applied state" claim.

---

### [NIT] Finding 7: dead field — `NavballInput::heading_deg` is never read
**File:** `crates/csp-ui/src/navball.rs:35-36`; `lib.rs:404` passes
`input.heading_deg` (the `FlightHudInput` field) to `heading_tape`, not
`input.navball.heading_deg`. The staged copy is pure dead weight in the input
struct — remove it or wire the tape to it so there's one source.

### [NIT] Finding 8: module-doc contradiction on the dirty contract
**File:** `crates/csp-ui/src/vab.rs:28-31`: "a stage op or a pick does not
re-resolve" — but `VabAction::SetStage` sets `dirty = true` (line 437) and
stage changes legitimately invalidate the per-stage readout, so it *does*
re-resolve. The doc should say picks don't re-resolve; as written it
contradicts the code it documents.

### [NIT] Finding 9: hardcoded `64.0` ticks/s twice in `maneuver_panel`
**File:** `crates/csp-ui/src/lib.rs:551` (`/ 64.0` ETA) and `573`
(`* 64.0` seconds→ticks). `csp-core::TICK_HZ = 64` can't be imported under §2.1
(the crate correctly avoids the edge — `csp-core` is a dev-dep only), so the
constant is duplicated; if the tick rate ever moves, node placement and ETA
silently desync. Consider a `ticks_per_second` parameter on
`maneuver_panel`/`ManeuverPanelState`.

### [NIT] Finding 10: `TutorialState::best_periapsis_m` is recorded, never displayed
**File:** `crates/csp-ui/src/tutorial.rs:158,193-195`. Maintained forever
(`f64::MIN` sentinel, `max` folded every frame) but `tutorial_overlay` only
shows `best_alt_m` — dead session data; either surface it (it's the more
meaningful "best") or drop it.

### [NIT] Finding 11: tutorial overlay overlaps the VAB's Craft inspector
**File:** `crates/csp-ui/src/tutorial.rs:245-246` (`RIGHT_TOP, vec2(-8,30)`) vs
`vab.rs:848-850` (`RIGHT_TOP, vec2(0,30)`, 320px). Drawn after the editor, the
checklist covers the right-side inspector — the panel the VAB steps coach the
user toward. Draggable, so self-recoverable; consider anchoring the tutorial
lower or left of the inspector while `in_vab`.

---

## Verified clean (no finding)

- `KeyBinding::parse` tokenization: trim+lowercase, `chars().count()==1` or
  named table — whitespace/`""` rejected, `"arrowup"`/`"ab"` rejected (tested).
- `Keymap::bind`/`clear`/`action_for` eviction and resolution order — sound.
- `VabSession::apply` notice semantics; `Remove`/`Reroot`/`Select`/`LoadBytes`
  selection + dirty handling; `part_limit` on both place and load paths;
  `file_io` refusal — all correct (tests cover).
- `recompute` dirty gating itself (clear-then-compute, error channel, stale
  validation handling) — correct; the defect is in the *caller's* gating
  (Finding 3).
- Navball `circle_front_points`/ortho_basis/ladder: degenerate `up` produces
  empty output (bare ball stays clean); basis safe at `|u0| ≥ 0.9` switch.
- `heading_tape` wrap arithmetic and clipping — correct.
- Tutorial predicates vs staged mirrors (`csp-view::tutorial_drive`) — rewind/
  latch/skip-ahead logic verified; `STEP_LAUNCH=2` indexes the launch step.
- `map_overlay` NDC→screen math and the `|x|,|y|>1.2` margin — behind-camera
  points are already filtered by the producer's `project()` → `Option`.
- `remove_stage(0)`/`insert_stage`/`add_root` refusal paths — safe in csp-phys.
- `attach_auto` facing/twist ranking — matches `snap_transform`'s local-frame
  snap arc; bound-node exclusion correct.
