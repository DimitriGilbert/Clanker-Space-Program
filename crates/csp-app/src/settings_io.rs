//! Settings persistence + the winit→binding-token bridge (M10).
//!
//! The [`Settings`] model lives in `csp_ui::settings` (§2.1: csp-ui
//! owns "settings, input bindings"); this module owns the *file* —
//! `$config_dir/csp/settings.ron`, §7.8's settings/keybinds sidecar —
//! and the translation of winit key events into [`KeyBinding`] tokens
//! the keymap resolves against.
//!
//! The config dir follows the platform conventions without a new
//! dependency: `$XDG_CONFIG_HOME` or `~/.config` on unix,
//! `%APPDATA%` on Windows, `~/Library/Application Support` on macOS.
//! wasm has no filesystem — the same `load`/`save` calls are in-memory
//! no-ops there (the `craft_store` precedent), so settings still apply
//! and edit for the session.

#[cfg(not(target_arch = "wasm32"))]
use std::collections::BTreeMap;
#[cfg(not(target_arch = "wasm32"))]
use std::path::{Path, PathBuf};

use csp_view::settings::{KeyBinding, Settings};
use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};

/// The app directory inside the platform config dir (§7.8's `$config_dir/csp`).
#[cfg(not(target_arch = "wasm32"))]
const APP_DIR: &str = "csp";
/// The settings file name inside that directory.
#[cfg(not(target_arch = "wasm32"))]
const SETTINGS_FILE: &str = "settings.ron";

/// The platform user-config directory — `None` when the environment
/// gives no answer (a bare container); [`settings_path`] falls back to
/// `./csp/` in that case so the file still lands somewhere named.
#[cfg(not(target_arch = "wasm32"))]
pub fn config_dir() -> PathBuf {
    #[cfg(target_os = "windows")]
    let base = std::env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|h| h.join("Library").join("Application Support"));
    #[cfg(all(unix, not(target_os = "macos")))]
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|h| h.join(".config"))
        });
    base.unwrap_or_else(|| PathBuf::from(".")).join(APP_DIR)
}

/// `$config_dir/csp/settings.ron` (native).
#[cfg(not(target_arch = "wasm32"))]
pub fn settings_path() -> PathBuf {
    config_dir().join(SETTINGS_FILE)
}

/// Load the settings file, falling back to defaults on every failure
/// class — a missing file is a first run, a corrupt file warns and
/// defaults rather than bricking the app (§2.13's "refused with a
/// message" is for *saves*; a settings file is cheap to rebuild).
/// A `schema_version` newer than this build loads anyway — §7.8's
/// forward-compat rule is "unknown keys ignored", so a newer file's
/// understood fields are kept and the rest defaulted.
///
/// Returns `(settings, notes)` — `notes` carries one user-facing line
/// per repair the load performed (a salvaged parse, dropped binding
/// tokens, backfilled verbs, rejected non-finite values) so the app
/// can surface them through the in-app notice channels: a settings
/// file that changed under the user's feet must say so, not just
/// `log::warn` (P14-F2).
#[cfg(not(target_arch = "wasm32"))]
pub fn load_settings() -> (Settings, Vec<String>) {
    load_settings_from(&settings_path())
}

/// The path-taking half of [`load_settings`] — what tests drive.
/// A file that parses is then normalized ([`normalize`]): the
/// keymap's raw-string tokens are re-validated through
/// `Keymap::sanitize`, verbs the file predates are backfilled from
/// the shipped defaults, and non-finite floats are rejected — each
/// repair recorded in the returned notes.
#[cfg(not(target_arch = "wasm32"))]
pub fn load_settings_from(path: &Path) -> (Settings, Vec<String>) {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return (Settings::default(), Vec::new());
        }
        Err(e) => {
            log::warn!(
                "settings: cannot read {}: {e} — using defaults",
                path.display()
            );
            return (
                Settings::default(),
                vec!["settings file could not be read — defaults in use".to_string()],
            );
        }
    };
    let mut notes = Vec::new();
    let mut s = match ron::from_str::<Settings>(&text) {
        Ok(s) => s,
        Err(e) => {
            // All-or-nothing is the wrong failure mode for a
            // hand-editable sidecar: one unknown keymap verb (a file
            // written by a NEWER build, or a typo'd variant name) used
            // to discard every setting with only a log line to show
            // for it (P14-F2). Salvage each field — and inside `keys`,
            // each binding row — that still parses.
            log::warn!(
                "settings: {} is not a valid settings file ({e}) — salvaging readable fields",
                path.display()
            );
            match salvage_settings(&text, &mut notes) {
                Some(s) => {
                    notes.insert(
                        0,
                        "settings file is not fully readable — kept every field that still parses"
                            .to_string(),
                    );
                    s
                }
                None => {
                    notes.push(
                        "settings file could not be parsed at all — defaults in use".to_string(),
                    );
                    Settings::default()
                }
            }
        }
    };
    normalize(&mut s, &mut notes);
    (s, notes)
}

/// Post-parse repair shared by the strict and the salvaged load:
///
/// - `Keymap::sanitize` — re-validate raw-string tokens (serde's
///   transparent `KeyBinding` accepts anything), dropping
///   unparseable/duplicate bindings so a hand-edited file meets the
///   same rules `Keymap::bind` enforces in-session.
/// - [`backfill_missing_bindings`] — a file written before a verb
///   existed never mentions it; absent actions inherit the shipped
///   defaults so upgraders don't get dead keys (P14-F2).
/// - Non-finite floats — RON accepts `NaN`/`inf` literals and
///   `f32::clamp` passes NaN straight through, so `ui_scale: NaN`
///   would reach `egui::Context::set_zoom_factor` and NaN the whole
///   UI past the point the settings panel could fix it (P14-F3).
/// - `orbit_sensitivity` — the multiplier on every camera drag: reset
///   when non-finite, clamped into its range when a hand edit left it
///   outside.
///
/// Each repair appends one user-facing line to `notes`.
#[cfg(not(target_arch = "wasm32"))]
fn normalize(s: &mut Settings, notes: &mut Vec<String>) {
    let dropped = s.keys.sanitize();
    if dropped > 0 {
        notes.push(format!(
            "{dropped} invalid/duplicate key binding token(s) dropped"
        ));
    }
    let backfilled = backfill_missing_bindings(&mut s.keys);
    if backfilled > 0 {
        notes.push(format!(
            "{backfilled} input action(s) had no binding in the settings file — default binding(s) restored"
        ));
    }
    if !s.audio.master_volume.is_finite() {
        s.audio.master_volume = csp_view::settings::AudioSettings::default().master_volume;
        notes.push("audio.master_volume was not a finite number — reset to default".into());
    }
    if !s.graphics.ui_scale.is_finite() {
        s.graphics.ui_scale = csp_view::settings::GraphicsSettings::default().ui_scale;
        notes.push("graphics.ui_scale was not a finite number — reset to default".into());
    }
    // P1: same non-finite guard as ui_scale — the render scale feeds
    // the scene target math (`Gpu::set_scene_scale` clamps bounds, but
    // NaN survives a clamp the same way) and a NaN there would poison
    // every scene target size.
    if !s.graphics.render_scale.is_finite() {
        s.graphics.render_scale = csp_view::settings::GraphicsSettings::default().render_scale;
        notes.push("graphics.render_scale was not a finite number — reset to default".into());
    }
    let (min, max) = (
        csp_view::settings::ORBIT_SENSITIVITY_MIN,
        csp_view::settings::ORBIT_SENSITIVITY_MAX,
    );
    if !s.graphics.orbit_sensitivity.is_finite() {
        s.graphics.orbit_sensitivity = csp_view::settings::ORBIT_SENSITIVITY_DEFAULT;
        notes.push("graphics.orbit_sensitivity was not a finite number — reset to default".into());
    } else if !(min..=max).contains(&s.graphics.orbit_sensitivity) {
        let asked = s.graphics.orbit_sensitivity;
        s.graphics.orbit_sensitivity = asked.clamp(min, max);
        notes.push(format!(
            "graphics.orbit_sensitivity was {asked}, outside {min} to {max} — clamped"
        ));
    }
}

/// Backfill actions the loaded keymap predates (P14-F2): `bindings`
/// deserializes only the entries present, so a `settings.ron` written
/// before a verb shipped leaves it unbound forever — a dead verb no
/// Reset-less upgrade path can revive. An action with NO `bindings`
/// entry inherits the shipped defaults for it; a *present-but-empty*
/// list stays empty (that is the deliberate unbind a user wrote — the
/// distinction `serde(default)` preserves). A default token already
/// claimed by another action is skipped, so a user's rebind is never
/// shadowed by a backfill. Returns the count of actions backfilled.
#[cfg(not(target_arch = "wasm32"))]
pub fn backfill_missing_bindings(keys: &mut csp_view::settings::Keymap) -> usize {
    use csp_view::settings::KeyAction;
    let defaults = csp_view::settings::Keymap::default();
    let mut filled = 0;
    for action in KeyAction::ALL {
        if keys.bindings.contains_key(action) {
            continue;
        }
        let claimed: std::collections::BTreeSet<&str> = keys
            .bindings
            .values()
            .flatten()
            .map(KeyBinding::as_str)
            .collect();
        let fresh: Vec<KeyBinding> = defaults
            .bindings
            .get(action)
            .into_iter()
            .flatten()
            .filter(|k| !claimed.contains(k.as_str()))
            .cloned()
            .collect();
        keys.bindings.insert(*action, fresh);
        filled += 1;
    }
    filled
}

/// One raw `name: value` slice of a RON map, keys included —
/// `RawValue` captures each field's *source text*, and `into_rust`
/// re-parses it through the real deserializer (enums included), which
/// `ron::Value` cannot do (it folds bare enum identifiers to `Unit`).
/// `RawValue` is `Ord`, so the map sorts by raw text; field lookup is
/// a linear find on a ≤6-entry table — not worth a comparator.
#[cfg(not(target_arch = "wasm32"))]
type SalvageMap = BTreeMap<Box<ron::value::RawValue>, Box<ron::value::RawValue>>;

/// `map.get(name)` — raw captures carry leading whitespace, so match
/// on trimmed text.
#[cfg(not(target_arch = "wasm32"))]
fn salvage_field<'m>(map: &'m SalvageMap, name: &str) -> Option<&'m ron::value::RawValue> {
    map.iter()
        .find(|(k, _)| k.trim().get_ron() == name)
        .map(|(_, v)| &**v)
}

/// A `(name: value, …)` field slice re-read as a map — the
/// struct-form sections (`graphics: (…)`) parse once the outer parens
/// are re-braced (RON map and struct syntaxes share `name: value`
/// pairs).
#[cfg(not(target_arch = "wasm32"))]
fn salvage_submap(raw: &ron::value::RawValue) -> Option<SalvageMap> {
    let inner = raw.trim().get_ron().strip_prefix('(')?.strip_suffix(')')?;
    ron::from_str::<SalvageMap>(&format!("{{{inner}}}")).ok()
}

/// `salvage_field` + `into_rust` + assign, recording a note on
/// unreadable text instead of failing. (A macro, not a generic fn:
/// `into_rust`'s `Deserialize` bound can't be named without a `serde`
/// dep.)
#[cfg(not(target_arch = "wasm32"))]
macro_rules! salvage_take {
    ($notes:expr, $map:expr, $name:literal, $ty:ty, $dst:expr) => {
        if let Some(raw) = salvage_field(&$map, $name) {
            match raw.into_rust::<$ty>() {
                Ok(v) => $dst = v,
                Err(_) => $notes.push(
                    concat!("settings field `", $name, "` unreadable — defaulted").to_string(),
                ),
            }
        }
    };
}

/// The lenient half of [`load_settings_from`]: when the strict
/// `Settings` parse fails — one unknown enum variant (a keymap verb a
/// newer build added, a typo'd quality tier) poisons the whole file —
/// re-read it as raw field text and re-parse field by field. Fields
/// that still parse are kept; inside `keys`, each binding row is kept
/// or dropped on its own ([`salvage_keymap`]). `None` when the file
/// is not even a RON `field: value` map (total garbage still yields
/// defaults + the caller's note).
#[cfg(not(target_arch = "wasm32"))]
fn salvage_settings(text: &str, notes: &mut Vec<String>) -> Option<Settings> {
    let inner = text.trim().strip_prefix('(')?.strip_suffix(')')?;
    let top: SalvageMap = ron::from_str(&format!("{{{inner}}}")).ok()?;
    let mut s = Settings::default();
    salvage_take!(notes, top, "schema_version", u32, s.schema_version);
    s.audio = salvage_audio(&top, notes);
    s.graphics = salvage_graphics(&top, notes);
    s.difficulty = salvage_difficulty(&top, notes);
    s.gameplay = salvage_gameplay(&top, notes);
    if let Some(raw) = salvage_field(&top, "keys") {
        match raw.into_rust::<csp_view::settings::Keymap>() {
            Ok(v) => s.keys = v,
            Err(_) => s.keys.bindings = salvage_keymap(&top, notes),
        }
    }
    Some(s)
}

/// Whole-section `into_rust` first; on failure the leaf fields are
/// salvaged one by one so a single bad value keeps its siblings —
/// the unit of damage is a field, not a file. An absent section is
/// default-quiet (serde(default) semantics), an unreadable one
/// leaves a note.
macro_rules! salvage_section_fn {
    ($fname:ident, $name:literal, $ty:ty, $sec:ident, $m:ident, { $($lf:literal : $lty:ty => $leaf:expr),* $(,)? }) => {
        #[cfg(not(target_arch = "wasm32"))]
        fn $fname(top: &SalvageMap, notes: &mut Vec<String>) -> $ty {
            let Some(raw) = salvage_field(top, $name) else {
                return <$ty>::default();
            };
            match raw.into_rust::<$ty>() {
                Ok(v) => v,
                Err(_) => {
                    // `$sec`/`$m` are caller-supplied idents so the
                    // `$leaf` expressions name them without fighting
                    // macro hygiene.
                    let mut $sec = <$ty>::default();
                    if let Some($m) = salvage_submap(raw) {
                        $(salvage_take!(notes, $m, $lf, $lty, $leaf);)*
                    }
                    notes.push(concat!(
                        "settings section `", $name,
                        "` partially unreadable — kept readable fields",
                    ).to_string());
                    $sec
                }
            }
        }
    };
}

salvage_section_fn!(salvage_audio, "audio", csp_view::settings::AudioSettings, sec, m, {
    "master_volume": f32 => sec.master_volume,
    "muted": bool => sec.muted,
});
salvage_section_fn!(salvage_graphics, "graphics", csp_view::settings::GraphicsSettings, sec, m, {
    "quality": csp_view::settings::GraphicsQuality => sec.quality,
    "vsync": csp_view::settings::VsyncMode => sec.vsync,
    "render_scale": f32 => sec.render_scale,
    "ui_scale": f32 => sec.ui_scale,
    "window_size": [u32; 2] => sec.window_size,
    "orbit_sensitivity": f32 => sec.orbit_sensitivity,
    "invert_vertical": bool => sec.invert_vertical,
});
salvage_section_fn!(salvage_difficulty, "difficulty", csp_view::settings::DifficultySettings, sec, m, {
    "life_support_enabled": bool => sec.life_support_enabled,
    "dormant_stress": bool => sec.dormant_stress,
});
salvage_section_fn!(salvage_gameplay, "gameplay", csp_view::settings::GameplaySettings, sec, m, {
    "max_warp_multiplier": u64 => sec.max_warp_multiplier,
});

/// The `keys` section's per-row salvage — `bindings` is a map from
/// the `KeyAction` enum, and one unknown variant fails the whole
/// struct parse, so rows are re-read individually: each entry's name
/// and binding list parse on their own, an unknown action or a
/// mangled list drops ONE row with a note instead of the whole
/// keymap. The returned map replaces `keys.bindings` wholesale —
/// rows the file never had are absent, so `normalize`'s backfill
/// restores their defaults, and a salvaged present-but-empty row
/// stays deliberately unbound.
#[cfg(not(target_arch = "wasm32"))]
fn salvage_keymap(
    top: &SalvageMap,
    notes: &mut Vec<String>,
) -> BTreeMap<csp_view::settings::KeyAction, Vec<KeyBinding>> {
    use csp_view::settings::KeyAction;
    let mut bindings = BTreeMap::new();
    let Some(keys_map) = salvage_field(top, "keys").and_then(salvage_submap) else {
        notes.push("settings section `keys` is unreadable — default bindings in use".into());
        return bindings;
    };
    let Some(bmap) = salvage_field(&keys_map, "bindings")
        .and_then(|r| ron::from_str::<SalvageMap>(r.get_ron()).ok())
    else {
        notes.push("settings section `keys` is unreadable — default bindings in use".into());
        return bindings;
    };
    let mut kept = 0usize;
    for (k, v) in &bmap {
        let name = k
            .trim()
            .get_ron()
            .strip_prefix("r#")
            .unwrap_or_else(|| k.trim().get_ron());
        match (k.into_rust::<KeyAction>(), v.into_rust::<Vec<KeyBinding>>()) {
            (Ok(a), Ok(list)) => {
                bindings.insert(a, list);
                kept += 1;
            }
            (Err(_), _) => notes.push(format!(
                "keymap binding(s) for `{name}` name an action this build doesn't know — dropped"
            )),
            (_, Err(_)) => {
                notes.push(format!(
                    "keymap binding(s) for `{name}` are unreadable — dropped"
                ));
            }
        }
    }
    notes.push(format!(
        "settings section `keys` partially unreadable — kept {kept} binding row(s), the rest defaulted"
    ));
    bindings
}

/// Persist settings to `$config_dir/csp/settings.ron`. Failures log —
/// the live session keeps the settings either way.
#[cfg(not(target_arch = "wasm32"))]
pub fn save_settings(settings: &Settings) -> Result<(), String> {
    save_settings_to(&settings_path(), settings)
}

/// The path-taking half of [`save_settings`] — what tests drive.
/// Writes go through a sibling `*.tmp` file + rename — the dashboard
/// store's discipline (csp-server `DashboardStore::save`): a crash
/// mid-write leaves at worst a stray `settings.ron.tmp`, never a torn
/// file the next boot has to salvage.
#[cfg(not(target_arch = "wasm32"))]
pub fn save_settings_to(path: &Path, settings: &Settings) -> Result<(), String> {
    let pretty = ron::ser::PrettyConfig::new().struct_names(false);
    let text = ron::ser::to_string_pretty(settings, pretty)
        .map_err(|e| format!("settings encode: {e}"))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("settings dir {}: {e}", dir.display()))?;
    }
    let tmp = {
        let mut t = path.as_os_str().to_owned();
        t.push(".tmp");
        PathBuf::from(t)
    };
    std::fs::write(&tmp, &text).map_err(|e| format!("settings write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        // Best-effort sweep of the orphaned temp file — the write
        // already succeeded, so only the rename failed.
        let _ = std::fs::remove_file(&tmp);
        format!(
            "settings rename {} -> {}: {e}",
            tmp.display(),
            path.display()
        )
    })
}

/// wasm: no filesystem — the session runs on defaults (the
/// `craft_store` precedent; a persistence story for the web build is a
/// §6.3 packaging question, not this milestone's). The empty notes
/// match the native signature — nothing was loaded, nothing repaired.
#[cfg(target_arch = "wasm32")]
pub fn load_settings() -> (Settings, Vec<String>) {
    (Settings::default(), Vec::new())
}

/// wasm: settings apply for the session but cannot persist — the
/// `Ok` here means "accepted for the session", and the caller's
/// notice says "applied", never "saved".
#[cfg(target_arch = "wasm32")]
pub fn save_settings(_settings: &Settings) -> Result<(), String> {
    Ok(())
}

/// Translate a winit logical key into the [`KeyBinding`] token the
/// keymap resolves against: `Key::Character` lowercases to its text,
/// `Key::Named` maps the subset of [`csp_view::settings::NAMED_KEY_TOKENS`]
/// that makes sense as game bindings. `None` = a key that cannot be
/// bound (dead keys, unidentified media keys) — it falls through the
/// dispatch harmlessly.
pub fn key_token(key: &Key) -> Option<String> {
    match key {
        Key::Character(c) => {
            let t = c.to_lowercase();
            // " " is a valid character but binds under the named token.
            if t == " " {
                Some("space".into())
            } else {
                Some(t)
            }
        }
        Key::Named(named) => Some(
            match named {
                NamedKey::Tab => "tab",
                NamedKey::Space => "space",
                NamedKey::Enter => "enter",
                NamedKey::Escape => "escape",
                NamedKey::Backspace => "backspace",
                NamedKey::Delete => "delete",
                NamedKey::Insert => "insert",
                NamedKey::Home => "home",
                NamedKey::End => "end",
                NamedKey::PageUp => "page-up",
                NamedKey::PageDown => "page-down",
                NamedKey::ArrowUp => "arrow-up",
                NamedKey::ArrowDown => "arrow-down",
                NamedKey::ArrowLeft => "arrow-left",
                NamedKey::ArrowRight => "arrow-right",
                NamedKey::Shift => "shift",
                NamedKey::Control => "control",
                NamedKey::Alt => "alt",
                NamedKey::Super => "super",
                NamedKey::CapsLock => "caps-lock",
                NamedKey::NumLock => "num-lock",
                NamedKey::ScrollLock => "scroll-lock",
                NamedKey::PrintScreen => "print-screen",
                NamedKey::Pause => "pause",
                NamedKey::ContextMenu => "context-menu",
                NamedKey::F1 => "f1",
                NamedKey::F2 => "f2",
                NamedKey::F3 => "f3",
                NamedKey::F4 => "f4",
                NamedKey::F5 => "f5",
                NamedKey::F6 => "f6",
                NamedKey::F7 => "f7",
                NamedKey::F8 => "f8",
                NamedKey::F9 => "f9",
                NamedKey::F10 => "f10",
                NamedKey::F11 => "f11",
                NamedKey::F12 => "f12",
                _ => return None,
            }
            .into(),
        ),
        _ => None,
    }
}

/// The winit key → [`KeyBinding`] pair used when the panel's rebind
/// capture lands — `key_token` plus the model's own validation (which
/// never rejects a token this module produces).
pub fn binding_for(key: &Key) -> Option<KeyBinding> {
    key_token(key).and_then(|t| KeyBinding::parse(&t))
}

/// Translate a winit *physical* key into the [`KeyBinding`] token the
/// keymap resolves against — the layout-independent path. `KeyCode`
/// names the key by its *position* (the QWERTY label), not the
/// character it types: the `W`-position key produces `key-w` whether
/// the layout prints `w` (QWERTY) or `z` (AZERTY) on it, so the
/// default movement/throttle cluster stays under the same fingers.
/// Named positional codes (space, arrows, F-keys, modifiers) map onto
/// the existing [`csp_view::settings::NAMED_KEY_TOKENS`] vocabulary —
/// they are layout-stable already. `None` for unidentifiable codes.
pub fn keycode_token(key: &PhysicalKey) -> Option<String> {
    let code = match key {
        PhysicalKey::Code(c) => *c,
        PhysicalKey::Unidentified(_) => return None,
    };
    let token = match code {
        // Letter positions → `key-<letter>`.
        KeyCode::KeyA => "key-a",
        KeyCode::KeyB => "key-b",
        KeyCode::KeyC => "key-c",
        KeyCode::KeyD => "key-d",
        KeyCode::KeyE => "key-e",
        KeyCode::KeyF => "key-f",
        KeyCode::KeyG => "key-g",
        KeyCode::KeyH => "key-h",
        KeyCode::KeyI => "key-i",
        KeyCode::KeyJ => "key-j",
        KeyCode::KeyK => "key-k",
        KeyCode::KeyL => "key-l",
        KeyCode::KeyM => "key-m",
        KeyCode::KeyN => "key-n",
        KeyCode::KeyO => "key-o",
        KeyCode::KeyP => "key-p",
        KeyCode::KeyQ => "key-q",
        KeyCode::KeyR => "key-r",
        KeyCode::KeyS => "key-s",
        KeyCode::KeyT => "key-t",
        KeyCode::KeyU => "key-u",
        KeyCode::KeyV => "key-v",
        KeyCode::KeyW => "key-w",
        KeyCode::KeyX => "key-x",
        KeyCode::KeyY => "key-y",
        KeyCode::KeyZ => "key-z",
        // Digit row.
        KeyCode::Digit0 => "key-0",
        KeyCode::Digit1 => "key-1",
        KeyCode::Digit2 => "key-2",
        KeyCode::Digit3 => "key-3",
        KeyCode::Digit4 => "key-4",
        KeyCode::Digit5 => "key-5",
        KeyCode::Digit6 => "key-6",
        KeyCode::Digit7 => "key-7",
        KeyCode::Digit8 => "key-8",
        KeyCode::Digit9 => "key-9",
        // Punctuation positions.
        KeyCode::Backquote => "key-backquote",
        KeyCode::Minus => "key-minus",
        KeyCode::Equal => "key-equal",
        KeyCode::BracketLeft => "key-bracket-left",
        KeyCode::BracketRight => "key-bracket-right",
        KeyCode::Backslash => "key-backslash",
        KeyCode::Semicolon => "key-semicolon",
        KeyCode::Quote => "key-quote",
        KeyCode::Comma => "key-comma",
        KeyCode::Period => "key-period",
        KeyCode::Slash => "key-slash",
        // Named positions → the plain named tokens (layout-stable).
        KeyCode::Space => "space",
        KeyCode::Tab => "tab",
        KeyCode::Enter => "enter",
        KeyCode::Escape => "escape",
        KeyCode::Backspace => "backspace",
        KeyCode::Delete => "delete",
        KeyCode::Insert => "insert",
        KeyCode::Home => "home",
        KeyCode::End => "end",
        KeyCode::PageUp => "page-up",
        KeyCode::PageDown => "page-down",
        KeyCode::ArrowUp => "arrow-up",
        KeyCode::ArrowDown => "arrow-down",
        KeyCode::ArrowLeft => "arrow-left",
        KeyCode::ArrowRight => "arrow-right",
        // Left/right collapse into the shared modifier tokens — a
        // binding on "shift" takes either side (the `key_token` path
        // made the same choice through `NamedKey::Shift`).
        KeyCode::ShiftLeft | KeyCode::ShiftRight => "shift",
        KeyCode::ControlLeft | KeyCode::ControlRight => "control",
        KeyCode::AltLeft | KeyCode::AltRight => "alt",
        KeyCode::SuperLeft | KeyCode::SuperRight => "super",
        KeyCode::CapsLock => "caps-lock",
        KeyCode::NumLock => "num-lock",
        KeyCode::ScrollLock => "scroll-lock",
        KeyCode::PrintScreen => "print-screen",
        KeyCode::Pause => "pause",
        KeyCode::ContextMenu => "context-menu",
        KeyCode::F1 => "f1",
        KeyCode::F2 => "f2",
        KeyCode::F3 => "f3",
        KeyCode::F4 => "f4",
        KeyCode::F5 => "f5",
        KeyCode::F6 => "f6",
        KeyCode::F7 => "f7",
        KeyCode::F8 => "f8",
        KeyCode::F9 => "f9",
        KeyCode::F10 => "f10",
        KeyCode::F11 => "f11",
        KeyCode::F12 => "f12",
        _ => return None,
    };
    Some(token.to_string())
}

/// The binding a rebind capture records for `event` — the physical
/// token when the key has one (a `key-*` position token binds
/// positionally, so a capture on an AZERTY `Z` records `key-w` and
/// works as the W-position on every layout), else the logical token.
pub fn event_binding(event: &winit::event::KeyEvent) -> Option<KeyBinding> {
    binding_of(&event.physical_key, &event.logical_key)
}

/// [`event_binding`] on the decomposed key identities — the capture
/// path a `KeyEvent`-less caller (tests) drives.
pub fn binding_of(physical: &PhysicalKey, logical: &Key) -> Option<KeyBinding> {
    keycode_token(physical)
        .and_then(|t| KeyBinding::parse(&t))
        .or_else(|| binding_for(logical))
}

/// The character a saved binding token uses → its positional `key-*`
/// token — the inverse of the punctuation names `keycode_token`
/// produces (letters and digits are `key-<char>`). `None` for
/// characters with no positional equivalent (non-Latin glyphs) and
/// for non-character tokens (named keys, `key-*` — already
/// layout-stable).
fn char_to_physical_token(token: &str) -> Option<String> {
    let mut it = token.chars();
    let (Some(c), None) = (it.next(), it.next()) else {
        return None;
    };
    match c.to_ascii_lowercase() {
        c @ ('a'..='z' | '0'..='9') => Some(format!("key-{c}")),
        // A bare " " is a valid one-char binding the event path can
        // never produce (space resolves as the "space" named token) —
        // migrating it fixes a dead binding too.
        ' ' => Some("space".to_string()),
        '`' => Some("key-backquote".into()),
        '-' => Some("key-minus".into()),
        '=' => Some("key-equal".into()),
        '[' => Some("key-bracket-left".into()),
        ']' => Some("key-bracket-right".into()),
        '\\' => Some("key-backslash".into()),
        ';' => Some("key-semicolon".into()),
        '\'' => Some("key-quote".into()),
        ',' => Some("key-comma".into()),
        '.' => Some("key-period".into()),
        '/' => Some("key-slash".into()),
        _ => None,
    }
}

/// Migrate a loaded keymap's character bindings onto positional
/// `key-*` tokens. Settings files written before positional tokens
/// existed bound the movement/throttle cluster by *typed character*
/// (`"w"`, `"z"`) — layout-dependent tokens that leave an AZERTY
/// user's controls on the wrong keys until a Reset-to-defaults (the
/// shipped map binds positions for exactly this reason). A character
/// with a positional equivalent becomes its `key-*` token; the rest
/// stay characters and surface through
/// [`layout_dependent_binding_count`]. Returns the migrated count —
/// the caller re-runs `Keymap::sanitize` to absorb collisions the
/// rewrite can produce (a hand-bound `"key-w"` beside an inherited
/// `"w"`).
pub fn migrate_char_bindings(keys: &mut csp_view::settings::Keymap) -> usize {
    let mut migrated = 0;
    for list in keys.bindings.values_mut() {
        for k in list.iter_mut() {
            if let Some(t) = char_to_physical_token(k.as_str())
                && let Some(b) = KeyBinding::parse(&t)
            {
                *k = b;
                migrated += 1;
            }
        }
    }
    migrated
}

/// Count bindings still bound to a bare character — after
/// [`migrate_char_bindings`] those are characters with no positional
/// equivalent: layout-dependent forever, the "you have bindings that
/// move with your keyboard layout" the user should be told about.
pub fn layout_dependent_binding_count(keys: &csp_view::settings::Keymap) -> usize {
    keys.bindings
        .values()
        .flatten()
        .filter(|k| k.as_str().chars().count() == 1)
        .count()
}

/// The printed glyph an event's physical token should display as —
/// the character the key actually produced on this machine's layout.
/// `None` for non-character keys (their token names the key already).
/// The input layer records this per `key-*` token so hint bars can
/// show an AZERTY `z` where a QWERTY hint would show `w`.
pub fn event_label(event: &winit::event::KeyEvent) -> Option<(String, String)> {
    let token = keycode_token(&event.physical_key)?;
    if !token.starts_with("key-") {
        return None;
    }
    if let Key::Character(c) = &event.logical_key {
        let t = c.to_lowercase();
        if t.chars().count() == 1 {
            return Some((token, t));
        }
    }
    None
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// The tmp-file + rename write lands the target and leaves no
    /// stray `*.tmp` — including the overwrite path where the file
    /// already exists (`rename` must replace it atomically).
    #[test]
    fn save_is_tmp_plus_rename() {
        let dir = std::env::temp_dir().join(format!("csp-settings-io-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.ron");
        std::fs::write(&path, "stale").unwrap();
        let s = Settings::default();
        save_settings_to(&path, &s).unwrap();
        assert_eq!(load_settings_from(&path), (s, Vec::new()));
        assert!(
            !dir.join("settings.ron.tmp").exists(),
            "temp file must not survive the rename"
        );
        // Save again over an existing file — the replace path a
        // crash-mid-write fix must not regress.
        let mut s2 = Settings::default();
        s2.audio.master_volume = 0.5;
        save_settings_to(&path, &s2).unwrap();
        assert_eq!(load_settings_from(&path), (s2, Vec::new()));
        assert!(!dir.join("settings.ron.tmp").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
