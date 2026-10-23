//! The persistent settings model + the settings panel (PLAN §2.1's
//! "settings, input bindings" for csp-ui, M10).
//!
//! [`Settings`] is the serializable user-preference model — audio,
//! graphics quality, window size and UI scale, the difficulty flags
//! applied at new-game, live gameplay preferences, and the rebindable
//! [`Keymap`]. It is *plain data* (serde): `csp-app` owns the
//! `$config_dir/csp/settings.ron` file (§7.8's sidecar class — outside
//! the save, the command log, and the state hash), and the winit
//! `Key` ↔ [`KeyBinding`] token mapping. This module knows nothing of
//! either: bindings are string tokens, not winit types.
//!
//! Forward-compatibility is §7.8's cheap rule: every field is
//! `#[serde(default)]` and unknown keys are ignored, so a newer file
//! loads with the fields it understands filled and the rest defaulted.
//! `schema_version` exists for the day that stops being enough.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

/// The settings file's own schema version (§7.8) — bumped only when a
/// fields-are-defaulted parse is no longer honest.
pub const SETTINGS_SCHEMA_VERSION: u32 = 1;

/// The whole settings file (§7.8 sidecar). `csp-app` serializes this to
/// `$config_dir/csp/settings.ron`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// [`SETTINGS_SCHEMA_VERSION`] of the writer.
    pub schema_version: u32,
    /// Audio preferences — consumed by the M10 audio sub-phase; until
    /// then they persist and round-trip but steer nothing.
    pub audio: AudioSettings,
    /// Presentation preferences — `quality` selects the render tier the
    /// M10 render work consumes.
    pub graphics: GraphicsSettings,
    /// New-game difficulty flags — applied when a save is *created*
    /// (they write sim state/construction config), never retroactively
    /// into a live session.
    pub difficulty: DifficultySettings,
    /// Live-session gameplay preferences — applied immediately, unlike
    /// [`DifficultySettings`] which binds at new-game only.
    pub gameplay: GameplaySettings,
    /// The rebindable keymap — the input layer resolves every key press
    /// through it.
    pub keys: Keymap,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema_version: SETTINGS_SCHEMA_VERSION,
            audio: AudioSettings::default(),
            graphics: GraphicsSettings::default(),
            difficulty: DifficultySettings::default(),
            gameplay: GameplaySettings::default(),
            keys: Keymap::default(),
        }
    }
}

/// Audio preferences (M10 audio lands after the plumbing).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioSettings {
    /// Master volume in `[0, 1]`.
    pub master_volume: f32,
    /// Mute the whole mix without losing the volume setting.
    pub muted: bool,
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self {
            master_volume: 0.8,
            muted: false,
        }
    }
}

/// Presentation preferences.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GraphicsSettings {
    /// The render-quality tier.
    pub quality: GraphicsQuality,
    /// The egui zoom factor — the whole UI scales by it (1.0 = native).
    /// Clamped into [`UI_SCALE_MIN`]..=[`UI_SCALE_MAX`] when applied.
    pub ui_scale: f32,
    /// The last window size in physical px — tracked by the app on
    /// `Resized` and persisted, so the next launch restores it. Not a
    /// panel-edited value: the window itself is the editor.
    pub window_size: [u32; 2],
}

/// The lower bound [`GraphicsSettings::ui_scale`] applies at — under
/// this the panels become unreadable.
pub const UI_SCALE_MIN: f32 = 0.5;
/// The upper bound [`GraphicsSettings::ui_scale`] applies at.
pub const UI_SCALE_MAX: f32 = 2.0;

impl Default for GraphicsSettings {
    fn default() -> Self {
        Self {
            quality: GraphicsQuality::default(),
            ui_scale: 1.0,
            window_size: [1280, 720],
        }
    }
}

/// The render-quality ladder the settings file names.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GraphicsQuality {
    /// The floor tier.
    Low,
    /// The default tier.
    #[default]
    Medium,
    /// The full tier.
    High,
}

/// New-game difficulty flags — written into the world at save creation:
/// `life_support_enabled` rides the logged `SetLifeSupportEnabled`
/// command; `dormant_stress` scales the §3.4 dormant hazard via the
/// world's reliability config. Both are sim state once applied — the
/// flags here are the *default for new saves*, not a live override.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DifficultySettings {
    /// Start saves with the §3.5 life-support subsystem enabled
    /// (default OFF, matching `World`'s opt-in convention).
    pub life_support_enabled: bool,
    /// Start saves with the §3.4 dormant-hazard mechanic enabled
    /// (`reliability_dormant_stress_scale`, default ON).
    pub dormant_stress: bool,
}

impl Default for DifficultySettings {
    fn default() -> Self {
        Self {
            life_support_enabled: false,
            dormant_stress: true,
        }
    }
}

/// The selectable time-warp ceilings — mirrors `csp-app`'s §2.6.2
/// `WARP_LADDER` (the app's copy is authoritative; this list is only the
/// panel's choice vocabulary, and the app clamps any value to the
/// largest rung `≤` it, so drift between the lists degrades gracefully
/// rather than breaking).
pub const WARP_CAP_OPTIONS: &[u64] = &[1, 2, 10, 100, 1_000, 10_000, 100_000];

/// Live-session gameplay preferences — applied the moment Apply lands,
/// unlike the new-game [`DifficultySettings`].
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GameplaySettings {
    /// The highest time-warp multiplier the player can step the ladder
    /// to — a QoL ceiling, not a sim rule: the sim's §2.6.2 regime
    /// gates and the §2.6.3 auto-derate still apply underneath it.
    /// Stored as the literal multiplier; the app clamps it onto the
    /// warp ladder.
    pub max_warp_multiplier: u64,
}

impl Default for GameplaySettings {
    fn default() -> Self {
        Self {
            max_warp_multiplier: 100_000,
        }
    }
}

/// One rebindable input action — the vocabulary `csp-app`'s input
/// layer dispatches on. Serialized as the keymap's map keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum KeyAction {
    /// Toggle the system map view.
    ToggleMap,
    /// Toggle the VAB editor.
    ToggleVab,
    /// Open/close the settings window.
    OpenSettings,
    /// Step the warp ladder up.
    WarpUp,
    /// Step the warp ladder down.
    WarpDown,
    /// Full throttle on the focused flight vessel.
    ThrottleFull,
    /// Cut throttle on the focused flight vessel.
    ThrottleCut,
    /// Cycle the map camera's focus body/vessel.
    CycleFocus,
    /// Camera pan up (map and VAB).
    PanUp,
    /// Camera pan down (map and VAB).
    PanDown,
    /// Camera pan left (map and VAB).
    PanLeft,
    /// Camera pan right (map and VAB).
    PanRight,
}

impl KeyAction {
    /// Every action, in stable panel order.
    pub const ALL: &[Self] = &[
        Self::ToggleMap,
        Self::ToggleVab,
        Self::OpenSettings,
        Self::WarpUp,
        Self::WarpDown,
        Self::ThrottleFull,
        Self::ThrottleCut,
        Self::CycleFocus,
        Self::PanUp,
        Self::PanDown,
        Self::PanLeft,
        Self::PanRight,
    ];

    /// The panel label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::ToggleMap => "toggle map",
            Self::ToggleVab => "toggle VAB",
            Self::OpenSettings => "open settings",
            Self::WarpUp => "warp faster",
            Self::WarpDown => "warp slower",
            Self::ThrottleFull => "throttle full",
            Self::ThrottleCut => "throttle cut",
            Self::CycleFocus => "cycle focus",
            Self::PanUp => "pan up",
            Self::PanDown => "pan down",
            Self::PanLeft => "pan left",
            Self::PanRight => "pan right",
        }
    }

    /// Whether key-repeat events count for this action — camera pans
    /// track a held key; every other action is a single-shot so a held
    /// key does not machine-gun warp steps or mode toggles.
    pub const fn accepts_repeat(self) -> bool {
        matches!(
            self,
            Self::PanUp | Self::PanDown | Self::PanLeft | Self::PanRight
        )
    }
}

/// One bound key — a host-agnostic *token*, not a winit `Key`
/// (§2.1: csp-ui never sees winit). Character keys serialize as their
/// lowercase character (`"m"`, `","`); named keys as a fixed kebab-case
/// name (`"tab"`, `"arrow-up"`, `"f5"`). `csp-app` produces tokens from
/// winit events and resolves actions by exact match.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct KeyBinding(pub String);

/// The named-key tokens [`KeyBinding::parse`] accepts beyond a single
/// character — kept in one table so the parser, the file format, and
/// the `csp-app` winit mapping can never drift.
pub const NAMED_KEY_TOKENS: &[&str] = &[
    "tab",
    "space",
    "enter",
    "escape",
    "backspace",
    "delete",
    "insert",
    "home",
    "end",
    "page-up",
    "page-down",
    "arrow-up",
    "arrow-down",
    "arrow-left",
    "arrow-right",
    "shift",
    "control",
    "alt",
    "super",
    "caps-lock",
    "num-lock",
    "scroll-lock",
    "print-screen",
    "pause",
    "context-menu",
    "f1",
    "f2",
    "f3",
    "f4",
    "f5",
    "f6",
    "f7",
    "f8",
    "f9",
    "f10",
    "f11",
    "f12",
];

impl KeyBinding {
    /// Parse and normalize a binding token: lowercase, validated
    /// against [`NAMED_KEY_TOKENS`] or a single character. `None` for
    /// anything else — an unrecognized token in a hand-edited file is
    /// rejected rather than silently dead-bound.
    pub fn parse(token: &str) -> Option<Self> {
        let t = token.trim().to_lowercase();
        if t.chars().count() == 1 || NAMED_KEY_TOKENS.contains(&t.as_str()) {
            Some(Self(t))
        } else {
            None
        }
    }

    /// The normalized token.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for KeyBinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The rebindable keymap — every input action → the keys that fire it.
/// A `BTreeMap` so the serialized file is ordered and a diff of two
/// keymaps is readable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Keymap {
    /// `action → bound keys`. An action with an empty (or missing) list
    /// is unbound — the file keeps it valid.
    pub bindings: BTreeMap<KeyAction, Vec<KeyBinding>>,
}

impl Default for Keymap {
    /// The shipped bindings — exactly the keys the M0–M9d hardcoded
    /// dispatch used, plus `escape` for the new settings toggle.
    fn default() -> Self {
        let b = |tokens: &[&str]| -> Vec<KeyBinding> {
            tokens.iter().filter_map(|t| KeyBinding::parse(t)).collect()
        };
        Self {
            bindings: BTreeMap::from([
                (KeyAction::ToggleMap, b(&["m"])),
                (KeyAction::ToggleVab, b(&["v"])),
                (KeyAction::OpenSettings, b(&["escape"])),
                (KeyAction::WarpUp, b(&["."])),
                (KeyAction::WarpDown, b(&[","])),
                (KeyAction::ThrottleFull, b(&["z"])),
                (KeyAction::ThrottleCut, b(&["x"])),
                (KeyAction::CycleFocus, b(&["tab"])),
                (KeyAction::PanUp, b(&["w", "arrow-up"])),
                (KeyAction::PanDown, b(&["s", "arrow-down"])),
                (KeyAction::PanLeft, b(&["a", "arrow-left"])),
                (KeyAction::PanRight, b(&["d", "arrow-right"])),
            ]),
        }
    }
}

impl Keymap {
    /// The first action bound to `token`, in `KeyAction` order (the
    /// input layer's resolution order — stable and total). [`bind`]
    /// keeps the map one-action-per-key so at most one can match.
    pub fn action_for(&self, token: &str) -> Option<KeyAction> {
        KeyAction::ALL.iter().copied().find(|a| {
            self.bindings
                .get(a)
                .is_some_and(|keys| keys.iter().any(|k| k.0 == token))
        })
    }

    /// The actions `token` currently fires (empty = unbound).
    pub fn bound_actions(&self, token: &str) -> Vec<KeyAction> {
        KeyAction::ALL
            .iter()
            .copied()
            .filter(|a| {
                self.bindings
                    .get(a)
                    .is_some_and(|keys| keys.iter().any(|k| k.0 == token))
            })
            .collect()
    }

    /// Rebind `action` to `key` — replacing the action's whole binding
    /// list and evicting `key` from every *other* action, so the map
    /// stays one-action-per-key (a stolen key never double-fires).
    /// Returns the action `key` was evicted from, if any.
    pub fn bind(&mut self, action: KeyAction, key: KeyBinding) -> Option<KeyAction> {
        let mut evicted = None;
        for (a, keys) in &mut self.bindings {
            if *a != action && keys.iter().any(|k| k == &key) {
                keys.retain(|k| k != &key);
                evicted = Some(*a);
            }
        }
        self.bindings.insert(action, vec![key]);
        evicted
    }

    /// Remove every binding for `action` (the row's "clear" affordance).
    pub fn clear(&mut self, action: KeyAction) {
        self.bindings.insert(action, Vec::new());
    }
}

/// The settings window's session state — owned by `csp-view` (§2.2-C
/// session data), mutated by [`settings_panel`] inside `UiBuild`.
#[derive(Clone, Debug, Default)]
pub struct SettingsPanelState {
    /// Whether the window is shown — toggled by the host's
    /// `OpenSettings` binding or the window's own close button.
    pub open: bool,
    /// The staged settings the widgets edit — copied from the app's
    /// live settings when the window opens, applied back on "Apply".
    pub settings: Settings,
    /// The action awaiting a captured key — set by the row's "rebind"
    /// button; `csp-app` resolves the next unconsumed key press into
    /// [`Keymap::bind`] (Escape cancels). `None` = not listening.
    pub listening: Option<KeyAction>,
    /// One-line feedback ("settings saved", a stolen-key notice, …).
    pub notice: Option<String>,
}

/// Requests the panel emits for the host (`csp-app`) — consumed via the
/// outbox at next frame's Input like every UiBuild product (§2.5).
#[derive(Clone, Copy, Debug, Default)]
pub struct SettingsPanelOutput {
    /// "Apply" pressed: persist the staged settings and apply them.
    pub apply: bool,
    /// "Reset to defaults" pressed: the host restages defaults (kept as
    /// a request, not a silent in-place reset, so the staged copy and
    /// the applied copy are the same write path).
    pub reset_defaults: bool,
}

/// The settings window — the egui half of M10's settings surface.
/// Edits [`SettingsPanelState::settings`] in place; the host persists
/// on `apply` and owns key-capture while `listening` is armed.
pub fn settings_panel(
    ctx: &egui::Context,
    state: &mut SettingsPanelState,
    out: &mut SettingsPanelOutput,
) {
    let mut open = state.open;
    egui::Window::new("Settings")
        .open(&mut open)
        .collapsible(false)
        .show(ctx, |ui| {
            egui::CollapsingHeader::new("Audio")
                .default_open(true)
                .show(ui, |ui| {
                    ui.add(
                        egui::Slider::new(&mut state.settings.audio.master_volume, 0.0..=1.0)
                            .text("master volume"),
                    );
                    ui.checkbox(&mut state.settings.audio.muted, "muted");
                });
            egui::CollapsingHeader::new("Graphics")
                .default_open(true)
                .show(ui, |ui| {
                    egui::ComboBox::from_label("quality")
                        .selected_text(match state.settings.graphics.quality {
                            GraphicsQuality::Low => "low",
                            GraphicsQuality::Medium => "medium",
                            GraphicsQuality::High => "high",
                        })
                        .show_ui(ui, |ui| {
                            for (q, label) in [
                                (GraphicsQuality::Low, "low"),
                                (GraphicsQuality::Medium, "medium"),
                                (GraphicsQuality::High, "high"),
                            ] {
                                ui.selectable_value(&mut state.settings.graphics.quality, q, label);
                            }
                        });
                    ui.add(
                        egui::Slider::new(
                            &mut state.settings.graphics.ui_scale,
                            UI_SCALE_MIN..=UI_SCALE_MAX,
                        )
                        .text("ui scale"),
                    );
                    ui.label(format!(
                        "window: {}×{} px (restored at launch)",
                        state.settings.graphics.window_size[0],
                        state.settings.graphics.window_size[1],
                    ));
                });
            egui::CollapsingHeader::new("Gameplay")
                .default_open(true)
                .show(ui, |ui| {
                    egui::ComboBox::from_label("max time warp")
                        .selected_text(format!("×{}", state.settings.gameplay.max_warp_multiplier))
                        .show_ui(ui, |ui| {
                            for &cap in WARP_CAP_OPTIONS {
                                ui.selectable_value(
                                    &mut state.settings.gameplay.max_warp_multiplier,
                                    cap,
                                    format!("×{cap}"),
                                );
                            }
                        });
                });
            egui::CollapsingHeader::new("Difficulty (applies to new games)")
                .default_open(true)
                .show(ui, |ui| {
                    ui.checkbox(
                        &mut state.settings.difficulty.life_support_enabled,
                        "life support",
                    );
                    ui.checkbox(
                        &mut state.settings.difficulty.dormant_stress,
                        "dormant stress",
                    );
                });
            egui::CollapsingHeader::new("Key bindings")
                .default_open(true)
                .show(ui, |ui| keymap_grid(ui, state));
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Apply").clicked() {
                    out.apply = true;
                }
                if ui.button("Reset to defaults").clicked() {
                    out.reset_defaults = true;
                }
            });
            if let Some(notice) = &state.notice {
                ui.label(notice);
            }
        });
    state.open = open;
}

/// The keymap section's grid — one row per [`KeyAction`]: label, current
/// bindings, and the rebind/clear affordance (or the "press a key" hint
/// while a capture is armed).
fn keymap_grid(ui: &mut egui::Ui, state: &mut SettingsPanelState) {
    egui::Grid::new("keymap_grid")
        .num_columns(3)
        .show(ui, |ui| {
            for action in KeyAction::ALL {
                ui.label(action.label());
                let keys = state
                    .settings
                    .keys
                    .bindings
                    .get(action)
                    .map(|v| {
                        v.iter()
                            .map(KeyBinding::as_str)
                            .collect::<Vec<_>>()
                            .join("  ")
                    })
                    .unwrap_or_default();
                ui.label(if keys.is_empty() {
                    "—".to_string()
                } else {
                    keys
                });
                if state.listening == Some(*action) {
                    ui.label("press a key…  (esc cancels)");
                } else {
                    ui.horizontal(|ui| {
                        if ui.button("rebind").clicked() {
                            state.listening = Some(*action);
                            state.notice = None;
                        }
                        if ui.button("clear").clicked() {
                            state.settings.keys.clear(*action);
                        }
                    });
                }
                ui.end_row();
            }
        });
}

#[cfg(test)]
// Tests signal failure by panicking — unwrap/expect stays legal here
// (§2.13's no-panic rule covers shipping fallible paths).
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn settings_ron_round_trip() {
        let mut s = Settings::default();
        s.audio.master_volume = 0.35;
        s.graphics.quality = GraphicsQuality::Low;
        s.graphics.ui_scale = 1.25;
        s.graphics.window_size = [1600, 900];
        s.difficulty.life_support_enabled = true;
        s.difficulty.dormant_stress = false;
        s.gameplay.max_warp_multiplier = 1_000;
        s.keys
            .bind(KeyAction::WarpUp, KeyBinding::parse("f6").unwrap());
        let text = ron::ser::to_string_pretty(&s, ron::ser::PrettyConfig::default()).unwrap();
        let back: Settings = ron::from_str(&text).unwrap();
        assert_eq!(back, s);
    }

    /// §7.8's forward-compat rule: a file with extra keys and a newer
    /// schema_version still parses — the fields it shares are loaded,
    /// the rest defaulted, nothing refused.
    #[test]
    fn unknown_keys_and_future_fields_ignored() {
        let text = r#"(
            schema_version: 99,
            future_toggle: true,
            audio: (master_volume: 0.5, muted: true, spatial: false),
            graphics: (quality: high, ray_traced: true),
            difficulty: (life_support_enabled: true, ironman: true),
            keys: (bindings: {}),
        )"#;
        let s: Settings = ron::from_str(text).unwrap();
        assert_eq!(s.audio.master_volume, 0.5);
        assert!(s.audio.muted);
        assert!(s.difficulty.life_support_enabled);
        assert_eq!(s.graphics.quality, GraphicsQuality::High);
        // Missing sections fall back to defaults.
        assert!(s.difficulty.dormant_stress);
        assert_eq!(s.gameplay, GameplaySettings::default());
        assert_eq!(s.graphics.window_size, [1280, 720]);
        assert!(s.keys.bindings.is_empty());
    }

    #[test]
    fn keymap_default_covers_every_action() {
        let k = Keymap::default();
        for a in KeyAction::ALL {
            assert!(
                k.bindings.get(a).is_some_and(|v| !v.is_empty()),
                "{a:?} has no default binding"
            );
        }
        // And no token double-fires.
        for a in KeyAction::ALL {
            for key in &k.bindings[a] {
                assert_eq!(k.bound_actions(key.as_str()), vec![*a]);
            }
        }
    }

    #[test]
    fn rebind_replaces_and_evicts() {
        let mut k = Keymap::default();
        // Steal "w" from PanUp for ThrottleFull.
        let evicted = k.bind(KeyAction::ThrottleFull, KeyBinding::parse("w").unwrap());
        assert_eq!(evicted, Some(KeyAction::PanUp));
        assert_eq!(k.action_for("w"), Some(KeyAction::ThrottleFull));
        // PanUp keeps its arrow alternative.
        assert_eq!(k.action_for("arrow-up"), Some(KeyAction::PanUp));
        assert_eq!(
            k.bindings[&KeyAction::ThrottleFull],
            vec![KeyBinding::parse("w").unwrap()]
        );
    }

    #[test]
    fn action_for_resolves_first_match() {
        let k = Keymap::default();
        assert_eq!(k.action_for("m"), Some(KeyAction::ToggleMap));
        assert_eq!(k.action_for("."), Some(KeyAction::WarpUp));
        assert_eq!(k.action_for(","), Some(KeyAction::WarpDown));
        assert_eq!(k.action_for("tab"), Some(KeyAction::CycleFocus));
        assert_eq!(k.action_for("q"), None);
    }

    #[test]
    fn keybinding_parse_validation() {
        assert_eq!(KeyBinding::parse("M").unwrap().as_str(), "m");
        assert_eq!(KeyBinding::parse("Tab").unwrap().as_str(), "tab");
        assert_eq!(
            KeyBinding::parse(" arrow-up ").unwrap().as_str(),
            "arrow-up"
        );
        assert!(KeyBinding::parse("arrowup").is_none());
        assert!(KeyBinding::parse("hyper").is_none());
        assert!(KeyBinding::parse("").is_none());
        assert!(KeyBinding::parse("ab").is_none());
    }
}
