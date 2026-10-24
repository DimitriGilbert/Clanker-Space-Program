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
#[cfg(not(target_arch = "wasm32"))]
pub fn load_settings() -> Settings {
    load_settings_from(&settings_path())
}

/// The path-taking half of [`load_settings`] — what tests drive.
/// A file that parses is then sanitized: the keymap's raw-string
/// tokens are re-validated through `Keymap::sanitize` (dropping
/// unparseable/duplicate bindings with a warn) so a hand-edited file
/// meets the same rules `Keymap::bind` enforces in-session.
#[cfg(not(target_arch = "wasm32"))]
pub fn load_settings_from(path: &Path) -> Settings {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Settings::default(),
        Err(e) => {
            log::warn!(
                "settings: cannot read {}: {e} — using defaults",
                path.display()
            );
            return Settings::default();
        }
    };
    match ron::from_str::<Settings>(&text) {
        Ok(mut s) => {
            // Serde's transparent `KeyBinding` accepts raw strings —
            // a hand-edited file can carry tokens `KeyBinding::parse`
            // would refuse (dead bindings) or one key under two
            // actions. Re-run the model's own validation so file data
            // meets the same rules `Keymap::bind` enforces in-session.
            let dropped = s.keys.sanitize();
            if dropped > 0 {
                log::warn!(
                    "settings: {} carried {dropped} invalid/duplicate key binding token(s) — dropped",
                    path.display()
                );
            }
            s
        }
        Err(e) => {
            log::warn!(
                "settings: {} is not a valid settings file ({e}) — using defaults",
                path.display()
            );
            Settings::default()
        }
    }
}

/// Persist settings to `$config_dir/csp/settings.ron`. Failures log —
/// the live session keeps the settings either way.
#[cfg(not(target_arch = "wasm32"))]
pub fn save_settings(settings: &Settings) -> Result<(), String> {
    save_settings_to(&settings_path(), settings)
}

/// The path-taking half of [`save_settings`] — what tests drive.
#[cfg(not(target_arch = "wasm32"))]
pub fn save_settings_to(path: &Path, settings: &Settings) -> Result<(), String> {
    let pretty = ron::ser::PrettyConfig::new().struct_names(false);
    let text = ron::ser::to_string_pretty(settings, pretty)
        .map_err(|e| format!("settings encode: {e}"))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("settings dir {}: {e}", dir.display()))?;
    }
    std::fs::write(path, text).map_err(|e| format!("settings write {}: {e}", path.display()))
}

/// wasm: no filesystem — the session runs on defaults (the
/// `craft_store` precedent; a persistence story for the web build is a
/// §6.3 packaging question, not this milestone's).
#[cfg(target_arch = "wasm32")]
pub fn load_settings() -> Settings {
    Settings::default()
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
    keycode_token(&event.physical_key)
        .and_then(|t| KeyBinding::parse(&t))
        .or_else(|| binding_for(&event.logical_key))
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
