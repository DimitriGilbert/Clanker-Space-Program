# Review Report — P01: Keymap, physical tokens & AZERTY

**Scope:** `crates/csp-ui/src/settings.rs`, `crates/csp-app/src/settings_io.rs`,
`crates/csp-app/src/lib.rs` (input plumbing), `crates/csp-view/src/lib.rs`
(session-mirror keymap plumbing).

**Verdict:** The core design is sound — physical-first resolution is symmetric on
press and release, the rebind-capture swallow-until-release cannot wedge (it
matches on `PhysicalKey`, immune to modifier-shifted logical tokens), and an
AZERTY `Z` press really does resolve `key-w` → `PanUp` end-to-end. Findings are
in the seams: two HUD hint call sites bypass the observed-label map (defeating
the AZERTY fix on the most prominent hints), the char→`key-*` migration can't
distinguish legacy bindings from UI-captured numpad/ISO keys, and the release
path re-resolves the *token* rather than remembering the armed *action* —
the same modifier-drift class the capture path fixed, left open for held verbs
bound to non-positional characters.

---

## Findings

### 1. MEDIUM — Two flight-HUD hint sites bypass the observed-label map; AZERTY users get permanently wrong key names on the most prominent hints

- **Where:** `crates/csp-ui/src/lib.rs:533-535` (no-vessel hint row) and
  `crates/csp-ui/src/lib.rs:555` (`"ON PAD — [{}] to launch"`).
- **Problem:** Both call `input.keys.hint(action)` — which is
  `hint_labeled(action, &BTreeMap::new())` (`crates/csp-ui/src/settings.rs:646-648`)
  — i.e. they pass an *empty* observed map, so every `key-*` token prints the
  QWERTY `physical_fallback` glyph forever. Every neighboring hint site
  (L324-325, L647-668, `vab.rs` L874/881/1728, tutorial `hint_for`) uses
  `hint_labeled`/`first_binding_label` with the real `input.key_labels`.
- **Evidence:** `Keymap::hint` (settings.rs:646) calls `hint_labeled` with
  `&BTreeMap::new()`; `display_label` (settings.rs:498-503) then falls back to
  `physical_fallback`, which prints the QWERTY name (`"key-z"` → `"Z"`).
  `FlightHudInput.key_labels` (csp-ui/src/lib.rs:447) is populated with
  `&session.key_labels` (csp-view/src/lib.rs:417) — the data is right there.
- **Impact:** On AZERTY the line `"ON PAD — [Z] to launch"` instructs the user
  to press the **Z-labeled** key — which on AZERTY sits at the physical `KeyW`
  position and fires `key-w` → `PanUp` (yielding "PARKED — ATTITUDE LOCKED"),
  not `ThrottleFull` (`key-z`, the W-labeled key). Same for `[M] map` /
  `[V] VAB` in the no-vessel row (`key-m` on AZERTY is the `,`/`?` key). The
  label-recording machinery (`event_label` → `set_key_label` → `key_labels`)
  can never fix these two sites — `hint()` ignores observed labels by
  construction, so the hints stay wrong for the whole session even after the
  keys have been pressed. These are exactly the hints a first-run user needs.
- **Suggestion:** Change both sites to `hint_labeled(..., input.key_labels)` —
  a two-line fix; `FlightHudInput` already carries the map.

### 2. MEDIUM — `migrate_char_bindings` re-binds keys that have no physical token; a UI-captured numpad/ISO binding silently moves on next launch

- **Where:** `crates/csp-app/src/settings_io.rs:325` (`keycode_token` `_ => return None`),
  `settings_io.rs:334-338` (`event_binding` logical fallback),
  `settings_io.rs:384-397` (`migrate_char_bindings`),
  `crates/csp-app/src/lib.rs:350-362` (unconditional migrate in `assemble`).
- **Problem:** `keycode_token` has no tokens for `KeyCode::Numpad*`,
  `IntlBackslash`, `IntlRo`, `NumpadAdd`/`Subtract`/etc. — they all hit the
  `_ => None` arm. `event_binding` then falls back to `binding_for(logical)`,
  so a rebind capture on Numpad-5 (NumLock on) records the *character* `"5"`.
  On the next launch, `assemble` unconditionally runs
  `migrate_char_bindings`, which converts `"5"` → `"key-5"` — the top-row digit
  position. After migration, pressing Numpad-5 resolves physical `None` →
  logical `"5"` → `action_for("5")` → `None`: **the captured key is dead and
  the binding moved to a key the user never pressed.**
- **Evidence:** `char_to_physical_token` (L346-370) maps every `[a-z0-9]` char
  and the common punctuation to `key-*`; `migrate_char_bindings` applies it to
  every binding in every loaded file with no provenance check — a `"5"` written
  by *this* build's capture path is indistinguishable from a `"5"` written by a
  pre-`key-*` build.
- **Impact:** Any user who rebinds to a numpad digit, numpad operator
  (`+`/`-`/`*`/`/`/`.`), or ISO `<>` key gets a working binding for one
  session that silently re-points to a different physical key on restart —
  exactly the "moved binding" failure mode migration was built to *fix*, now
  *caused* by it. Secondary variant: with NumLock off, numpad keys report
  named logicals (`ArrowDown`, `PageDown`, …) — capturing one binds the
  *shared* token (`"arrow-down"`) and `Keymap::bind` evicts it from `PanDown`
  etc., silently stealing the arrow-key default.
- **Suggestion:** Give the unmappable physical codes their own tokens
  (`"numpad-5"`, `"key-intl-backslash"` or similar — extend
  `PHYSICAL_KEY_TOKENS` + `keycode_token`), or stamp migrated files
  (e.g. a `keymap_tokens: positional` field / schema bump) so the migration
  only runs on pre-`key-*` files. Failing that, restrict migration to the
  characters a legacy file could plausibly carry for the default cluster.

### 3. MEDIUM — Release path re-resolves the *token* under current modifiers; held verbs bound to modifier-dependent characters can wedge

- **Where:** `crates/csp-app/src/lib.rs:1383-1390` (`action_of`),
  `lib.rs:1680-1689` (release arm of `keyboard_event`).
- **Problem:** The release resolves `token → action` *at release time*. For a
  binding on a logical character whose glyph is modifier-dependent, changing
  modifier state between press and release makes the release resolve a
  different token — or none. The held set (`attitude_held`/`throttle_held`)
  stores the armed *action*; if the release resolves `None` or a different
  action, the entry is never removed → the verb stays held until `focus_lost`.
- **Evidence:** The capture path fixed this exact class deliberately — the
  `captured_key` doc (lib.rs:175-179) and `intercept_rebind_key` (L1707-1712)
  match on `physical_key` precisely *because* "a Shift+1 capture records `!`
  but, once Shift is already up, releases as `1` — a token match would never
  clear." The release path applies the un-fixed version of the same logic.
  Concrete repro: bind `"<"` (ISO `IntlBackslash` unshifted glyph — survives
  migration since `char_to_physical_token` returns `None` for it, and *is*
  UI-capturable since `keycode_token(IntlBackslash)` → `None` → logical
  fallback) to `PanUp`. Press the key → `PanUp` held. Hold Shift (ThrottleUp
  by default!), release the `<` key → release logical is `">"` →
  `action_for(">")` → `None` → `attitude_release` never runs → the craft
  pitches forever. Same for `"é"`/`"!"` file-bound chars on AZERTY (holding an
  AZERTY top-row key while reaching for Shift-throttle is a natural combo).
- **Impact:** Stuck rotation (`attitude_held` leak → the last commanded
  `SetAttitude` rate persists) or throttle ramped to the rail and held
  (`throttle_held` leak). Recovers on window focus loss or a same-glyph
  press/release pair, but is silent and scary mid-flight.
- **Suggestion:** Track held verbs by physical key, not by resolved action —
  e.g. `held_keys: BTreeMap<PhysicalKey, KeyAction>` populated in `key_press`
  when an attitude/throttle-axis action dispatches, drained on release by
  physical-key match. That also makes release immune to mid-hold keymap
  changes the `drop_rebound_holds` heuristics currently paper over.

### 4. LOW — `event_label` is last-writer-wins and records shifted/consumed/release glyphs; the captured press never records a label

- **Where:** `crates/csp-app/src/settings_io.rs:416-428` (`event_label`),
  `crates/csp-app/src/lib.rs:1664-1670` (`note_key_label`),
  `lib.rs:1882-1890` (intercept runs before `note_key_label`).
- **Problem:** Every `KeyboardInput` event for a `key-*` token with a
  single-char logical overwrites the label: (a) a `Shift+Digit1` anywhere —
  including typing `!` into a focused text field, since `note_key_label` runs
  before egui's `consumed` check — relabels `key-1` as `"!"` until an
  unshifted press; (b) release events record too (usually same glyph, but a
  modifier-released-early release records a different one); (c) the rebind
  capture press returns before `note_key_label`, so a just-bound key shows the
  QWERTY fallback until its next normal press — an AZERTY user who binds the
  `z`-position sees `"W"` in the bindings column until they press it.
- **Impact:** Hint labels can transiently show the shifted glyph; purely
  cosmetic, self-heals on the next unshifted press of that key.
- **Suggestion:** Prefer not overwriting a recorded label, or record only
  press events and prefer the glyph that matches the token's own name
  (`key-1` ↔ `"1"`) when both have been seen. Cheap improvement: also call
  `set_key_label` from the capture arm so the just-bound key is labeled
  immediately.

### 5. LOW — Migration collision resolution claims by `KeyAction::ALL` order, not by provenance

- **Where:** `crates/csp-app/src/lib.rs:350-352` (migrate → `sanitize`),
  `crates/csp-ui/src/settings.rs:617-640` (`sanitize` first-claimant order).
- **Problem:** When migration rewrites `"w"` → `"key-w"` under an action that
  sits earlier in `KeyAction::ALL` than a deliberately hand-bound `"key-w"`
  under a later action, `sanitize` keeps the *migrated legacy* claim and drops
  the deliberate positional one — claim order is action order, with no notion
  of which token was "physical to begin with."
- **Impact:** Requires a hand-edited file mixing a legacy char and a `key-*`
  on the same position — narrow, and the warn/first-launch notices at least
  disclose that migration and drops happened. The loser ends silently
  unbound.
- **Suggestion:** If worth fixing, make `migrate_char_bindings` skip (leave as
  char → `layout_dependent_binding_count` reports it) when the target `key-*`
  token already exists anywhere in the map, rather than colliding into
  `sanitize`'s first-come order.

### 6. LOW — `Keymap::bind` replaces the action's whole binding list; notice reports the raw token

- **Where:** `crates/csp-ui/src/settings.rs:590-600` (`bind`),
  `crates/csp-view/src/lib.rs:1307-1315` (`bind_key` notice).
- **Problem:** `bind` does `self.bindings.insert(action, vec![key])` — every
  rebind silently drops the action's *other* defaults. Rebinding `PanUp`
  deletes the `"arrow-up"` alternative permanently (defaults are only
  recoverable via Reset-to-defaults, which resets *everything*). The eviction
  notice also prints `{key}` via `Display` — the raw token `"key-w"` — while
  the bindings column right beside it prints the observed glyph (`"z"`); on a
  non-QWERTY machine the notice names a key the user can't see on their
  keyboard.
- **Impact:** Functional but surprising UX: dual-bound defaults
  (Pan*/ToggleHelp) lose their alternates on first rebind. The grid does
  reflect the result next frame, so it's disclosed-but-undocumented.
- **Suggestion:** If "replace all" is the intent, note the dropped alternates
  in the notice text; use `display_label(&key_labels)` for the notice so the
  confirmation names the key the user pressed.

### 7. LOW — Two startup keymap notices write the same slot; second wins

- **Where:** `crates/csp-app/src/lib.rs:356-370`.
- **Problem:** `set_settings_notice` at L359-361 (migration summary) is
  overwritten by `set_settings_notice` at L368-370 (layout-dependent count)
  when both fire — the panel only ever shows the second. The migration summary
  still lands on the flight-notice channel and the log, so it isn't fully
  lost.
- **Impact:** Minor — the panel understates what happened on the one launch
  where both conditions hold.
- **Suggestion:** Concatenate into a single notice.

### 8. LOW — Held attitude verbs are not cleared on mode switch (throttle axis is)

- **Where:** `crates/csp-app/src/lib.rs:1566-1570` (`tick_throttle_ramp`
  clears `throttle_held` on `ViewMode::Vab`) vs `lib.rs:1397-1407`,
  `1838-1845` — `attitude_held` has no mode-switch clearing.
- **Problem:** Hold `W` (PanUp held, rate commanded), press `V` mid-hold:
  entering the VAB clears the throttle hold but `PanUp` stays in
  `attitude_held` and the last `SetAttitude` rate keeps integrating — the
  craft keeps rotating through the VAB visit until `W` is released (the
  release still clears it, and `W`'s repeats meanwhile pan the VAB camera).
- **Impact:** Borderline — semantically "held stays held," but inconsistent
  with the documented VAB suppression of the throttle axis and surprising for
  a mode that should be inert w.r.t. the flown vessel.
- **Suggestion:** Either clear `attitude_held` (with a zero-rate re-issue) in
  `set_mode`'s VAB branch like the throttle path, or document the asymmetry.

---

## Verified clean (focus items from the map)

- **Physical-first order, press/release symmetric** — `key_press`
  (lib.rs:1365-1370) and `action_of` (L1383-1390) use identical order:
  `keycode_token(physical)` → `action_for`, then `key_token(logical)` →
  `action_for`. For `key-*` tokens the resolution is modifier-independent, so
  press/release are always symmetric. The only asymmetry is finding 3's
  modifier-dependent *logical* tokens.
- **`intercept_rebind_key` cannot wedge on shifted tokens** —
  `captured_key` stores `PhysicalKey` (lib.rs:180) and the release match at
  L1712 compares physical identity, so a captured `key-1` released as `!`
  still matches; `focus_lost` (L1653) is the backstop; the swallowed release
  still runs `keyboard_event` (L1722) so held sets clear. Repeat presses while
  armed return early (key_press L1340-1343) and don't double-capture.
- **`bind`'s one-action-per-key eviction** — `bind` (settings.rs:590-600)
  evicts the key from every other action; in-session invariant holds
  (test `rebind_replaces_and_evicts` covers it). See finding 6 for the
  whole-list-replace caveat.
- **`migrate_char_bindings` + `sanitize` ordering** — load-time `sanitize`
  (settings_io.rs:94) runs before `assemble`'s migrate (lib.rs:350), which
  re-runs `sanitize` only when `migrated > 0` — the re-sanitize correctly
  absorbs collisions; claim-order caveat is finding 5.
- **`event_label` gate** — settings_io.rs:416-428 correctly restricts to
  `key-*` tokens and single-char glyphs (multi-char IME strings rejected);
  over-recording issues are finding 4.
- **AZERTY end-to-end** — pressing the Z-labeled key yields
  `PhysicalKey::Code(KeyW)` → `keycode_token` = `"key-w"` → `action_for` =
  `PanUp` → `attitude_press` in flight mode: **yes, it fires**. And
  `event_label` records `("key-w", "z")` so labeled hints show `z`. The
  remaining gap is purely the two unlabeled `hint()` call sites (finding 1).
- **Escape-cancel capture** (lib.rs:1346-1349) disarms without dispatching
  `OpenSettings`; panel X-close and `toggle_settings_panel` both disarm a
  stale `listening`, and `rebind_target` (csp-view/src/lib.rs:1293-1296) is
  open-gated — an armed-but-hidden panel cannot swallow keys.
- **Settings-file hygiene** — `load_settings_from` sanitizes on parse and
  warns on drops; `bind_key` writes only the staged map until Apply;
  `drop_rebound_holds` (lib.rs:1633-1646) correctly retains only held verbs
  whose binding list is unchanged; releases always reach `keyboard_event`
  regardless of egui `consumed` (lib.rs:1943-1946).
- **Nit (not a defect):** the `"space" => "space"` arm in `physical_fallback`
  (settings.rs:483) is unreachable — `"key-space"` is rejected by
  `KeyBinding::parse` (not in `PHYSICAL_KEY_TOKENS`), so no stored token can
  reach it.
