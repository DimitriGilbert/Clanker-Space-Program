# Review Report 15: lib.rs back half (lines 1701-3409)

Reviewer: ruthless subagent, static review only (no cargo). Scope: crates/csp-app/src/lib.rs:1701-3409.

### [SEVERITY: MEDIUM] Finding 1: focus_lost clears 3 of 5 held-input states — map drag and click candidate survive a focus loss
**File**: crates/csp-app/src/lib.rs:1888
**Problem**: `focus_lost()` clears `captured_key`, `throttle_held`, and `attitude_held` — the three release-driven keyboard states — but not `map_dragging` or `lmb_press`, which are also cleared only by `MouseInput` release events (line 2285). Its own doc comment states the contract: "A focus loss can drop the releases that clear a captured key, held attitude keys, and the throttle axis — clear all three here"; mouse release can be dropped by exactly the same focus loss.
**Evidence**: `fn focus_lost(&mut self) { self.captured_key = None; self.throttle_held.clear(); ... }` — no `self.map_dragging = false;` / `self.lmb_press = None;`. `map_dragging` is set false only in the `MouseInput` arm.
**Impact**: Player left-drags to orbit the map/flight camera, then alt-tabs (or a compositor action like the GNOME overview steals the pointer — implicit grabs do not survive it; on X11 there is no grab at all). The `Focused(false)` clears keyboard state but `map_dragging` stays `true`; the button-up outside the window never arrives. On refocus, every `CursorMoved` orbits the camera with no button held (line 2290-2305) until the user clicks again — the camera appears possessed. A stale `lmb_press` can additionally fire a phantom VAB pick on the first post-refocus release.
**Suggestion**: In `focus_lost`, add `self.map_dragging = false; self.lmb_press = None;` alongside the existing clears (one-line fix, matches the function's own stated discipline).

### [SEVERITY: MEDIUM] Finding 2: throttle axis silently retargets to a new focus vessel with the old vessel's commanded value
**File**: crates/csp-app/src/lib.rs:1828
**Problem**: `throttle_axis_step` sends `SetThrottle { vessel: flight_focus() ... }` to whatever vessel is focused *now*, but `throttle_axis` is only rebased from `body.throttle` when a *fresh* hold starts (`throttle_press`, line 1739-1757, `len() == 1`). A key held through a focus change ramps the new vessel's throttle starting from the *previous* vessel's commanded axis value, with no rebase and no notice.
**Evidence**: `tick_throttle_ramp` (1793) checks only `read_only`/Vab-mode/blocker — no focus-identity tracking. Concrete path: hold Shift (default `ThrottleUp`, confirmed at settings.rs:543 region / test line 3399), press M (Map mode — the ramp keeps running there; only Vab clears it, line 1797), press the CycleFocus binding (dispatch line 2110) to another craft. Every subsequent frame enqueues `SetThrottle{new_vessel, old_axis + ramp}` on the player FIFO. The same happens in flight if the focused vessel is destroyed/staged away and focus falls back.
**Impact**: The newly focused craft's throttle snaps to a value the player never commanded for it and keeps ramping — a silent cross-vessel throttle bleed, logged as if legitimate in the §7.6 replay log.
**Suggestion**: Track the vessel id the hold started on (e.g. `throttle_axis_vessel: Option<VesselId>` set at press/rebase); in `tick_throttle_ramp`, if `flight_focus() != throttle_axis_vessel`, either drop the hold (with a flight_notice, matching the codebase's no-dead-input discipline) or rebase from the new body's `throttle` before continuing.

### [SEVERITY: LOW] Finding 3: text-edit gating swallows non-text control keys — Esc and mode toggles are dead while a TextEdit holds focus
**File**: crates/csp-app/src/lib.rs:2268
**Problem**: The press gate `if event.state == ElementState::Released || !typing { self.keyboard_event(&event); }` hides *every* key from the game while `text_edit_focused()`, but Esc produces no text in a TextEdit — the comment's own rule is "Only *text entry* justifies hiding a press from the game", and Esc is not text entry.
**Evidence**: With the craft-name field (VAB) or any TextEdit focused, pressing Esc (default `OpenSettings`, settings.rs:543) or M/F7 mode keys does nothing in-game and inserts nothing in the edit — a dead key with zero feedback. Recovery requires clicking outside the field first, which is undiscoverable.
**Impact**: "Keys randomly stop working" reports after the player clicks into a text field; interacts with the already-known Esc/OpenSettings collision.
**Suggestion**: Exempt non-character keys from the typing gate (e.g. `let typing_blocks = typing && matches!(event.logical_key, Key::Character(_))`), or at minimum exempt the settings/close verbs.

### [SEVERITY: LOW] Finding 4: load_craft_store silently skips unreadable/invalid .cspcraft files — no notice for a hand-placed file
**File**: crates/csp-app/src/lib.rs:2439
**Problem**: `rd.flatten()` and the `if let (Ok(bytes), ...)` pattern drop read errors, name-decode failures, and `validate_craft_name` refusals without any log line or notice. Everywhere else this codebase refuses loudly (even "click had no cursor position", line 1946); here a hand-placed file the player expects in the craft list just never appears.
**Evidence**: A `foo.cspcraft` with no read permission, a stem that fails `validate_craft_name` (e.g. `my..craft` — refused as "path traversal" though it is a legal unix name), or a non-UTF8 stem vanishes from `craft_store` with zero diagnostics.
**Impact**: Player drops a craft file into `crafts/`, it is absent from the load list, and nothing says why — a silent failure by the project's own standard.
**Suggestion**: Collect skipped names in `load_craft_store` and surface them through `view.flight_notice`/`set_settings_notice` at assemble time (the load-notes channel already exists), and log::warn per skipped file.

### [SEVERITY: NIT] Finding 5: bug_report_cli hashes the data set twice
**File**: crates/csp-app/src/lib.rs:2384
**Problem**: `csp_view::data_set_hash(&csp_view::data_root()).ok()` is evaluated at line 2384 (for the embedded save) and again at 2396 (for the fingerprint) — two full data-tree walks + hashes in one CLI run. Same pattern in `collect_bug_report` (front half). Compute once and reuse.
**Impact**: Trivial extra latency on a manual CLI path; correctness unaffected.

## Verdict (5 lines)

1. The back half is in good shape: the throttle ramp/press/release machinery, rebind capture, and text-edit gating are carefully reasoned and heavily documented.
2. The test suite (2514-3409) is substantive — real sim stepping, command-log assertions, tolerance-checked ramp math; no vacuous gates found.
3. Two real MEDIUM input bugs: stuck camera drag after focus loss, and cross-vessel throttle bleed when focus moves under a held key.
4. Two LOW honesty gaps (dead Esc while typing, silently skipped craft files) plus a double-hash nit — all cheap fixes.
5. APPROVE — nothing here blocks merge; the two MEDIUMs should be fixed in this PR since both are one-to-few-line changes in this exact range.

Out-of-scope follow-ups:
- `apply_attitude` (lib.rs:1621, front half): held attitude keys across a focus change leave the OLD vessel's commanded angular rate running and issue nothing to the new focus until the next press/release — same family as Finding 2.
- `collect_bug_report` (lib.rs:~585, front half): same double `data_set_hash` walk as Finding 5.
