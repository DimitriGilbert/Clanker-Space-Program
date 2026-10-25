# Review — P14: Settings & session persistence

**Scope:** `crates/csp-ui/src/settings.rs`, `crates/csp-app/src/settings_io.rs`,
`crates/csp-app/src/lib.rs` (assemble/staging, `apply_live_settings`/`persist_settings`/
`apply_startup_difficulty`, `settings_apply` outbox, rebind capture, `drop_rebound_holds`),
`crates/csp-view/src/lib.rs` (`SettingsPanelRes`, stage/live keymap plumbing, `m10_panels`).

Diff surface confirmed: `git show HEAD~4` — the previous `KeyAction` had **14** variants;
this diff adds 6 (`ThrottleUp`, `ThrottleDown`, `Stage`, `RecoverControl`, `RollLeft`,
`RollRight`) — load-bearing for Finding 2.

---

## Findings

### F1 — MEDIUM: `graphics.quality` is a dead control

- **Where:** `crates/csp-ui/src/settings.rs:88-89` (field), `:748-762` (the combo box),
  `crates/csp-app/src/lib.rs:431-459` (`apply_live_settings` — the only live-apply site).
- **Problem:** The panel presents `quality: low / medium / high` as a working control
  and Apply reports "settings saved", but nothing consumes the value. `apply_live_settings`
  applies only `ui_scale` and `max_warp_multiplier`; `grep -rn "\.quality"` across the
  workspace finds zero readers outside the model, its tests, and the panel itself — no
  render crate references `GraphicsQuality` at all. The field's doc (`settings.rs:37-38`)
  claims it "selects the render tier the M10 render work consumes" — that consumer never
  landed.
- **Evidence:** `apply_live_settings` (lib.rs:431) contains no quality branch; the Apply
  e2e test (`settings_apply_persists_and_reaches_live_targets`, lib.rs:2436) pins
  `ui_scale`/`max_warp_multiplier`/`window_size` only.
- **Impact:** A player-visible control that lies — pick "High", get "settings saved",
  and absolutely nothing changes. This is the exact "action without effect or cue" class
  the rest of the playability pass was built to kill, on the settings surface.
- **Suggestion:** Wire it to a real render knob (terrain patch budget, transmittance LUT
  resolution, plume/dust toggles are the natural candidates), or until then mark the row
  in-panel as inert ("not yet active") and fix the doc comment. Don't leave a
  fully-functional-looking dead control.

### F2 — MEDIUM: forward-compat gaps in the keymap load — new verbs unbound for upgraders; unknown variants reset the whole file

- **Where:** `crates/csp-ui/src/settings.rs:515-521` (`Keymap` field serde),
  `:617-640` (`sanitize`), `crates/csp-app/src/settings_io.rs:87-110`
  (`load_settings_from`).
- **Problem (a) — upgraded files leave new actions dead:** `bindings` deserializes only
  the entries present; `sanitize` iterates `KeyAction::ALL` via `get_mut` and skips
  absent entries — it never fills them. A `settings.ron` written by the previous
  milestone contains all 14 then-known actions and **no** entries for `ThrottleUp`,
  `ThrottleDown`, `Stage`, `RecoverControl`, `RollLeft`, `RollRight` (verified: those
  six variants did not exist in `HEAD~4`). On the new build those six verbs have no
  binding at all — `action_for` finds nothing, the keys are dead, and the file stays
  that way on every subsequent Apply/close persist. Only Reset-to-defaults or a manual
  rebind revives them. `migrate_char_bindings` fixes char→position for bindings that
  *exist*; it does nothing for bindings that are *absent*.
- **Problem (b) — all-or-nothing parse:** any single deserialization failure in the
  file — an unknown `KeyAction` variant or `GraphicsQuality` value in a file written by
  a *newer* build (downgrade), a typo'd variant name, a malformed `bindings` value —
  fails the whole `ron::from_str::<Settings>` and silently discards **every** user
  setting with only `log::warn!` (settings_io.rs:103-109). The module doc's "unknown
  keys are ignored" rule (settings.rs:13-16) holds for struct *fields* — the
  `unknown_keys_and_future_fields_ignored` test (settings.rs:908) proves that — but not
  for map keys or enum variants, which hard-fail. Worse, the reset is invisible to the
  player: there is no in-app notice, so a corrupted keymap just means dead keys.
- **Impact:** For (a): upgrading users — everyone with an existing config dir — get the
  milestone's headline verbs (throttle axis, staging, recover-control, roll) unbound.
  For (b): a downgrade or a single hand-edit typo wipes all settings and says nothing
  in-app.
- **Suggestion:**
  - (a) After `sanitize`, for each action in `KeyAction::ALL` whose `bindings` entry is
    **absent** (present-but-empty stays deliberately unbound — that distinction is the
    user's "clear"), merge in `Keymap::default()`'s bindings for that action when the
    tokens are unclaimed. New verbs then ship working for upgraders.
  - (b) Surface the reset — `load_settings_from` already knows the path and the failure;
    return a flag/`Option<String>` so `assemble` can push `set_settings_notice` /
    `flight_notice` ("settings file could not be read — defaults in use"), the same
    channel the migration notice uses. The same applies to the `dropped > 0` sanitize
    warn — currently log-only while the player's keys go dead.

### F3 — LOW-MEDIUM: `NaN` literals in a hand-edited file bypass every clamp

- **Where:** `crates/csp-app/src/lib.rs:432-437` (`ui_scale` clamp → `set_zoom_factor`),
  `crates/csp-app/src/audio.rs:549-555` (`master_gain` clamp), parsed at
  `settings_io.rs:87`.
- **Problem:** RON 0.12 accepts `NaN`, `inf`, `NaNf32`, `NaNf64` literals
  (`ron-0.12.2/src/parse.rs:468-475`). `f32::clamp` returns `self` for `NaN` (both
  comparisons are false), so a file carrying `ui_scale: NaN` survives the apply-time
  clamp and lands `set_zoom_factor(NaN)` — egui stores it into `Options::zoom_factor`,
  `pixels_per_point` goes NaN, and every panel renders NaN geometry. The app keeps
  running with a completely unusable UI — and the settings panel that could fix it is
  part of what's broken, so recovery means hand-deleting the file. `master_volume: NaN`
  similarly produces NaN gain → NaN samples into the subprocess audio pipe.
  (`inf`/`-inf` do clamp correctly — only `NaN` slips through.)
- **Impact:** Hand-edited-file edge, but the failure mode is total UI loss with no
  in-app recovery path — and the "sanitize" contract (`Keymap::sanitize` +
  `serde(default)`) covers only the keymap; present-but-nonfinite floats get nothing.
- **Suggestion:** Finiteness checks at the cheapest sites: `load_settings_from`
  (`if !s.graphics.ui_scale.is_finite() { reset }`) or inside `apply_live_settings`
  (`let scale = if raw.is_finite() { raw.clamp(..) } else { 1.0 }`), and
  `master_gain` already clamps — add the same finite check there.

### F4 — LOW: a captured rebind key never records its glyph label

- **Where:** `crates/csp-app/src/lib.rs:1882-1890` (`intercept_rebind_key` early-returns
  before `note_key_label`), `:1664-1670` (`note_key_label`/`event_label`).
- **Problem:** `note_key_label` runs *after* the rebind intercept, so the capture press
  (and its swallowed repeats/release) never records the observed glyph for the `key-*`
  token it just bound. An AZERTY user who binds their `Z` key gets `key-w` — correct —
  but the panel row and hint bars then display the QWERTY fallback `"W"` (via
  `physical_fallback`, settings.rs:468-489) until the key is pressed once more in
  normal dispatch. The wrong-glyph window is exactly the scenario the label feature
  exists to fix.
- **Impact:** Cosmetic but misleading — "the panel shows the wrong letter right after
  I rebound", self-healing on next press.
- **Suggestion:** In `key_press`'s capture arm (lib.rs:1344-1356), feed
  `settings_io::event_label(event)` into `view.set_key_label` alongside `bind_key` —
  one line, and the freshly-bound row immediately prints the true glyph.

### F5 — LOW: the AZERTY migration maps glyphs to QWERTY positions — custom bindings relocate on non-QWERTY layouts

- **Where:** `crates/csp-app/src/settings_io.rs:346-370` (`char_to_physical_token`),
  `crates/csp-app/src/lib.rs:350-361` (call site + notice).
- **Problem:** `char_to_physical_token` translates a character to the `key-*` position
  *named for that glyph* — `"z"` → `key-z`. On a QWERTY-written old file that is the
  same physical key; on a file authored under AZERTY it is not — `"z"` was typed at the
  QWERTY-W position, but migrates to `key-z` (the AZERTY `w` key). For the shipped
  default cluster this relocation *is* the intent (canonical WASD positions), and the
  code documents it ("a silent remap is still a moved binding"), but the HUD/panel
  notice says only "N key binding(s) migrated to positional keys" — it doesn't name
  which bindings moved or that the physical key changed. Secondary wrinkle: digits
  migrate too — an AZERTY `"1"` (typed Shift+row-key) becomes `key-1`, which now fires
  unshifted; defensible, but a semantics change the notice doesn't mention.
- **Impact:** Low — disclosed, idempotent, and self-healing via rebind — but an AZERTY
  user's custom char bindings can land on a different physical key than they bound.
- **Suggestion:** Cheap improvement: name the moved bindings in the notice
  (`"z → key-z"` per binding, or at least "check your bindings" when
  `layout_dependent_binding_count`/`migrated` both fire). No format change needed.

### F6 — LOW: `schema_version` round-trips the file's value, not the writer's

- **Where:** `crates/csp-ui/src/settings.rs:32-33`, `crates/csp-app/src/settings_io.rs:122-130`.
- **Problem:** `schema_version` deserializes whatever the file says — a future file
  with `schema_version: 99` loads with 99 intact (the forward-compat test at
  settings.rs:909-921 asserts exactly this), and `save_settings_to` then writes
  `schema_version: 99` back. This build now stamps files with a version it isn't.
  Harmless today (no consumer reads it), but the field exists precisely to be honest
  about who wrote the file.
- **Suggestion:** `save_settings_to` should write `SETTINGS_SCHEMA_VERSION` (or load
  should normalize to `min(file, CURRENT)`), so the writer always stamps its own
  version — the convention the field's doc already states ("`SETTINGS_SCHEMA_VERSION`
  of the writer").

---

## Verified clean (focus items that check out)

- **Staged-vs-live discipline:** dispatch resolves `self.settings.keys` (live) at
  lib.rs:1365-1370/1384-1389; help card, HUD hints, tutorial, top bar all read
  `session.keys` — the live mirror written by `set_live_keymap` (view:1228), never the
  staged copy. `staged_settings()` has exactly one consumer: the Apply arm (lib.rs:993).
- **`window_size` app authority:** lib.rs:993-994 overwrites the stale staged value
  with the app-tracked live one; `Resized` feeds the live copy at lib.rs:1917-1918;
  the e2e test (lib.rs:2436-2464) pins all three legs.
- **Difficulty flags & determinism:** `apply_startup_difficulty` (lib.rs:486-499) runs
  in `assemble` before the first `step_frame`. `life_support_enabled` rides a logged
  `SetLifeSupportEnabled` (tick-0 stamp at drain) — replay reproduces it.
  `dormant_stress` → `set_reliability_config` is a construction-config write;
  `reliability_cfg` IS in `WorldSnapshot` (csp-sim lib.rs:3018/3115) and §7.6 replay
  restores from the *initial snapshot* (csp-bench replay.rs:724) — the "snapshot
  carries it" claim is mechanically true. `SETTINGS_SOURCE_ID = 3` matches the
  documented distinct-source convention (view lib.rs:65).
- **wasm persistence honesty:** `save_settings` is an in-memory `Ok(())` (settings_io.rs:143-146)
  and the Apply notice reads "cannot persist them" on wasm, "saved" on native
  (lib.rs:1013-1021). `persist_settings`/`CloseRequested` are cfg'd native-only.
- **`serde(default)` + sanitize:** every struct is `#[serde(default)]`; unknown fields
  ignored (test L908); `Keymap::sanitize` re-validates tokens, dedups, and enforces
  first-claimant one-action-per-key in `ALL` order — matching `bind`'s in-session rule.
- **AZERTY migration order-of-claim:** load-sanitize → migrate → conditional
  re-sanitize (assemble lib.rs:350-354) — collision-absorbing is correct and idempotent
  (`key-*` tokens are multi-char, so migration is a fixed point).
- **`drop_rebound_holds` (lib.rs:1633-1646):** retains held verbs by binding-*list*
  equality — a moved binding drops its held action and re-issues the combined rate;
  `Apply` → `drop_rebound_holds` runs inside `handle_ui_outbox`, before the same
  frame's `tick_attitude`/`tick_throttle_ramp`, so a rebind under a held finger stops
  the ramp that frame.
- **Rebind capture:** `rebind_target` open-gates `listening` (view:1293-1296); X-close
  and `toggle_settings_panel` both disarm; `intercept_rebind_key` swallows the captured
  *physical* key until release (modifier-shift-proof); releases of non-captured keys
  still route normally while armed, so held keys can't wedge.
- **Reset-to-defaults:** restages `Settings::default()`, disarms armed capture, honest
  "Apply to keep" notice (view:640-647) — staged copy and applied copy share one write
  path as designed.
- **Warp cap:** `set_cap_multiplier` floors at ×1, clamps to largest rung ≤ cap, demotes
  only when needed; the read-only gate on the demotion command carries the documented
  notice (lib.rs:442-450).
- **Restage-on-open:** `OpenSettings` dispatch restages the live set on open only
  (lib.rs:1743-1745) — no path opens the panel on a stale staged copy (no top-bar
  settings button exists).
- **Notice channel:** persist failure surfaces the real error as the panel notice
  (lib.rs:1023-1026); `bind_key`/`reset`/migration all land notices on the right rows.

## Minor observations (not findings)

- `bind_key`'s confirmation notice prints the raw token (`"key-w bound to pan up…"`,
  view:1312) rather than the observed glyph — cosmetic, next to F4.
- `config_dir` treats a set-but-empty `XDG_CONFIG_HOME` as `""` → `./csp/`; POSIX says
  empty means unset, but the outcome equals the documented `.` fallback anyway.
- Migration + layout-dependent notices: the second `set_settings_notice` (assemble
  lib.rs:368) overwrites the first (L359) when both fire; the migration still reaches
  the HUD via `flight_notice`, so nothing is fully lost.
- `intercept_rebind_key` ordering means a key captured for rebind does not dispatch —
  and its release calls `keyboard_event` so the held sets still clear. Correct.
