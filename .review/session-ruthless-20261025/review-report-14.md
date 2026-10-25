# Cluster 14 — csp-ui UX review (vab.rs, lib.rs, settings.rs, navball.rs, tutorial.rs)

Static review only (per instruction — no cargo run). egui version verified against vendored
source: egui 0.37.0. `git status --porcelain crates/csp-ui` was clean at review start; vab.rs is
mid-edit by another agent, so line numbers are against the current tree.

Verified-clean before flagging: Δv/TWR/burn-time math (csp-phys readout.rs — Tsiolkovsky with
mass-flow-weighted Isp, supply-region staging, hand-checked tests; VAB display units match),
navball projection (ground-cap decomposition, seam-split ladder runs, degenerate up/prograde
cases are all tested and the math reads correct), settings schema round-trip/forward-compat/
sanitize and the app-side corrupt-file fallback, `Id::unique` Areas (stable hash in 0.37, not a
per-frame counter), flight-HUD Area overflow (egui auto-constrains Areas into the screen),
launch refusal surfacing, keymap hint derivation. The findings below are what survived.

### [SEVERITY: CRITICAL] Finding 1: Del/Esc/X hotkeys are dead code — `typing` reads window focus, not text-edit focus
**File**: crates/csp-ui/src/vab.rs:1937 (and 2020)
**Problem**: The hotkey gate binds `typing` to `egui::InputState::focused`, which in egui 0.37 is
the bool "The native window has the keyboard focus (i.e. is receiving key presses)"
(egui-0.37.0/src/input_state/mod.rs:313-316) — NOT "a text field wants the input". It is `true`
during all normal play, so the `if typing { return; }` gate swallows the entire hotkey block on
every frame the player could ever press a key. The intended check exists on `Context`:
`ctx.text_edit_focused()` (egui-0.37.0/src/context.rs:3490) — the app layer uses exactly that
(csp-app/src/lib.rs:2269), and its own comment there documents this precise trap.
**Evidence**:
```rust
let (pos, pressed, primary_down, released, deciding, secondary, del, esc, sym_key, typing) =
    ctx.input(|i| { ( ..., i.focused, ) });
...
// Hotkeys — gated on no text field wanting the input so a rename
// or a save-slot edit never eats them.
if typing {
    return;
}
if del && let Some(id) = vab.selected() { ... VabAction::Remove(id) ... }
if esc { ... VabAction::CancelPlacement / Select(None) ... }
if sym_key { ... VabAction::SetSymmetry(next) ... }
```
**Impact**: Every advertised Esc affordance is a lie. The palette banner says "Esc or
right-click puts it back", the hint bar says "(Esc/right-click puts it back)", the grab notice
says "(Esc puts it back)", the hint bar says "Del/right-click: remove" — pressing Esc or Del
does nothing in the VAB. Worse: Esc is the default `OpenSettings` binding (settings.rs:543),
and the app dispatch is only gated on `text_edit_focused`, so pressing Esc while carrying a
part POPS OPEN THE SETTINGS MODAL instead of cancelling — the exact "action does the wrong
thing" trap. X (symmetry cycle) never fires (the ×1/×2/×4 buttons still work, which is why
this hides from casual testing).
**Suggestion**: Replace the tuple element with `ctx.text_edit_focused()` (call it outside the
`ctx.input` closure): `let typing = ctx.text_edit_focused();` … `if typing { return; }`. That
keeps the rename/save-slot guard and un-deletes the three hotkey branches. A unit test
simulating `Context` with a focused TextEdit vs without would pin it.

### [SEVERITY: HIGH] Finding 2: Pointer pass treats top bar / tutorial / help / settings / crash dialog as 3D viewport — clicks and right-clicks fall through to destructive ops
**File**: crates/csp-ui/src/vab.rs:1944 (viewport test), 1954-1981 (press/drag-lift), 1986-2016 (release-drop, right-click remove)
**Problem**: `over_viewport` masks only the three rects `vab_editor` itself creates
(palette, inspector, action bar). In VAB mode the egui pass also draws the top bar
(csp-view/src/lib.rs:344), the tutorial checklist — **on by default**, anchored CENTER_TOP
(tutorial.rs:287-291), directly over the craft — the help card, the settings window, and the
crash dialog. None are in the mask, and the pass reads raw pointer events
(`i.pointer.button_pressed` / `secondary_clicked`), which fire regardless of which widget
consumed them (verified: egui derives them from `PointerEvent`s, not from widget consumption).
The comment says geometric masking was chosen over `is_pointer_over_area` because of drag
misreporting — but then only 3 of the ~7 interactable windows were masked.
**Evidence**:
```rust
let over_viewport = pos
    .is_some_and(|p| ctx.content_rect().contains(p) && !panels.iter().any(|r| r.contains(p)));
...
if secondary && over_viewport {
    if vab.carried().is_some() {
        let _ = vab.apply(VabAction::CancelPlacement);
    } else if let Some(hit) = vab.hover() {
        let _ = vab.apply(VabAction::Remove(hit.node));   // deletes hovered subtree
    }
}
```
(and `if let Some(node) = vab.move_node() { … else if over_viewport { Drop } }` on release;
`press_node` set from hover on any press, then `Grab` once `is_decidedly_dragging()`.)
**Impact**: Concrete player-visible failures, all reproducible with the default-on tutorial
card covering the build area: (1) right-click anywhere on the tutorial card, the top bar, or
the settings window **deletes the craft part behind the UI plus its whole subtree** — no
confirmation, no undo; (2) a click on any of their buttons that turns into a small drag lifts
the part behind the window onto the cursor (surprise `Grab`); (3) releasing a palette drag or
a moved part while over those windows **commits the attach** at the hover target behind them
(e.g. clicking the top bar's "Tutorial" button while moving a part drops the part onto
whatever the ray behind the button hit).
**Suggestion**: Make the mask complete. Cheapest correct fix: gate the *press/click* semantics
(`press_node` set, `secondary` remove, `orbit_block`) on `ctx.is_pointer_over_area(pos) ==
false`, and keep the geometric panels mask only for the release/drop branches where egui's
area hit-test is unreliable mid-drag; or have every host (top_bar, m10_panels, help_panel)
return its window rect and pass them all into `vab_pointer_input` alongside the editor panels.

### [SEVERITY: MEDIUM] Finding 3: Carry-state banner and ghost-refusal reason paint behind the tutorial card (which is on by default)
**File**: crates/csp-ui/src/vab.rs:3161,3188,3199 (banner/status positions); crates/csp-ui/src/tutorial.rs:287-291 (tutorial anchor)
**Problem**: In VAB the tutorial overlay anchors CENTER_TOP (+30px) and is ~290px wide /
~250px tall — exactly the band where the hint bar paints the "carrying <part> — click a craft
part to place it" banner (`rect.top() + 46.0`, Order::Background, non-interactable) and the
ghost refusal reason (`rect.top() + 64.0`). A normal window (Order::Middle) paints above a
Background layer, so with the tutorial open the banner and the "why the ghost is red" line are
invisible. The code even deliberates about banner-vs-problem-line overlap ("A carried part is
the banner state — it outranks the problem line") but never accounts for the tutorial card.
**Evidence**:
```rust
// vab.rs — banner (Background order):
egui::pos2(rect.center().x, rect.top() + 46.0),
// tutorial.rs — the window that covers it in VAB:
let anchor = if in_vab { (egui::Align2::CENTER_TOP, egui::vec2(0.0, 30.0)) } else { ... };
```
**Impact**: A new session starts with the tutorial active, so every new player's first VAB
experience has the carry feedback (and the planner's refusal reason that explains a red ghost)
hidden behind the checklist card. They pick up a part, the ghost goes red over a bad spot, and
the explanation is painted under a window.
**Suggestion**: Offset the banner/status lines below the tutorial band while it is active
(pass `tutorial_active` into `vab_hint_bar` and drop the y anchor to ~300), or move the
tutorial card out of the center column (e.g. LEFT_BOTTOM above the action bar, or BELOW_TOP
beside the mode bar).

### [SEVERITY: MEDIUM] Finding 4: Esc is default-bound to "open settings" — colliding with the universal cancel verb
**File**: crates/csp-ui/src/settings.rs:543
**Problem**: `(KeyAction::OpenSettings, b(&["escape"]))` makes Esc a mode-agnostic settings
toggle in every view, while the VAB simultaneously promises Esc as cancel-placement/deselect
(vab.rs banner, hint bar, `CancelPlacement` doc) and egui itself uses Esc to close combo
popups. The two consumers never coordinate: the app dispatch fires on any non-text-edit Esc.
**Evidence**:
```rust
(KeyAction::OpenSettings, b(&["escape"])),
```
**Impact**: Even after Finding 1 is fixed, a player pressing Esc to put back a carried part
will cancel *and* open the Settings window in the same keypress. Players habitually Esc to
back out of anything; here it throws a modal in their face mid-build.
**Suggestion**: Default `OpenSettings` to a dedicated non-modal key (`f2` or `key-o`), or route
Esc contextually in the app dispatch (if a carry is armed / a popup is open, let the UI consume
it and skip `OpenSettings`). Keep `escape` as a rebindable option for players who want it.

### [SEVERITY: MEDIUM] Finding 5: Tutorial "reach space" coaching line hardcodes the target the predicate does not enforce
**File**: crates/csp-ui/src/tutorial.rs:99 (hint) vs 157-163 (`space_alt`)
**Problem**: The step completes on the data-derived `space_alt(i)` (live `atmosphere_top_m`,
fallback 70 km), but the hint text is a hardcoded string naming "(70 km on Terrin)". The two
can diverge: another home body in the data, an atmosphere depth rebalance, or the airless
fallback case.
**Evidence**:
```rust
hint: "Keep burning until you clear the atmosphere (70 km on Terrin).",
...
fn space_alt(i: &TutorialInput) -> f64 {
    if i.atmosphere_top_m > 0.0 { i.atmosphere_top_m } else { SPACE_FALLBACK_ALT_M }
}
```
**Impact**: The tutorial silently stops completing at the altitude the coach line names (or
completes at one it never named) — the classic "checklist disagrees with the coach" trap; the
player climbs to the printed number and the step stays open with no explanation.
**Suggestion**: Make the hint dynamic: either format it at draw time from a staged
`atmosphere_top_m` (extend `TutorialStep::hint_for` with the input, like `{key}` substitution)
or drop the number from the static string ("clear the atmosphere — watch the altimeter").

### [SEVERITY: LOW] Finding 6: Staging toolbar "+ stage" is an invisible no-op and "− stage" only touches the topmost stage
**File**: crates/csp-ui/src/vab.rs:2714-2731
**Problem**: "+ stage" calls `InsertStage(top + 1)`, which shifts nothing (no part has stage ≥
top+1) and creates an empty stage the panel never lists (it renders only populated stages from
the analysis) — the sole feedback is the notice line. "− stage" removes only the highest
populated stage; there is no way to delete an arbitrary stage or stage 0, and no per-stage row
affordance at all.
**Evidence**:
```rust
if ui.small_button("+ stage").clicked() {
    let top = vab.session().populated_stages().last().copied().unwrap_or(0);
    let _ = vab.apply(VabAction::InsertStage(top.saturating_add(1)));
}
if ui.small_button("− stage").clicked()
    && let Some(top) = vab.session().populated_stages().last().copied()
{
    let _ = vab.apply(VabAction::RemoveStage(top));
}
```
**Impact**: Player clicks "+ stage", the staging list does not change (the new stage has no
parts) and it reads as a broken button; reassigning a part into the new stage is only
discoverable via the inspector's per-part stage DragValue.
**Suggestion**: Insert *below* the top populated stage (`InsertStage(top)` opens a real gap
that visibly renumbers rows), or list empty stages as ghost rows with a "move parts here" hint;
add per-row remove on each stage header.

### [SEVERITY: LOW] Finding 7: Rebinding an action silently destroys its other bindings
**File**: crates/csp-ui/src/settings.rs:595-605
**Problem**: `Keymap::bind` replaces the action's whole binding list with the single new key
(`self.bindings.insert(action, vec![key])`). Defaults ship two keys for the pans
(`key-w` + `arrow-up`, etc.), so rebinding PanUp to `key-n` silently throws away arrow-key
panning; nothing in the panel warns that a second binding existed or was lost.
**Evidence**:
```rust
self.bindings.insert(action, vec![key]);
```
**Impact**: A player who rebinds one camera key loses keyboard-only camera control on the
arrows with no feedback — a discoverability dead end for keyboard-only users.
**Suggestion**: Append instead of replace when the action has multiple bindings (evicting the
key from other actions as today), or at least surface "replaced N existing binding(s)" in
`SettingsPanelState::notice`.

Follow-ups (out of scope, one line): no undo/redo anywhere in the editor — subtree removals
(vab.rs `remove_node`) and reroot are unrecoverable; symmetry copies are placed-only (no
re-edit/delete of a symmetry group); the stage DragValue caps at 64 while the session accepts
999 (session.rs:182); map hint bar prints camera distance in scientific notation (`{:.3e}`).

## Verdict

The model layer is genuinely good: attach planning, readout math, navball projection, settings
schema and the tutorial's predicate-driven progress are correct, tested, and honest — the
"sessions + notices everywhere" discipline shows. But the interaction layer betrays it: the
advertised Esc/Del/X keys are provably dead (window-focus bool used as text-edit focus), Esc
opens a settings modal instead of cancelling, and raw pointer events fall through the
default-on tutorial card into subtree deletes and surprise drops. For a build editor whose
core loop is pick → carry → place → cancel, that is two broken verbs and one destructive
click-through in the primary flow; fix Findings 1-2 before any player touches the VAB again.
Not garbage — well-engineered bones with two load-bearing input bugs and a feedback-occlusion
problem exactly where new players look.
