# Verified Report 14 — csp-ui UX review (vab.rs, lib.rs, settings.rs, navball.rs, tutorial.rs)

Verified 2026-10-25 against the current tree (all cited line numbers match exactly; vab.rs
last modified 12:17, report written 13:31 — the report already reviewed the current state;
`git status crates/csp-ui` clean, so no concurrent-edit drift). egui claims checked against
vendored egui-0.37.0 source in ~/.cargo/registry.

### Finding 1: Del/Esc/X hotkeys are dead code — `typing` reads window focus, not text-edit focus — CONFIRMED
**Original**: The hotkey gate binds `typing` to `egui::InputState::focused` (native window focus, true during all normal play), so `if typing { return; }` unconditionally swallows the Del/Esc/X hotkey block; the correct API is `ctx.text_edit_focused()`.
**Verification**:
- crates/csp-ui/src/vab.rs:1937 — tuple element is `i.focused`, read inside `ctx.input`; vab.rs:2020-2022 — `if typing { return; }` precedes the Del (2023), Esc (2026), X (2033) branches. The gate's own comment (2018-2019) says "gated on no text field wanting the input" — intent confirmed, implementation wrong.
- egui-0.37.0/src/input_state/mod.rs:313-316 — `pub focused: bool` documented as "The native window has the keyboard focus (i.e. is receiving key presses). False when the user alt-tabs away" — i.e. true during all normal play, not a text-edit indicator.
- egui-0.37.0/src/context.rs:3490-3496 — `text_edit_focused()` exists, checks focused widget is a TextEdit.
- csp-app/src/lib.rs:2269-2273 — the app's key dispatch uses exactly `text_edit_focused()`, with a comment (2249-2267) documenting this precise trap ("dead-keys the entire board").
- Advertised affordances that lie: vab.rs:1039 ("Esc puts it back"), 2089 ("Put back (Esc)"), 2452, 3135 ("Del/right-click: remove"), 3165, 3178. The ×1/×2/×4 buttons (vab.rs:2055+) bypass the gate, confirming why it hides from casual testing.
- Esc-opens-settings interplay confirmed: settings.rs:543 binds OpenSettings to escape; csp-app/src/lib.rs:2054-2063 dispatches it with no mode/carry gate; no other Escape handler exists in the VAB path (only settings_io token mappings and a unit test). So Esc while carrying pops the settings modal; CancelPlacement never fires.

### Finding 2: Pointer pass treats top bar / tutorial / help / settings / crash dialog as 3D viewport — clicks and right-clicks fall through to destructive ops — CONFIRMED
**Original**: `over_viewport` masks only the three rects `vab_editor` creates; the top bar, default-on tutorial card, help card, settings window, and crash dialog are unmasked, and raw pointer events fire regardless of widget consumption — so right-clicks on them delete the craft part behind the UI (plus subtree), drags lift parts, and releases commit drops.
**Verification**:
- crates/csp-ui/src/vab.rs:1896-1899 — `panels` contains exactly palette/inspector/action_bar rects; vab.rs:1944-1945 — `over_viewport` = content_rect contains pos && no masked panel contains it. Nothing else is masked.
- Windows drawn in the same egui pass in VAB mode, none masked: top_bar (csp-view/src/lib.rs:343, egui Window anchored LEFT_TOP full of buttons — vab.rs:1815-1820), help_panel (lib.rs:354-360), and m10_panels (lib.rs:438-446) drawing the settings window (when open), crash_dialog (csp-ui/src/lib.rs:140-143, CENTER_CENTER — dead center over the craft), and the tutorial overlay — `crates/csp-view/src/lib.rs:683-687` draws it while `tutorial.active`, which defaults to `true` (tutorial.rs:192-205), anchored CENTER_TOP +30px with width 290 (tutorial.rs:287-296) — directly over the build area.
- Raw-pointer claim verified in egui 0.37: `button_pressed`/`button_released`/`secondary_clicked` are derived from `pointer_events` (`!down -> down` transitions) — input_state/mod.rs:1429-1436, 1524-1527. Pointer input has no consumption path in InputState (unlike `consume_key`), so widget handling is invisible to these reads.
- Destructive chain verified end-to-end: pointer over an unmasked window → `over_viewport` true → `set_cursor_px(Some)` (vab.rs:1946-1950) → csp-view casts the hover ray at that screen position (csp-view/src/vab.rs:411-414, whose own comment names "right-click remove") → `vab.hover()` = the part behind the UI → vab.rs:2010-2016 `secondary && over_viewport` → `Remove(hit.node)`; `remove_node` (vab.rs:934-949) deletes node + whole subtree with no confirmation. Press/drag-lift: vab.rs:1954-1981 sets `press_node` from hover on any press, `Grab` on `is_decidedly_dragging()` (frame-global). Release-drop: vab.rs:1987-2004 commits `Drop` when `over_viewport` — e.g. over the top bar.
- Nuance on the suggested fix: `ctx.is_pointer_over_area(pos)` does NOT exist in egui 0.37 (grep of vendored source: only `is_pointer_over_egui`, `egui_wants_pointer_input`, `layer_id_at(pos)`). The vab.rs:1942 comment cites the same nonexistent API. The report's alternative (hosts return their window rects into `vab_pointer_input`) is the fix that compiles; a layer check would be `layer_id_at(pos)` vs the background layer. Finding stands; fix wording needs that adjustment.

### Finding 3: Carry-state banner and ghost-refusal reason paint behind the default-on tutorial card — CONFIRMED
**Original**: The tutorial card anchors CENTER_TOP (+30px) in VAB, exactly over the hint bar's banner (top+46) and ghost-status (top+64) lines; the banner Area paints at Order::Background and the tutorial Window at Order::Middle, so the card covers the feedback.
**Verification**:
- crates/csp-ui/src/vab.rs:3122-3124 — hint bar Area is `order(egui::Order::Background)`; banner at `rect.center().x, rect.top() + 46.0` (3161, 3174, 3188), ghost status at `rect.top() + 64.0` (3199).
- tutorial.rs:287-296 — VAB anchor `CENTER_TOP, (0.0, 30.0)`, `default_width(290.0)`; card spans y >= 30 at screen center, so both y=46 and y=64 sit inside it (title bar + first rows), same x-center. Default `active: true` (tutorial.rs:197).
- Paint order verified: epaint Order docs — Background is "Painted behind all floating windows"; egui Window/Area default order is Middle (egui-0.37.0/src/containers/area.rs:146), which paints above Background regardless of submission order. Confirmed both axes of overlap and the z-order.
- The deliberation comment exists (vab.rs:3154-3157: "A carried part is the banner state — it outranks the problem line") with no tutorial consideration, as reported.

### Finding 4: Esc is default-bound to "open settings" — colliding with the universal cancel verb — CONFIRMED
**Original**: `(KeyAction::OpenSettings, b(&["escape"]))` makes Esc a mode-agnostic settings toggle while the VAB promises Esc as cancel; both consumers fire on the same keypress.
**Verification**:
- crates/csp-ui/src/settings.rs:543 — exact line confirmed.
- csp-app/src/lib.rs:2268-2273 — keyboard dispatch fires on any press where `!text_edit_focused` (egui consumption deliberately ignored per the preceding comment); lib.rs:2054-2063 — OpenSettings toggles the settings panel with no mode, popup, or VAB-carry condition. The egui-side VAB handler and the winit-side app dispatch are independent layers with no shared consumption for Esc, so once Finding 1 is fixed, one Esc press both cancels the carry and opens the settings modal.
- Third consumer noted: the rebind capture also treats Esc as "cancel capture" (lib.rs:1463-1466), intercepted pre-egui — consistent with the collision claim. egui combo popups close on Esc additionally.

### Finding 5: Tutorial "reach space" coaching line hardcodes the target the predicate does not enforce — CONFIRMED (latent with stock data)
**Original**: Step completes on data-derived `space_alt(i)` (live `atmosphere_top_m`, fallback 70 km) but the hint text hardcodes "(70 km on Terrin)"; the two can diverge.
**Verification**:
- crates/csp-ui/src/tutorial.rs:99 — static hint string; tutorial.rs:157-163 — `space_alt` prefers `i.atmosphere_top_m` when > 0, else `SPACE_FALLBACK_ALT_M` (70 km).
- csp-view/src/tutorial.rs:110 — `atmosphere_top_m` comes from the live `FlightEnv.atm_top_m`; shipped Terrin data has `top_m: 70000.0` (data/systems/home/terrin/body.ron:35), so hint and predicate currently agree — the bug is latent, not live: it fires on any atmosphere rebalance, a different home body, or the airless-home fallback (where the number still matches the fallback but "on Terrin"/"clear the atmosphere" are both wrong). The mechanics of the claim are exactly as stated; severity MEDIUM is fair given the project's advertised-affordance-honesty discipline, with the caveat that no stock-data player hits it today.

### Finding 6: Staging toolbar "+ stage" is an invisible no-op and "− stage" only touches the topmost stage — CONFIRMED
**Original**: "+ stage" inserts at top+1, shifting nothing and creating a stage the panel never lists; "− stage" only removes the highest populated stage; no per-row affordance.
**Verification**:
- crates/csp-ui/src/vab.rs:2714-2731 — code matches the quoted evidence verbatim.
- csp-phys/src/session.rs:1647-1672 — `insert_stage(at)` shifts only nodes with `stage >= at`; `at = top+1` where top is the highest populated stage (vab.rs:2716-2722) matches zero nodes → no visible change. Stage rows render from `analysis().stages` (vab.rs:2741-2749), which derive from populated stages only (session.rs:1617-1632), so an empty inserted stage is never listed. `remove_stage` (session.rs:1676-1684) folds stage `at` parts into `at-1`; the panel only ever passes the topmost populated stage, and stage 0 cannot be removed this way. No per-stage-row remove exists anywhere in `staging_section`.

### Finding 7: Rebinding an action silently destroys its other bindings — CONFIRMED
**Original**: `Keymap::bind` replaces the action's whole binding list with one key; defaults ship two keys for the pans, so a rebind silently loses the arrow bindings with no notice.
**Verification**:
- crates/csp-ui/src/settings.rs:603 — `self.bindings.insert(action, vec![key]);` (report cited 595-605, the `bind` body; exact insert at 603). Doc comment explicitly says "replacing the action's whole binding list" — intended behavior, so this is a UX/design gap, not an accident.
- settings.rs:553-556 — PanUp/PanDown/PanLeft/PanRight ship two bindings each (`key-w` + `arrow-up`, etc.).
- Feedback path checked: the rebind lands via `View::bind_key` (csp-view/src/lib.rs:1398-1406); the notice it writes reports only the cross-action eviction ("was {other}"), never the destroyed sibling bindings of the rebound action. Silent loss confirmed.

## Summary
7 confirmed, 0 dismissed, 0 fixed-by-concurrent-edit.
