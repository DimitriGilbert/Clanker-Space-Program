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
    /// Increment throttle on the focused flight vessel (repeatable).
    ThrottleUp,
    /// Decrement throttle on the focused flight vessel (repeatable).
    ThrottleDown,
    /// Fire the focused vessel's next stage (jettison its drop set).
    Stage,
    /// Cycle the map camera's focus body/vessel.
    CycleFocus,
    /// Camera pan up (map and VAB); nose-down pitch in flight.
    PanUp,
    /// Camera pan down (map and VAB); nose-up pitch in flight.
    PanDown,
    /// Camera pan left (map and VAB); yaw left in flight.
    PanLeft,
    /// Camera pan right (map and VAB); yaw right in flight.
    PanRight,
    /// Roll left (counterclockwise) in flight.
    RollLeft,
    /// Roll right (clockwise) in flight.
    RollRight,
    /// Toggle the controls-help card.
    ToggleHelp,
    /// Toggle the dev debug panel (adapter/tick internals).
    ToggleDebug,
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
        Self::ThrottleUp,
        Self::ThrottleDown,
        Self::Stage,
        Self::CycleFocus,
        Self::PanUp,
        Self::PanDown,
        Self::PanLeft,
        Self::PanRight,
        Self::RollLeft,
        Self::RollRight,
        Self::ToggleHelp,
        Self::ToggleDebug,
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
            Self::ThrottleUp => "throttle up",
            Self::ThrottleDown => "throttle down",
            Self::Stage => "stage",
            Self::CycleFocus => "cycle focus",
            Self::PanUp => "pan up / pitch down",
            Self::PanDown => "pan down / pitch up",
            Self::PanLeft => "pan left / yaw left",
            Self::PanRight => "pan right / yaw right",
            Self::RollLeft => "roll left",
            Self::RollRight => "roll right",
            Self::ToggleHelp => "controls help",
            Self::ToggleDebug => "debug panel",
        }
    }

    /// Whether key-repeat events count for this action — camera pans
    /// and throttle nudges track a held key; every other action is a
    /// single-shot so a held key does not machine-gun warp steps,
    /// staging, or mode toggles.
    pub const fn accepts_repeat(self) -> bool {
        matches!(
            self,
            Self::PanUp
                | Self::PanDown
                | Self::PanLeft
                | Self::PanRight
                | Self::ThrottleUp
                | Self::ThrottleDown
        )
    }

    /// Whether the action is a held-to-turn attitude verb — the input
    /// layer tracks its press *and* release (flight mode only); map/VAB
    /// pans are still per-event camera moves, never held.
    pub const fn is_attitude(self) -> bool {
        matches!(
            self,
            Self::PanUp
                | Self::PanDown
                | Self::PanLeft
                | Self::PanRight
                | Self::RollLeft
                | Self::RollRight
        )
    }
}

/// One bound key — a host-agnostic *token*, not a winit `Key`
/// (§2.1: csp-ui never sees winit). Character keys serialize as their
/// lowercase character (`"m"`, `","`); named keys as a fixed kebab-case
/// name (`"tab"`, `"arrow-up"`, `"f5"`); physical-position keys as
/// `"key-<name>"` (`"key-w"`, `"key-comma"`) — the QWERTY-name of the
/// *position*, which is layout-independent (the same physical key
/// resolves the token whether the OS layout prints `w` or `z` on it —
/// that is what makes WASD-style controls work on AZERTY). `csp-app`
/// produces tokens from winit events and resolves actions by exact
/// match.
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

/// The physical-position tokens [`KeyBinding::parse`] accepts — the
/// `key-` prefix plus the QWERTY name of the key's *position* (winit's
/// `KeyCode` vocabulary, mirrored here as plain strings because §2.1
/// keeps winit out of this crate). `key-w` means "the key in the
/// QWERTY `W` position", which is the AZERTY `Z` key — bindings stay
/// positional, so the default movement/throttle cluster lands under
/// the same fingers on every letter layout.
pub const PHYSICAL_KEY_TOKENS: &[&str] = &[
    // Letter row positions (QWERTY names).
    "key-a",
    "key-b",
    "key-c",
    "key-d",
    "key-e",
    "key-f",
    "key-g",
    "key-h",
    "key-i",
    "key-j",
    "key-k",
    "key-l",
    "key-m",
    "key-n",
    "key-o",
    "key-p",
    "key-q",
    "key-r",
    "key-s",
    "key-t",
    "key-u",
    "key-v",
    "key-w",
    "key-x",
    "key-y",
    "key-z",
    // Digit row.
    "key-0",
    "key-1",
    "key-2",
    "key-3",
    "key-4",
    "key-5",
    "key-6",
    "key-7",
    "key-8",
    "key-9",
    // Punctuation positions (QWERTY names).
    "key-backquote",
    "key-minus",
    "key-equal",
    "key-bracket-left",
    "key-bracket-right",
    "key-backslash",
    "key-semicolon",
    "key-quote",
    "key-comma",
    "key-period",
    "key-slash",
];

impl KeyBinding {
    /// Parse and normalize a binding token: lowercase, validated
    /// against [`NAMED_KEY_TOKENS`], [`PHYSICAL_KEY_TOKENS`], or a
    /// single character. `None` for anything else — an unrecognized
    /// token in a hand-edited file is rejected rather than silently
    /// dead-bound.
    pub fn parse(token: &str) -> Option<Self> {
        let t = token.trim().to_lowercase();
        if t.chars().count() == 1
            || NAMED_KEY_TOKENS.contains(&t.as_str())
            || PHYSICAL_KEY_TOKENS.contains(&t.as_str())
        {
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

/// A physical token's printed-glyph fallback — the QWERTY character at
/// that position. Used when no observed label exists (the key was
/// never pressed this session, so the real layout's glyph is unknown).
fn physical_fallback(name: &str) -> String {
    match name.strip_prefix("key-") {
        Some(rest) if rest.chars().count() == 1 => rest.to_uppercase(),
        Some(rest) => match rest {
            "backquote" => "`",
            "minus" => "-",
            "equal" => "=",
            "bracket-left" => "[",
            "bracket-right" => "]",
            "backslash" => "\\",
            "semicolon" => ";",
            "quote" => "'",
            "comma" => ",",
            "period" => ".",
            "slash" => "/",
            "space" => "space",
            _ => rest,
        }
        .to_string(),
        None => name.to_string(),
    }
}

impl KeyBinding {
    /// The human-readable key name for hint bars and the bindings
    /// column: `observed` maps physical tokens to the glyph the key
    /// actually produced on this machine's layout (recorded by the
    /// input layer when the key was pressed — so an AZERTY `Z` shows
    /// as `z`, not `w`). Un-observed physical tokens fall back to
    /// their QWERTY glyph; character/named tokens print verbatim.
    pub fn display_label(&self, observed: &BTreeMap<String, String>) -> String {
        if let Some(l) = observed.get(&self.0) {
            return l.clone();
        }
        physical_fallback(&self.0)
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
    /// The shipped bindings. Position-sensitive keys (the
    /// movement/throttle/attitude letters and the punctuation warp
    /// keys) bind **physical positions** (`key-*`) so the layout the OS
    /// reports — QWERTY, AZERTY, Dvorak — never moves the controls:
    /// `key-w` is the same finger position everywhere. Named keys are
    /// layout-stable already and keep their plain tokens.
    fn default() -> Self {
        let b = |tokens: &[&str]| -> Vec<KeyBinding> {
            tokens.iter().filter_map(|t| KeyBinding::parse(t)).collect()
        };
        Self {
            bindings: BTreeMap::from([
                (KeyAction::ToggleMap, b(&["key-m"])),
                (KeyAction::ToggleVab, b(&["key-v"])),
                (KeyAction::OpenSettings, b(&["escape"])),
                (KeyAction::WarpUp, b(&["key-period"])),
                (KeyAction::WarpDown, b(&["key-comma"])),
                (KeyAction::ThrottleFull, b(&["key-z"])),
                (KeyAction::ThrottleCut, b(&["key-x"])),
                (KeyAction::ThrottleUp, b(&["shift"])),
                (KeyAction::ThrottleDown, b(&["control"])),
                (KeyAction::Stage, b(&["space"])),
                (KeyAction::CycleFocus, b(&["tab"])),
                (KeyAction::PanUp, b(&["key-w", "arrow-up"])),
                (KeyAction::PanDown, b(&["key-s", "arrow-down"])),
                (KeyAction::PanLeft, b(&["key-a", "arrow-left"])),
                (KeyAction::PanRight, b(&["key-d", "arrow-right"])),
                (KeyAction::RollLeft, b(&["key-q"])),
                (KeyAction::RollRight, b(&["key-e"])),
                (KeyAction::ToggleHelp, b(&["f1", "key-slash"])),
                (KeyAction::ToggleDebug, b(&["f3"])),
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

    /// Re-validate a deserialized keymap — serde accepts any string
    /// through the transparent [`KeyBinding`] newtype, so a
    /// hand-edited file can carry tokens [`KeyBinding::parse`] would
    /// refuse (dead bindings that display but can never fire) and the
    /// same token bound under two actions. Normalize each token
    /// through `parse` (trim + lowercase), drop the ones it rejects,
    /// dedup within a row, and keep each surviving token under its
    /// first claiming action in [`KeyAction::ALL`] order — the same
    /// one-action-per-key rule [`Self::bind`] enforces. Returns the
    /// dropped-token count so the load path can warn.
    pub fn sanitize(&mut self) -> usize {
        let mut dropped = 0usize;
        let mut claimed = std::collections::BTreeSet::new();
        for action in KeyAction::ALL {
            let Some(keys) = self.bindings.get_mut(action) else {
                continue;
            };
            let mut clean: Vec<KeyBinding> = Vec::with_capacity(keys.len());
            for k in std::mem::take(keys) {
                match KeyBinding::parse(k.as_str()) {
                    Some(b) if !claimed.contains(b.as_str()) => {
                        claimed.insert(b.as_str().to_owned());
                        clean.push(b);
                    }
                    // `parse` refused, a dup within the row, or a key
                    // an earlier action already claimed — dropped
                    // either way.
                    _ => dropped += 1,
                }
            }
            *keys = clean;
        }
        dropped
    }

    /// The binding display a hint line prints for `action` — every
    /// bound token joined `" / "`, or `"unbound"` when the action has
    /// no keys. Hint text derives through this so a rebind can never
    /// leave a printed key name stale.
    pub fn hint(&self, action: KeyAction) -> String {
        self.hint_labeled(action, &BTreeMap::new())
    }

    /// [`Self::hint`] with the observed physical-key labels — prints
    /// the glyph the key produces on *this* machine's layout where one
    /// has been recorded ([`KeyBinding::display_label`]).
    pub fn hint_labeled(&self, action: KeyAction, observed: &BTreeMap<String, String>) -> String {
        self.bindings
            .get(&action)
            .map(|v| {
                v.iter()
                    .map(|k| k.display_label(observed))
                    .collect::<Vec<_>>()
                    .join(" / ")
            })
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "unbound".to_string())
    }

    /// The action's first bound key — the compact hint form for a
    /// row that would otherwise stack every alternative.
    pub fn first_binding(&self, action: KeyAction) -> Option<&str> {
        self.bindings
            .get(&action)
            .and_then(|v| v.first())
            .map(KeyBinding::as_str)
    }

    /// [`Self::first_binding`] with the observed-label translation
    /// applied ([`KeyBinding::display_label`]).
    pub fn first_binding_label(
        &self,
        action: KeyAction,
        observed: &BTreeMap<String, String>,
    ) -> Option<String> {
        self.bindings
            .get(&action)
            .and_then(|v| v.first())
            .map(|k| k.display_label(observed))
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
    /// button; `csp-app` intercepts the next key press *before* egui
    /// (a focused widget cannot steal it) and resolves it into
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
/// `key_labels` is the host's observed physical-token → glyph map —
/// the bindings column prints what the keys say on *this* keyboard.
pub fn settings_panel(
    ctx: &egui::Context,
    state: &mut SettingsPanelState,
    out: &mut SettingsPanelOutput,
    key_labels: &BTreeMap<String, String>,
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
                .show(ui, |ui| keymap_grid(ui, state, key_labels));
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
    // An X-close while a rebind capture is armed must drop it — the
    // host reads `listening` for its pre-egui key intercept, and an
    // armed-but-invisible panel would swallow the next key press into
    // a keymap the user cannot see (the `OpenSettings` toggle's own
    // close path disarms too — this is the window-chrome half).
    if !state.open {
        state.listening = None;
    }
}

/// The keymap section's grid — one row per [`KeyAction`]: label, current
/// bindings, and the rebind/clear affordance (or the "press a key" hint
/// while a capture is armed). Bindings print through
/// [`KeyBinding::display_label`] — the glyph the key produces on this
/// machine's layout where one was observed.
fn keymap_grid(
    ui: &mut egui::Ui,
    state: &mut SettingsPanelState,
    labels: &BTreeMap<String, String>,
) {
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
                            .map(|k| k.display_label(labels))
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
        // Steal "key-w" from PanUp for ThrottleFull.
        let evicted = k.bind(KeyAction::ThrottleFull, KeyBinding::parse("key-w").unwrap());
        assert_eq!(evicted, Some(KeyAction::PanUp));
        assert_eq!(k.action_for("key-w"), Some(KeyAction::ThrottleFull));
        // PanUp keeps its arrow alternative.
        assert_eq!(k.action_for("arrow-up"), Some(KeyAction::PanUp));
        assert_eq!(
            k.bindings[&KeyAction::ThrottleFull],
            vec![KeyBinding::parse("key-w").unwrap()]
        );
    }

    #[test]
    fn action_for_resolves_first_match() {
        let k = Keymap::default();
        assert_eq!(k.action_for("key-m"), Some(KeyAction::ToggleMap));
        assert_eq!(k.action_for("key-period"), Some(KeyAction::WarpUp));
        assert_eq!(k.action_for("key-comma"), Some(KeyAction::WarpDown));
        assert_eq!(k.action_for("tab"), Some(KeyAction::CycleFocus));
        // A *logical* "z" is unbound — the default is the physical
        // "key-z" (position), so a bare character token resolves
        // nothing under the shipped map.
        assert_eq!(k.action_for("z"), None);
        assert_eq!(k.action_for("key-z"), Some(KeyAction::ThrottleFull));
    }

    /// R18-F5: a hand-edited file deserializes raw strings through the
    /// transparent `KeyBinding` — `sanitize` re-runs `parse`, drops
    /// dead tokens, normalizes case/whitespace, and keeps each key
    /// under its first claiming action (`bind`'s one-action-per-key
    /// rule applied to file data).
    #[test]
    fn sanitize_repairs_hand_edited_keymap() {
        let mut k = Keymap::default();
        // A dead token, an unnormalized token, an in-row dup, and a
        // cross-action dup — every shape a text editor can produce.
        k.bindings.insert(
            KeyAction::WarpUp,
            vec![
                KeyBinding("arrowup".into()), // unparseable — dead
                KeyBinding(" F6 ".into()),    // valid once normalized
                KeyBinding("f6".into()),      // in-row dup of the above
            ],
        );
        // "key-m" is ToggleMap's; claiming it under WarpDown too is the
        // file-only shape `bind` prevents in-session.
        k.bindings
            .insert(KeyAction::WarpDown, vec![KeyBinding("key-m".into())]);
        let dropped = k.sanitize();
        assert_eq!(dropped, 3); // "arrowup", dup "f6", stolen "key-m"
        assert_eq!(
            k.bindings[&KeyAction::WarpUp],
            vec![KeyBinding::parse("f6").unwrap()]
        );
        assert!(k.bindings[&KeyAction::WarpDown].is_empty());
        // The first claimant keeps the key — ToggleMap still owns
        // "key-m".
        assert_eq!(k.action_for("key-m"), Some(KeyAction::ToggleMap));
        // A clean map is a fixed point.
        assert_eq!(k.sanitize(), 0);
        assert_eq!(k.action_for("key-m"), Some(KeyAction::ToggleMap));
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
