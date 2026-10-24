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
use winit::keyboard::{Key, NamedKey};

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
