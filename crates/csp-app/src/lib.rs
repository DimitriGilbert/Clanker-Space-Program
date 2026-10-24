//! Application shell: winit window + wgpu surface + egui paint, driving the
//! `csp-view` presentation world in the literal PLAN §2.5 frame-loop shape:
//!
//! ```text
//! winit RedrawRequested:
//!   dt_wall = clamp(now - last, 0, 100ms)        // wall clock ONLY here
//!   warp.observe(last_tick_ms, dt_wall)         // §2.6.3 — per-tick, wall-side
//!   if let Some(l) = warp.decide() { enqueue SetWarp(l) }
//!   target = warp.target_tick(clock, dt_wall)   // integer target
//!   enqueue AdvanceTo(target)                   // §7.6: log is self-contained
//!   Input → SimStep → Sync → Presentation → UiBuild → RenderExtract
//!   paint the UiBuild-produced FullOutput via egui-wgpu
//! ```
//!
//! `csp-app` never reads `view.world` for UI (§2.5): input arrives through
//! `View`'s plumbing methods, the whole egui pass is the `ui_build`
//! schedule's `csp_ui` panel system, and render consumes the stored
//! `FullOutput`. Native and wasm share this code; the entry points differ
//! (`main.rs` vs `wasm_start` below).
//!
//! M10 additions: persistent settings + a rebindable keymap
//! ([`settings_io`]), the §2.13/§2.14 crash path — a `Fatal` or a
//! panic halts the clock, marks the session read-only, writes a
//! bug-report bundle ([`bugreport`]), and surfaces it in the UI — and
//! the real audio backend ([`audio`], native-only): the M4c `FlightSfx`
//! cue channel finally drives synthesized sound (§3.7's deferral).

#![forbid(unsafe_code)]

#[cfg(not(target_arch = "wasm32"))]
pub mod audio;
#[cfg(not(target_arch = "wasm32"))]
pub mod bugreport;
#[cfg(not(target_arch = "wasm32"))]
pub mod logring;
pub mod settings_io;
#[cfg(not(target_arch = "wasm32"))]
mod synth;
mod warp;

use std::sync::Arc;

use csp_core::SimError;
use std::collections::BTreeMap;

use csp_render::Gpu;
use csp_render::flight::FlightRenderer;
use csp_render::map::MapRenderer;
use csp_render::terrain::TerrainRenderer;
use csp_render::vab::VabRenderer;
use csp_view::flight::FlightEvent;
use csp_view::map::MapKeyEvent;
use csp_view::settings::{KeyAction, Settings, UI_SCALE_MAX, UI_SCALE_MIN};
use csp_view::vab::VabEvent;
use csp_view::{EventClass, ModeSelect, SimCommand, SourceClass, View, ViewMode};
use warp::WarpController;
use web_time::Instant;
use winit::{
    application::ApplicationHandler,
    event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoop},
    window::{Window, WindowId},
};

/// Max wall seconds a frame may contribute to sim time — clamps the
/// spiral-of-death case when the app stalls (§2.5 `min(dt, 100ms)`).
const MAX_FRAME_DT: f64 = 0.1;

/// `source_id` for the warp controller's commands (§2.2.1). The controller
/// is the only `Ui`-class producer at M0; `SetWarp` is enqueued before
/// `AdvanceTo` each frame so per-source seq order applies the level first.
const WARP_SOURCE_ID: u32 = 0;
/// §2.2.1 source id for the map maneuver panel's commands (Ui class;
/// distinct FIFO from the warp controller's).
const MANEUVER_SOURCE_ID: u32 = 2;
/// §2.2.1 source id for settings-applied commands (the difficulty
/// flags' `SetLifeSupportEnabled` at new-game; Ui class, own FIFO).
const SETTINGS_SOURCE_ID: u32 = 3;
// The throttle keys' `SetThrottle` rides `csp_view::PLAYER_SOURCE_ID`
// (id 1) — deliberately shared with the launch path's
// `SpawnPhysicalVessel` so every player-initiated craft command keeps
// issue order in one FIFO; the const lives in csp-view so the two
// producers can never drift on the value.

/// Map camera controls (M1-D, PLAN §5.1 R0): orbit-drag sensitivity is
/// defined inside `csp_view::map` (`MapKeyEvent::Orbit` takes radians) —
/// these are only the winit-side unit conversions.
/// Radians of camera yaw/pitch per pixel of left-drag.
const DRAG_RAD_PER_PX: f64 = 0.005;
/// `ln` of the zoom factor per `MouseWheel::LineDelta` line.
const ZOOM_LN_PER_LINE: f64 = 0.18;
/// `ln` of the zoom factor per `MouseWheel::PixelDelta` pixel.
const ZOOM_LN_PER_PX: f64 = 0.003;
/// Screen px of pan per WASD/arrow key event (winit auto-repeat keeps it
/// moving while held).
const PAN_STEP_PX: f64 = 80.0;
/// Throttle fraction per `ThrottleUp`/`ThrottleDown` key event (the
/// KSP-style Shift/Ctrl fine control — `accepts_repeat` steps it while
/// held at the OS repeat rate).
const THROTTLE_STEP: f64 = 0.1;
/// Commanded body-frame angular rate per held attitude key, rad/s —
/// the `SetAttitude` control stand-in's "stick deflection" (≈46°/s).
const ATTITUDE_RATE_RAD_S: f64 = 0.8;
/// Max cursor travel (px) between LMB down/up that still counts as a
/// click — anything further is a camera orbit drag and must not also
/// fire a VAB pick.
const CLICK_MAX_DRAG_PX: f64 = 4.0;

struct App {
    window: Option<Arc<Window>>,
    gpu: Option<Gpu>,
    egui_state: Option<egui_winit::State>,
    egui_renderer: Option<egui_wgpu::Renderer>,
    /// The map pass's renderer — pipelines + grow-only vertex buffers
    /// (csp_render::map). `None` until GPU init.
    flight_renderer: Option<FlightRenderer>,
    /// The R2 surface pass (csp_render::terrain): quadtree terrain,
    /// LUT-scattered sky, ocean — drawn into the flight view before
    /// the craft vertices. `None` until GPU init.
    terrain_renderer: Option<TerrainRenderer>,
    map_renderer: Option<MapRenderer>,
    /// The VAB pass's renderer — craft boxes + the offscreen object-id
    /// pick target (csp_render::vab). `None` until GPU init.
    vab_renderer: Option<VabRenderer>,
    /// Last frame's VAB packet — kept so a click can re-rasterize the
    /// same geometry into the pick target.
    last_vab_packet: Option<csp_render::vab::VabPacket>,
    /// A click awaiting pick-pass encode — physical px.
    pending_pick: Option<(u32, u32)>,
    /// Pick readback channel + in-flight flag (one pick at a time —
    /// the texture's single pixel is a serial resource).
    pick_tx: std::sync::mpsc::Sender<u32>,
    pick_rx: std::sync::mpsc::Receiver<u32>,
    pick_in_flight: bool,
    /// The `pick_ids` table snapshotted when the in-flight pick was
    /// rasterized — decode must use THIS, not `last_vab_packet`: the
    /// packet may be replaced between the encode submission and the
    /// map_async landing, and a stale table would silently mis-decode
    /// (selecting the wrong part).
    pick_decode_ids: Option<Vec<u64>>,
    /// Craft-file store: name → `CSPCRAFT` bytes. Memory-first; on
    /// native the store is mirrored to `crafts/*.cspcraft` files under
    /// [`Self::craft_dir`]. The §6.3 wasm demo refuses craft IO entirely
    /// (`demo` gates the outbox; the session's `file_io` gate is deeper
    /// still) — the store exists but stays empty there.
    craft_store: BTreeMap<String, Vec<u8>>,
    /// The directory `craft_store` mirrors to on native — `crafts/`
    /// beside the CWD in production, injected in tests so a test's
    /// `craft_save` outbox never writes into the repo (the
    /// `settings_path` precedent). Read on every target — the wasm
    /// `save_craft_file` ignores it.
    craft_dir: std::path::PathBuf,
    /// Left-drag state for map camera orbit — pointer position at the
    /// last `CursorMoved`, and whether the left button is held.
    last_cursor: Option<(f64, f64)>,
    map_dragging: bool,
    /// Cursor position at the last unconsumed LMB press — a candidate
    /// VAB click. A release within `CLICK_MAX_DRAG_PX` of it becomes a
    /// pick request; a release past it was an orbit drag and does not
    /// select.
    lmb_press: Option<(f64, f64)>,
    /// The physical key a rebind capture just consumed — that key's
    /// events are swallowed until its release. Without this a held
    /// key's repeats would resolve through the *live* keymap (the
    /// capture only wrote the staged one, pending Apply) and an
    /// `accepts_repeat` action could machine-gun the old binding.
    ///
    /// This is the *physical* key (the scancode), not the logical
    /// token, because the token is modifier-dependent: a Shift+1
    /// capture records "!" but, once Shift is already up, releases
    /// as "1" — a token match would never clear and would swallow
    /// every future "!" press until a focus loss reset it.
    captured_key: Option<winit::keyboard::PhysicalKey>,
    /// Attitude actions currently held (flight mode) — the set is the
    /// angular-rate request [`Self::apply_attitude`] folds into one
    /// `SetAttitude` command per change. Populated on key press,
    /// depopulated on release; a focus loss clears it (the missed
    /// release could never arrive) and re-issues a zero-rate hold.
    attitude_held: std::collections::BTreeSet<KeyAction>,
    view: View,
    /// §2.6.3's controller — owns the warp ladder and the integer tick
    /// target; all wall-side, emitting only logged commands.
    warp: WarpController,
    last_frame: Instant,
    /// Per-tick wall ms of the last `step_frame` — the frame's whole step
    /// cost divided by the ticks it advanced (`View::ticks_last_frame`),
    /// because §2.6.3's inequality is written in per-tick cost. Never sim
    /// state.
    last_tick_ms: f64,
    /// Set when the sim reports a §2.13 `Fatal`: ticking halts, the
    /// session marks itself read-only, the bug-report bundle is written,
    /// and the crash dialog surfaces it. (M0's stub — "stop advancing
    /// and show the error" — is now the full §2.13 policy.)
    fatal: Option<SimError>,
    /// The live settings — loaded at startup ([`settings_io`]), edited
    /// through the settings panel's staged copy, persisted on Apply.
    /// The input layer resolves every key press through `settings.keys`.
    settings: Settings,
    /// §2.13's read-only mark: set by the `Fatal` path — a halted
    /// session refuses craft-file writes too (they would persist
    /// artifacts of a world that may be corrupt).
    read_only: bool,
    /// Where `settings.ron` loads from and where Apply persists to —
    /// `Some(settings_io::settings_path())` in production, `None` on wasm
    /// (in-memory settings — never read there, hence the dead_code
    /// allowance on that target). Kept injectable for test hermeticity:
    /// a developer's real `$XDG_CONFIG_HOME/csp/settings.ron` must never
    /// leak into a test — a `dormant_stress: false` file would silently
    /// rewrite the world under test through `apply_startup_difficulty`.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    settings_path: Option<std::path::PathBuf>,
    /// Where bug-report bundles land — `bugreport::bug_report_root()`
    /// normally; tests redirect it. Native only (the bundle is a
    /// filesystem artifact).
    #[cfg(not(target_arch = "wasm32"))]
    bug_report_root: std::path::PathBuf,
    /// `(adapter, backend)` remembered for the bundle's system info —
    /// `set_gpu_info` already hands them to the session mirror.
    gpu_info: Option<(String, String)>,
    /// §6.3: `true` on the web-demo build — craft-file import/export is
    /// refused at the outbox (the VAB session's own `file_io` gate is
    /// the deeper layer; this is the app's half).
    demo: bool,
    /// wasm only: the deferred GPU init channel — `init_graphics` spawns
    /// `Gpu::new` rather than `pollster::block_on`ing it (the web
    /// adapter/device requests resolve through browser promises that
    /// need the JS event loop to turn), and `poll_gpu_init` lands the
    /// result here a few frames later.
    #[cfg(target_arch = "wasm32")]
    gpu_rx: Option<std::sync::mpsc::Receiver<Result<Gpu, csp_render::GpuError>>>,
    /// The M10 audio backend ([`audio`]) — `Some` once `init_graphics`
    /// opened the OS device, `None` on any device failure (graceful
    /// silence) and in tests (which never run `init_graphics`).
    /// Presentation only: it reads `FlightSfx`/`FlightSnapshot`
    /// post-`end_frame` and feeds nothing back.
    #[cfg(not(target_arch = "wasm32"))]
    audio: Option<audio::AudioBackend>,
}

impl App {
    // §8.3/§2.13: `View::new` loads `data/` (balance.ron + the home
    // system) — a missing or invalid file is a startup failure, surfaced
    // loudly at construction. Presentation init is the app's one allowed
    // `expect` site per the workspace lint doc.
    #[allow(clippy::expect_used)]
    fn new() -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        let (settings, settings_path) = (
            settings_io::load_settings(),
            Some(settings_io::settings_path()),
        );
        #[cfg(target_arch = "wasm32")]
        let (settings, settings_path) = (settings_io::load_settings(), None);
        Self::new_inner(settings, settings_path, std::path::PathBuf::from("crafts"))
    }

    /// The settings-path-injecting constructor — what tests drive so the
    /// real `$XDG_CONFIG_HOME/csp/settings.ron` can never leak into a
    /// test process (see the `settings_path` field's doc). Production
    /// goes through `new()` → `settings_io::settings_path()`.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    fn new_with_settings_path(settings_path: &std::path::Path) -> Self {
        Self::new_inner(
            settings_io::load_settings_from(settings_path),
            Some(settings_path.to_path_buf()),
            settings_path.with_file_name("crafts"),
        )
    }

    /// The §6.3 demo build for tests — `View::new_demo` through the same
    /// [`Self::assemble`] the production constructor uses.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    #[allow(clippy::expect_used)]
    fn new_demo_with_settings_path(settings_path: &std::path::Path) -> Self {
        Self::assemble(
            View::new_demo().expect("demo data load failed"),
            settings_io::load_settings_from(settings_path),
            Some(settings_path.to_path_buf()),
            settings_path.with_file_name("crafts"),
        )
    }

    #[allow(clippy::expect_used)]
    fn new_inner(
        settings: Settings,
        settings_path: Option<std::path::PathBuf>,
        craft_dir: std::path::PathBuf,
    ) -> Self {
        let mut view = View::new().expect("data/ startup load failed");
        // M10 first run: seed the stock pad craft so the app opens on a
        // real flight scene — a Flight view with nothing InBubble draws
        // nothing but the clear color (the shipped "purple void"). The
        // same seed `View::new_demo` bakes in; on wasm `View::new` IS
        // `new_demo` — `is_demo()` short-circuits this branch there.
        if !view.is_demo() {
            view.seed_starter_craft()
                .expect("starter craft seed failed");
        }
        Self::assemble(view, settings, settings_path, craft_dir)
    }

    /// The `View`-injecting half of construction — the demo build and
    /// the demo-mode tests route `View::new_demo()` through the same
    /// assembly (settings staging, difficulty flags, live settings).
    fn assemble(
        view: View,
        mut settings: Settings,
        settings_path: Option<std::path::PathBuf>,
        craft_dir: std::path::PathBuf,
    ) -> Self {
        let (pick_tx, pick_rx) = std::sync::mpsc::channel();
        let mut view = view;
        let demo = view.is_demo();
        // Settings-file migration: pre-`key-*` files bound the
        // movement/throttle cluster by typed *character* ("w", "z") —
        // layout-dependent tokens that leave AZERTY controls on the
        // wrong keys. Migrate every character with a positional
        // equivalent onto its `key-*` token (sanitize re-runs to
        // absorb collisions the rewrite can produce), and say so —
        // a silent remap is still a moved binding. Characters with no
        // positional equivalent can't migrate; they get the honest
        // "layout-dependent bindings exist" notice instead.
        let migrated = settings_io::migrate_char_bindings(&mut settings.keys);
        if migrated > 0 {
            let dropped = settings.keys.sanitize();
            log::warn!(
                "settings: migrated {migrated} layout-dependent character binding(s) to positional key-* tokens ({dropped} collision(s) dropped)"
            );
            view.flight_notice(format!(
                "SETTINGS — {migrated} KEY BINDING(S) MIGRATED TO POSITIONAL KEYS"
            ));
            view.set_settings_notice(Some(format!(
                "migrated {migrated} layout-dependent character binding(s) to positional keys"
            )));
        }
        let layout_bound = settings_io::layout_dependent_binding_count(&settings.keys);
        if layout_bound > 0 {
            log::warn!(
                "settings: {layout_bound} key binding(s) use characters with no positional equivalent — layout-dependent"
            );
            view.set_settings_notice(Some(format!(
                "{layout_bound} key binding(s) use layout-dependent characters — rebind them or Reset to defaults"
            )));
        }
        view.stage_settings(&settings);
        // The live keymap mirror — every hint bar and the controls
        // card prints *this*, never the staged edit copy.
        view.set_live_keymap(&settings.keys);
        let mut app = Self {
            window: None,
            gpu: None,
            egui_state: None,
            egui_renderer: None,
            flight_renderer: None,
            terrain_renderer: None,
            map_renderer: None,
            vab_renderer: None,
            last_vab_packet: None,
            pending_pick: None,
            pick_tx,
            pick_rx,
            pick_in_flight: false,
            pick_decode_ids: None,
            craft_store: load_craft_store(&craft_dir),
            craft_dir,
            last_cursor: None,
            map_dragging: false,
            lmb_press: None,
            captured_key: None,
            attitude_held: std::collections::BTreeSet::new(),
            view,
            warp: WarpController::new(),
            last_frame: Instant::now(),
            last_tick_ms: 0.0,
            fatal: None,
            settings,
            settings_path,
            read_only: false,
            #[cfg(not(target_arch = "wasm32"))]
            bug_report_root: bugreport::bug_report_root(),
            gpu_info: None,
            demo,
            #[cfg(target_arch = "wasm32")]
            gpu_rx: None,
            #[cfg(not(target_arch = "wasm32"))]
            audio: None,
        };
        app.apply_startup_difficulty();
        app.apply_live_settings();
        app
    }

    /// Apply the live-session half of [`Settings`] — everything that is
    /// *not* a new-game flag or a file location: the egui zoom factor
    /// (`ui_scale`) and the warp controller's QoL ceiling
    /// (`max_warp_multiplier`). Called at startup and on the settings
    /// panel's Apply. A cap lowered under the live warp demotes now and
    /// rides the §7.6-logged `SetWarp` channel like every other warp
    /// decision — except in a §2.13 read-only session, where the
    /// command is refused (the dead clock would never drain it).
    fn apply_live_settings(&mut self) {
        let scale = self
            .settings
            .graphics
            .ui_scale
            .clamp(UI_SCALE_MIN, UI_SCALE_MAX);
        self.view.egui_context().set_zoom_factor(scale);
        if let Some(level) = self
            .warp
            .set_cap_multiplier(self.settings.gameplay.max_warp_multiplier)
        {
            if self.read_only {
                // §2.13: a halted session takes no new sim commands —
                // the demotion would queue silently behind a clock
                // that never ticks again. The wall-side cap itself
                // still landed (it is app state, not sim state); only
                // the logged command is refused — same notice
                // convention as the launch/maneuver gates.
                self.view.vab_event(VabEvent::Notice(
                    "session is read-only — warp demotion not applied".to_string(),
                ));
            } else {
                self.view.enqueue_command(
                    SourceClass::Ui,
                    SETTINGS_SOURCE_ID,
                    SimCommand::SetWarp(level),
                );
            }
        }
    }

    /// Persist the live settings — shared by the panel's Apply and the
    /// close path (which is how a resized window's new size reaches the
    /// file). Native only; wasm settings are session-memory.
    #[cfg(not(target_arch = "wasm32"))]
    fn persist_settings(&self) {
        let result = match &self.settings_path {
            Some(p) => settings_io::save_settings_to(p, &self.settings),
            None => settings_io::save_settings(&self.settings),
        };
        if let Err(e) = result {
            log::warn!("settings persist failed: {e}");
        }
    }

    /// The difficulty flags are *new-game* choices: they write
    /// construction config / a logged command before the first
    /// `step_frame`, never mid-session (a mid-game config write would
    /// diverge from the §7.6 command log — it is not a logged
    /// mutation). `life_support_enabled` rides `SetLifeSupportEnabled`;
    /// `dormant_stress: false` scales the §3.4 dormant hazard out via
    /// `set_reliability_config` — a **construction-config write**, the
    /// same channel `View::new`'s balance.ron `set_*_config` installs
    /// use (serialized in the save, hashed by §7.1, but *not* a
    /// `SimCommand` and never in the command log — replay determinism
    /// rides on the snapshot carrying it).
    fn apply_startup_difficulty(&mut self) {
        if self.settings.difficulty.life_support_enabled {
            self.view.enqueue_command(
                SourceClass::Ui,
                SETTINGS_SOURCE_ID,
                SimCommand::SetLifeSupportEnabled { enabled: true },
            );
        }
        if !self.settings.difficulty.dormant_stress {
            let mut cfg = self.view.sim.reliability_cfg().clone();
            cfg.dormant_stress_scale = 0.0;
            self.view.sim.set_reliability_config(cfg);
        }
    }

    /// §2.13's `Fatal` policy (M10): halt the clock, mark the session
    /// read-only, write the §2.14 bug-report bundle, and surface it —
    /// the world has already rolled back to the pre-tick shadow inside
    /// `step_frame`, so the bundle's snapshot is the last good state.
    /// Every step is best-effort: a failed bundle write must never
    /// compound the halt with a second failure.
    fn on_sim_fatal(&mut self, e: SimError) {
        self.fatal = Some(e);
        self.read_only = true;
        // Refresh the halt mirror so UiBuild sees it this frame.
        self.view.set_fatal(self.fatal);
        log::error!("sim halted ({e}) — session is read-only");
        #[cfg(not(target_arch = "wasm32"))]
        {
            let input = self.collect_bug_report("fatal sim error", &e.to_string());
            let bundle = match bugreport::write_bug_report(&self.bug_report_root, "fatal", &input) {
                Ok(dir) => {
                    log::error!("bug-report bundle: {}", dir.display());
                    dir.display().to_string()
                }
                Err(err) => {
                    log::error!("bug-report bundle write failed: {err}");
                    format!("bundle write failed: {err}")
                }
            };
            self.view.set_fatal_bundle(Some(bundle));
        }
    }

    /// Collect the §2.14 bundle's sim-side contents: the save (encoded
    /// at the current schema — the bundle doubles as the crash save),
    /// the command log, the data-set hash, and the log tail. A refused
    /// snapshot is recorded, not fatal to the bundle.
    #[cfg(not(target_arch = "wasm32"))]
    fn collect_bug_report(&self, title: &'static str, error: &str) -> bugreport::BugReportInput {
        let save = self
            .view
            .sim
            .snapshot()
            .map_err(|e| format!("snapshot refused: {e}"))
            .and_then(|snapshot| {
                csp_view::save::encode_save(&csp_view::save::Save {
                    game_version: env!("CARGO_PKG_VERSION").into(),
                    label: Some(format!("crash at tick {}", snapshot.clock_ticks)),
                    data_set_hash: csp_view::data_set_hash(&csp_view::data_root()).ok(),
                    snapshot,
                })
                .map_err(|e| e.to_string())
            });
        let command_log = csp_view::save::encode_command_log(self.view.sim.command_log())
            .map_err(|e| e.to_string());
        bugreport::BugReportInput {
            title,
            error: error.to_string(),
            tick: Some(self.view.sim.clock().ticks()),
            save,
            command_log,
            data_set_hash: csp_view::data_set_hash(&csp_view::data_root()).ok(),
            state_hash: self.view.sim.state_hash().ok(),
            gpu_adapter: self.gpu_info.as_ref().map(|(a, _)| a.clone()),
            gpu_backend: self.gpu_info.as_ref().map(|(_, b)| b.clone()),
            log_tail: logring::tail(),
        }
    }

    // §2.13's no-panic rule covers the determinism domain; window/GPU init
    // is unrecoverable presentation setup — there is nothing to fall back
    // to, so `expect` is the honest failure mode (allowed at fn scope).
    #[allow(clippy::expect_used)]
    fn create_window(&self, event_loop: &ActiveEventLoop) -> Arc<Window> {
        // The persisted window size from settings (physical px —
        // `Resized` feeds `inner_size` straight back into it). A
        // hand-edited file with a degenerate size floors at 1 px.
        let [win_w, win_h] = self.settings.graphics.window_size;
        let attrs = Window::default_attributes()
            .with_title("Clanker Space Program")
            .with_inner_size(winit::dpi::PhysicalSize::new(win_w.max(1), win_h.max(1)));

        #[cfg(target_arch = "wasm32")]
        let attrs = {
            use winit::platform::web::WindowAttributesExtWebSys;
            attrs.with_append(true)
        };

        Arc::new(
            event_loop
                .create_window(attrs)
                .expect("failed to create window"),
        )
    }

    /// Kick off window + GPU init. On native this is synchronous —
    /// `pollster::block_on` resolves wgpu's adapter/device requests
    /// in-process. On wasm those requests resolve through *browser
    /// promises* that need the JS event loop to turn, so
    /// `pollster::block_on` inside `resumed` would deadlock: the wasm
    /// path spawns `Gpu::new` on the winit loop instead and
    /// [`Self::poll_gpu_init`] lands the result into
    /// [`Self::finish_gpu_init`] a few event turns later.
    #[allow(clippy::expect_used)]
    fn init_graphics(&mut self, event_loop: &ActiveEventLoop) {
        let window = self.create_window(event_loop);

        #[cfg(not(target_arch = "wasm32"))]
        {
            let size = window.inner_size();
            let gpu = pollster::block_on(Gpu::new(window.clone(), (size.width, size.height)))
                .expect("failed to init GPU");
            self.window = Some(window);
            self.finish_gpu_init(gpu);
        }
        #[cfg(target_arch = "wasm32")]
        {
            let (tx, rx) = std::sync::mpsc::channel();
            self.gpu_rx = Some(rx);
            self.window = Some(window.clone());
            wasm_bindgen_futures::spawn_local(async move {
                let size = window.inner_size();
                let _ = tx.send(Gpu::new(window, (size.width, size.height)).await);
            });
        }
    }

    /// wasm only: land the spawned GPU init — success finishes the
    /// presentation setup, failure logs once and stays black (the demo
    /// keeps running its headless frames; there is no GPU to recover).
    #[cfg(target_arch = "wasm32")]
    fn poll_gpu_init(&mut self) {
        let Some(rx) = &self.gpu_rx else {
            return;
        };
        match rx.try_recv() {
            Ok(Ok(gpu)) => {
                self.gpu_rx = None;
                self.finish_gpu_init(gpu);
            }
            Ok(Err(e)) => {
                self.gpu_rx = None;
                log::error!("GPU init failed: {e}");
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.gpu_rx = None;
                log::error!("GPU init task was dropped");
            }
        }
    }

    /// The post-GPU half of init — egui state, the mode renderers, the
    /// adapter mirror, audio, the craft-file list — shared by the
    /// native synchronous path and the wasm deferred path. `self.window`
    /// is already set by [`Self::init_graphics`].
    fn finish_gpu_init(&mut self, gpu: Gpu) {
        let Some(window) = self.window.clone() else {
            return;
        };

        // The egui context lives in view.world as a resource — the UiBuild
        // system runs the whole pass against it (§2.5); csp-app only feeds
        // raw input in and paints the FullOutput out.
        let egui_state = egui_winit::State::new(
            self.view.egui_context(),
            egui::ViewportId::ROOT,
            &window,
            Some(window.scale_factor() as f32),
            None,
            None,
        );
        let egui_renderer = egui_wgpu::Renderer::new(
            &gpu.device,
            gpu.surface_format(),
            egui_wgpu::RendererOptions::default(),
        );
        let map_renderer = MapRenderer::new(&gpu.device, gpu.surface_format());
        let vab_renderer = VabRenderer::new(
            &gpu.device,
            gpu.surface_format(),
            (gpu.config.width, gpu.config.height),
        );
        let flight_renderer = FlightRenderer::new(
            &gpu.device,
            gpu.surface_format(),
            (gpu.config.width, gpu.config.height),
        );
        let terrain_renderer = TerrainRenderer::new(&gpu.device, gpu.surface_format());
        let info = gpu.adapter.get_info();
        self.gpu_info = Some((info.name.clone(), format!("{:?}", info.backend)));
        self.view
            .set_gpu_info(info.name.clone(), format!("{:?}", info.backend));

        self.gpu = Some(gpu);
        self.egui_state = Some(egui_state);
        self.egui_renderer = Some(egui_renderer);
        self.flight_renderer = Some(flight_renderer);
        self.terrain_renderer = Some(terrain_renderer);
        self.map_renderer = Some(map_renderer);
        self.vab_renderer = Some(vab_renderer);
        // M10 audio (native only): open the device beside the other
        // presentation init — `None` on any failure is the supported
        // silent path, never a startup error. Tests skip
        // `init_graphics`, so they never touch a device.
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.audio = audio::AudioBackend::open(&self.settings.audio);
        }
        self.view
            .set_craft_files(self.craft_store.keys().cloned().collect());
    }

    /// The §2.5 frame loop — see the module doc for the shape. Runs on every
    /// `RedrawRequested`; egui's pass happens inside `view.end_frame()`'s
    /// `UiBuild` stage, not here.
    fn render(&mut self) {
        // Wall clock — read only here, never reaching the sim (§2.5).
        let now = Instant::now();
        let dt_wall = now
            .duration_since(self.last_frame)
            .as_secs_f64()
            .min(MAX_FRAME_DT);
        self.last_frame = now;

        let window = match &self.window {
            Some(w) => w.clone(),
            None => return,
        };

        self.run_frame(&window, dt_wall);
        self.paint(&window);
        window.request_redraw();
    }

    /// The schedule half of the frame (§2.5): controller → commands →
    /// Input → SimStep → Sync → Presentation → UiBuild → RenderExtract.
    /// Ends with a complete `egui::FullOutput` staged for [`Self::paint`].
    #[allow(clippy::too_many_lines)]
    fn run_frame(&mut self, window: &Window, dt_wall: f64) {
        // UiBuild → next frame's Input (§2.5's accepted one-frame latency):
        // fold last frame's panel requests into the controller.
        let outbox = self.view.take_ui_outbox();
        self.handle_ui_outbox(outbox);
        // GPU pick readback: a completed map_async delivers the object
        // id — decode through the id table snapshotted at encode time
        // (`last_vab_packet` may have been replaced between rasterize
        // and this landing) and route the selection through the Input
        // schedule.
        while let Ok(pid) = self.pick_rx.try_recv() {
            self.pick_in_flight = false;
            let node = pid
                .checked_sub(1)
                .and_then(|i| self.pick_decode_ids.as_ref()?.get(i as usize).copied());
            self.view.vab_event(VabEvent::Pick(node));
        }

        // §2.6.3: observe wall cost → decide → log the level + target as
        // commands. Skipped entirely while halted — a dead sim drains no
        // queue.
        let target = if self.fatal.is_none() {
            self.warp.observe(self.last_tick_ms, dt_wall);
            if let Some(level) = self.warp.decide() {
                self.view.enqueue_command(
                    SourceClass::Ui,
                    WARP_SOURCE_ID,
                    SimCommand::SetWarp(level),
                );
            }
            let target = self
                .warp
                .target_tick(self.view.sim.clock().ticks(), dt_wall);
            // §7.6: the frame target rides the log as AdvanceTo, so the
            // command log is the self-contained replay artifact — a replay
            // driver reads every advance target back out of the log (see
            // csp_bench::replay). The step_frame argument below drives
            // only this frame.
            self.view.enqueue_command(
                SourceClass::Ui,
                WARP_SOURCE_ID,
                SimCommand::AdvanceTo(target),
            );
            Some(target)
        } else {
            None
        };

        // Input plumbing → resources the schedules consume. `take_egui_input`
        // is the winit→egui bridge; the egui pass itself runs inside UiBuild.
        if let Some(egui_state) = &mut self.egui_state {
            self.view.set_egui_input(egui_state.take_egui_input(window));
        }
        self.view.update_session(
            dt_wall * 1000.0,
            self.warp.requested_multiplier(),
            self.warp.multiplier(),
            self.fatal,
        );
        // The cameras' projections need the live framebuffer size —
        // re-fed every frame from `gpu.config` so a `Resized` swallowed
        // by the deferred wasm GPU init (or a backend that never sends
        // one) can't wedge an aspect.
        if let Some(gpu) = &self.gpu {
            self.view
                .set_map_viewport(f64::from(gpu.config.width), f64::from(gpu.config.height));
            self.view
                .set_vab_viewport(f64::from(gpu.config.width), f64::from(gpu.config.height));
            self.view
                .set_flight_viewport(f64::from(gpu.config.width), f64::from(gpu.config.height));
        }

        // §2.5 order: Input → SimStep → Sync → Presentation → UiBuild →
        // RenderExtract. One sim_step call per frame, always.
        self.view.begin_frame();
        if let Some(target) = target {
            let step_start = Instant::now();
            match self.view.sim_step(target) {
                Ok(report) => {
                    // §2.6.5: a fired Clamping event demotes warp — now,
                    // before the next frame's step — so the player cannot
                    // overshoot (PlayerAlarm, tracked-vessel SoiTransition).
                    // The demotion itself is a logged SetWarp; re-promotion
                    // still runs the §2.6.3 hysteresis.
                    let clamping = report
                        .fired_events
                        .iter()
                        .any(|e| e.class == EventClass::Clamping);
                    // §2.2.1/§2.13's honest channels: rejected commands and
                    // degraded events surface as log lines — they are the
                    // app's view of Recoverable, never a halt.
                    for rej in &report.rejected {
                        log::warn!(
                            "command rejected on originating channel {:?}: {}",
                            rej.key,
                            rej.error
                        );
                    }
                    for e in &report.degraded {
                        log::warn!("event degraded (fallback applied): {e}");
                    }
                    // §2.6.2's rails-warp refusal seam: the sim-side
                    // predicate, checked here every frame on post-step
                    // state — the only place it can be. Honest about the
                    // one-frame lag: the predicate reads state THIS
                    // frame's step produced, and the demote is a queued
                    // SetWarp that lands at the next frame's boundary —
                    // the frame the refusal appeared in already ran at
                    // the old level. Bounded, not compounding: the
                    // refusal conditions (thrust, contact, atmosphere)
                    // persist until commanded away, so the predicate
                    // re-reports every frame until the state changes.
                    let refusal = self.view.sim.rails_warp_refusal();
                    if (clamping || refusal.is_some())
                        && let Some(level) = self.warp.demote(0)
                    {
                        // §2.6.2: the refusal names the offending vessel.
                        if let Some(reason) = &refusal {
                            log::warn!("rails warp refused ({reason}) — demoting to ×{level}");
                        }
                        self.view.enqueue_command(
                            SourceClass::Ui,
                            WARP_SOURCE_ID,
                            SimCommand::SetWarp(level),
                        );
                    }
                }
                Err(e @ SimError::Fatal(_)) => {
                    // §2.13: only Fatal halts — step_frame keeps
                    // Recoverable inside its report channels. The full
                    // policy (read-only + bundle + dialog) is
                    // `on_sim_fatal`.
                    self.on_sim_fatal(e);
                }
                Err(e) => {
                    // Contract drift: a non-Fatal error escaped step_frame.
                    // §2.13's rule is "bounded failures degrade, not halt"
                    // — log it loud and keep running.
                    log::error!("non-fatal error escaped step_frame: {e}");
                }
            }
            // §2.6.3's inequality is per-tick cost: divide the frame's
            // whole step_frame wall cost by the ticks it advanced.
            // A zero-advance frame reports its cost whole (`max(1)`) —
            // mild overestimate, which demotes early, never late.
            self.last_tick_ms = step_start.elapsed().as_secs_f64() * 1000.0
                / self.view.ticks_last_frame().max(1) as f64;
        } else {
            self.last_tick_ms = 0.0;
        }
        self.view.end_frame();

        // M10 audio (native only): the `FlightSfx` cue queue and the
        // flight snapshot are fresh post-`end_frame` — drain new cues
        // into one-shots and re-drive the continuous voices. Read-only
        // on the presentation world; nothing feeds back.
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(backend) = &mut self.audio {
            backend.update(
                self.view.world.resource::<csp_view::flight::FlightSfx>(),
                self.view
                    .world
                    .resource::<csp_view::flight::FlightSnapshot>(),
                &self.settings.audio,
            );
        }
    }

    /// The session-side acknowledge blip — an app event (settings
    /// applied, mode switched), not a sim transition, so it never
    /// appears in the `FlightSfx` stream; the backend fires it
    /// directly. Silent (no-op) without a device.
    #[cfg(not(target_arch = "wasm32"))]
    fn ui_confirm(&self) {
        if let Some(backend) = &self.audio {
            backend.ui_confirm();
        }
    }

    /// wasm: no audio backend — the blip is a no-op. Keeping the
    /// method defined lets every call site stay unconditional and
    /// symmetric with native.
    #[cfg(target_arch = "wasm32")]
    fn ui_confirm(&self) {}

    /// The one-frame-late half of the frame (§2.5): apply every request
    /// the last `UiBuild` pass wrote into its outbox — mode changes,
    /// craft file ops, the settings panel's Apply, node commands.
    /// Extracted from `run_frame` under the complexity budget.
    fn handle_ui_outbox(&mut self, outbox: csp_view::UiOutbox) {
        if outbox.warp_step != 0 {
            self.warp.request_step(outbox.warp_step);
        }
        if let Some(mode) = outbox.mode {
            self.ui_confirm();
            self.view.set_mode(match mode {
                ModeSelect::Flight => ViewMode::Flight,
                ModeSelect::Map => ViewMode::Map,
                ModeSelect::Vab => ViewMode::Vab,
            });
        }
        if let Some((name, bytes)) = outbox.craft_save {
            if self.demo {
                // §6.3: the demo build does not persist or export craft
                // files — the panel hides the row, this is the app's
                // enforcement half.
                self.view.vab_event(VabEvent::Notice(
                    "craft save is off in the web demo".to_string(),
                ));
            } else if self.read_only {
                // §2.13: a halted session is read-only — a craft file
                // written from a possibly-corrupt world is an artifact
                // nobody should trust.
                self.view.vab_event(VabEvent::Notice(format!(
                    "session is read-only — '{name}' not saved"
                )));
            } else if let Err(reason) = validate_craft_name(&name) {
                // Reject before either half of the store can diverge:
                // a separator or traversal name would write outside —
                // or below — `craft_dir` while the store key and the
                // file list showed a phantom entry the non-recursive
                // loader can never find.
                self.view.vab_event(VabEvent::Notice(format!(
                    "'{name}' is not a usable craft name ({reason})"
                )));
            } else if let Err(e) = save_craft_file(&self.craft_dir, &name, &bytes) {
                // Honest failure: no store entry, no list update — the
                // craft files row and the disk mirror stay in lockstep.
                self.view
                    .vab_event(VabEvent::Notice(format!("could not save '{name}': {e}")));
            } else {
                // Insert only after the write lands — store, file
                // list, and filesystem agree or none of them moves.
                self.craft_store.insert(name.clone(), bytes);
                self.view
                    .set_craft_files(self.craft_store.keys().cloned().collect());
                self.view
                    .vab_event(VabEvent::Notice(format!("saved {name}")));
            }
        }
        if outbox.settings_apply {
            self.ui_confirm();
            // The panel's staged copy becomes the live set: the keymap
            // dispatch reads `self.settings.keys` next key press, and
            // the file write makes it durable (§7.8) — at the injected
            // path so a test's Apply never writes the real config dir.
            // `window_size` is app-tracked, not panel-edited: the live
            // copy's value wins over the stale staged one.
            let mut applied = self.view.staged_settings();
            applied.graphics.window_size = self.settings.graphics.window_size;
            self.settings = applied;
            self.apply_live_settings();
            // The applied keymap is now live — re-mirror it so the
            // controls card and hint bars name the keys that actually
            // dispatch (they never read the staged copy).
            self.view.set_live_keymap(&self.settings.keys);
            #[cfg(not(target_arch = "wasm32"))]
            let persist = match &self.settings_path {
                Some(p) => settings_io::save_settings_to(p, &self.settings),
                None => settings_io::save_settings(&self.settings),
            };
            #[cfg(target_arch = "wasm32")]
            let persist = settings_io::save_settings(&self.settings);
            match persist {
                // wasm's `save_settings` is an in-memory `Ok(())` — the
                // notice must not claim a file write that never happened.
                Ok(()) => self.view.set_settings_notice(Some(
                    if cfg!(target_arch = "wasm32") {
                        "settings applied — the web demo cannot persist them"
                    } else {
                        "settings saved"
                    }
                    .into(),
                )),
                Err(e) => {
                    log::warn!("settings persist failed: {e}");
                    self.view.set_settings_notice(Some(e));
                }
            }
        }
        if outbox.launch {
            if self.read_only {
                // §2.13: a halted session takes no new sim commands —
                // the spawn would sit undrained forever while
                // `launch_craft` still flips the mode to Flight: an
                // inert-but-misleading switch. (The §6.3 demo keeps
                // launch — its fixed-scenario contract caps parts and
                // refuses craft IO, not flight.)
                self.view.vab_event(VabEvent::Notice(
                    "session is read-only — launch refused".to_string(),
                ));
            } else {
                // A refusal already lands on the VAB notice row inside
                // `launch_craft`; its `Err` exists for callers with no
                // notice audience (the startup seed).
                let _ = self.view.launch_craft();
            }
        }
        // M5 node panel → the §7.6-logged command path.
        if let Some(cmd) = outbox.maneuver {
            if self.read_only {
                // Same §2.13 halt discipline — the command must not
                // queue silently behind a clock that never ticks again.
                self.view.vab_event(VabEvent::Notice(
                    "session is read-only — maneuver not applied".to_string(),
                ));
            } else {
                self.view
                    .enqueue_command(SourceClass::Ui, MANEUVER_SOURCE_ID, cmd);
            }
        }
        if let Some(name) = outbox.craft_load {
            if self.demo {
                // §6.3: no craft-file import either — the VabSession's
                // `file_io` gate would refuse the load anyway; this
                // keeps the honest notice at the app's layer.
                self.view.vab_event(VabEvent::Notice(format!(
                    "craft load is off in the web demo ('{name}')"
                )));
            } else {
                match self.craft_store.get(&name) {
                    Some(b) => self.view.vab_event(VabEvent::LoadCraft(b.clone())),
                    None => self
                        .view
                        .vab_event(VabEvent::Notice(format!("no craft file '{name}'"))),
                }
            }
        }
    }

    /// The paint half of the frame: consume the `UiBuild`-produced
    /// `egui::FullOutput` (platform output, tessellation, textures delta)
    /// and draw it — no egui pass logic here (§2.5).
    #[allow(clippy::too_many_lines)]
    fn paint(&mut self, window: &Window) {
        let Some(mut full_output) = self.view.take_ui_frame() else {
            return;
        };
        if let Some(egui_state) = &mut self.egui_state {
            egui_state.handle_platform_output(window, full_output.platform_output);
        }
        let tris = self
            .view
            .egui_context()
            .tessellate(full_output.shapes, full_output.pixels_per_point);

        // `TexturesDelta` is `#[must_use]` — epaint debug-asserts if one is
        // dropped unapplied — so it is pulled out and fully consumed before
        // any early return below. Uploads need only device+queue; the free
        // list names textures this frame's shapes no longer reference, so
        // handling it up front is safe (egui's post-paint ordering is the
        // conservative form).
        let mut textures_delta = std::mem::take(&mut full_output.textures_delta);
        let (gpu, egui_renderer, map_renderer) = match (
            &mut self.gpu,
            &mut self.egui_renderer,
            &mut self.map_renderer,
        ) {
            (Some(g), Some(r), Some(m)) => (g, r, m),
            _ => {
                // No painter to give the delta to — `clear` is the
                // documented way to drop it intentionally.
                textures_delta.clear();
                return;
            }
        };
        for (id, deltas) in &textures_delta.set {
            for delta in deltas {
                egui_renderer.update_texture(&gpu.device, &gpu.queue, *id, delta);
            }
        }
        for id in &textures_delta.free {
            egui_renderer.free_texture(id);
        }
        textures_delta.clear();

        // The RenderExtract product: the map pass's vertex data for this
        // frame (None in flight mode).
        let map_packet = self.view.take_map_packet();
        let flight_packet = self.view.take_flight_packet();
        match self.view.take_vab_packet() {
            Some(p) => self.last_vab_packet = Some(p),
            None => self.last_vab_packet = None,
        }

        let mut frame = match gpu.begin_frame() {
            csp_render::FrameResult::Ok(f) => f,
            csp_render::FrameResult::Reconfigure => {
                // Surface lost/outdated (e.g. resize race) or an
                // acquire-time validation error — reconfigure and
                // skip; `begin_frame` already logged the cause once.
                let (w, h) = (gpu.config.width, gpu.config.height);
                gpu.resize(w, h);
                return;
            }
            // Transient (timeout/occluded) — `begin_frame` warned once.
            csp_render::FrameResult::Skip => return,
        };

        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [gpu.config.width, gpu.config.height],
            pixels_per_point: full_output.pixels_per_point,
        };
        let cmds = egui_renderer.update_buffers(
            &gpu.device,
            &gpu.queue,
            &mut frame.encoder,
            &tris,
            &screen,
        );

        {
            let mut pass = frame
                .encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("scene"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &frame.view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color {
                                r: 0.02,
                                g: 0.02,
                                b: 0.05,
                                a: 1.0,
                            }),
                            store: wgpu::StoreOp::Store,
                        },
                        depth_slice: None,
                    })],
                    // Depth belongs only to the modes whose pipelines
                    // declare it — the VAB and flight renderers each own a
                    // surface-sized target (reverse-Z: clear 0.0 = far,
                    // `Greater` wins). Map's pipelines carry no depth
                    // state, and wgpu validates pass/pipeline depth
                    // compatibility exactly: a no-depth pipeline in a
                    // depth-attached pass is a validation error, so Map
                    // gets `None` here.
                    depth_stencil_attachment: (match self.view.mode() {
                        ViewMode::Flight => self.flight_renderer.as_ref().map(|v| v.depth_view()),
                        ViewMode::Vab => self.vab_renderer.as_ref().map(|v| v.depth_view()),
                        ViewMode::Map => None,
                    })
                    .map(|view| wgpu::RenderPassDepthStencilAttachment {
                        view,
                        depth_ops: Some(wgpu::Operations {
                            load: wgpu::LoadOp::Clear(0.0),
                            store: wgpu::StoreOp::Store,
                        }),
                        stencil_ops: None,
                    }),
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
            // The map pass draws under the UI: same render pass, before
            // egui's triangles (§5.1 R0 — unlit lines + markers).
            if let Some(packet) = &map_packet {
                map_renderer.draw(&mut pass, &gpu.device, &gpu.queue, packet);
            }
            if let (Some(v), Some(packet)) = (&mut self.vab_renderer, &self.last_vab_packet) {
                v.draw(&mut pass, &gpu.device, &gpu.queue, packet);
            }
            if self.view.mode() == ViewMode::Flight
                && let (Some(f), Some(packet)) = (&mut self.flight_renderer, &flight_packet)
            {
                // R2: the surface pass first — its sky is the backdrop,
                // terrain/ocean write depth the craft tests against.
                if let (Some(t), Some(tp)) = (&mut self.terrain_renderer, &packet.terrain) {
                    t.draw(&mut pass, &gpu.device, &gpu.queue, tp);
                }
                f.draw(&mut pass, &gpu.device, &gpu.queue, packet);
            }
        }

        // egui paints in a second pass of its own: its pipeline declares
        // no depth state (`RendererOptions::default().depth_stencil_format
        // = None`), and wgpu requires pass/pipeline depth formats to match
        // — drawing it inside the scene pass fails validation whenever a
        // depth target is attached. `LoadOp::Load` composites the UI over
        // the scene; no depth attachment.
        {
            let pass = frame
                .encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("ui"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &frame.view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        },
                        depth_slice: None,
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
            egui_renderer.render(&mut pass.forget_lifetime(), &tris, &screen);
            drop(cmds); // user command buffers currently unused
        }

        // A queued pick: rasterize the same packet into the object-id
        // target and copy the clicked pixel to the readback buffer —
        // same encoder submission as the frame.
        let mut encode_pick = false;
        let mut encoded_ids = None;
        if let (Some((x, y)), Some(v), Some(packet)) = (
            self.pending_pick.take(),
            &mut self.vab_renderer,
            &self.last_vab_packet,
        ) {
            if self.pick_in_flight {
                // One outstanding pick — drop the request; the next
                // click retriggers.
            } else {
                encode_pick =
                    v.encode_pick(&mut frame.encoder, &gpu.device, &gpu.queue, packet, x, y);
                if encode_pick {
                    // Snapshot the decode table alongside the rasterized
                    // pixels — the readback decodes a frame or two later
                    // and `last_vab_packet` may have moved on by then.
                    encoded_ids = Some(packet.pick_ids.clone());
                }
            }
        }
        if let Some(ids) = encoded_ids {
            self.pick_decode_ids = Some(ids);
        }
        gpu.submit(*frame);
        if encode_pick {
            if let Some(v) = &self.vab_renderer {
                v.map_pick(self.pick_tx.clone());
            }
            self.pick_in_flight = true;
        }
        // Service map_async callbacks (native; wasm callbacks fire on
        // the event loop).
        if let Err(e) = gpu.device.poll(wgpu::PollType::Poll) {
            log::warn!("device poll failed: {e}");
        }
        window.request_redraw();
    }

    /// One key-press event, resolved through the rebindable keymap
    /// (M10): `key_token` normalizes the winit key, `settings.keys`
    /// names the action, and the dispatch table below routes it — the
    /// M0–M9d hardcoded bindings are now the keymap's *defaults*.
    ///
    /// Rebind capture comes first: while the settings panel is
    /// listening, the next press becomes that action's binding
    /// (Escape cancels). `window_event` routes presses here *before*
    /// egui sees the event, so a widget that would consume the key —
    /// a focused TextEdit — cannot steal the capture and the key never
    /// lands as typed text. OS auto-repeat never counts while a
    /// capture is armed, and the captured key stays swallowed until
    /// its release ([`Self::captured_key`]) so the live keymap's old
    /// binding cannot fire under a held finger before Apply.
    ///
    /// Repeat presses only count for [`KeyAction::accepts_repeat`]
    /// actions (the camera pans), so a held key cannot machine-gun
    /// warp steps or mode toggles.
    fn key_press(&mut self, event: &winit::event::KeyEvent) {
        // Rebind capture — before all other dispatch, swallowing the key.
        if self.view.rebind_target().is_some() {
            if event.repeat {
                // A held key must not count as a second "next press".
                return;
            }
            match settings_io::event_binding(event) {
                None => { /* unbindable key — keep listening */ }
                Some(b) if b.as_str() == "escape" => {
                    self.captured_key = Some(event.physical_key);
                    self.view.set_rebind_target(None);
                }
                Some(b) => {
                    let Some(action) = self.view.rebind_target() else {
                        return;
                    };
                    self.captured_key = Some(event.physical_key);
                    self.view.bind_key(action, b);
                }
            }
            return;
        }
        // Physical-first resolution (§ M10 AZERTY): the `key-*` tokens
        // name the key's *position* — a bound `key-w` fires on the
        // W-position key whether the layout prints `w` or `z` on it.
        // The logical token is the fallback for character bindings a
        // user (or an old settings file) recorded by glyph.
        let action = settings_io::keycode_token(&event.physical_key)
            .and_then(|t| self.settings.keys.action_for(&t))
            .or_else(|| {
                settings_io::key_token(&event.logical_key)
                    .and_then(|t| self.settings.keys.action_for(&t))
            });
        let Some(action) = action else {
            return;
        };
        if event.repeat && !action.accepts_repeat() {
            return;
        }
        self.dispatch(action);
    }

    /// The release-side resolver — the same physical-first order
    /// [`Self::key_press`] uses, so a key that fired an attitude action
    /// releases exactly that action regardless of layout.
    fn action_of(&self, event: &winit::event::KeyEvent) -> Option<KeyAction> {
        settings_io::keycode_token(&event.physical_key)
            .and_then(|t| self.settings.keys.action_for(&t))
            .or_else(|| {
                settings_io::key_token(&event.logical_key)
                    .and_then(|t| self.settings.keys.action_for(&t))
            })
    }

    /// Mark an attitude action held and re-issue the combined rate
    /// command (the `BTreeSet::insert` dedup makes OS key repeats free).
    fn attitude_press(&mut self, action: KeyAction) {
        if self.attitude_held.insert(action) {
            self.apply_attitude();
        }
    }

    /// Clear an attitude action on release and re-issue the combined
    /// rate — an empty set commands a zero angular rate (the stand-in
    /// control authority's "stop rotating" until M3+ real SAS).
    fn attitude_release(&mut self, action: KeyAction) {
        if self.attitude_held.remove(&action) {
            self.apply_attitude();
        }
    }

    /// Fold the held attitude set into one `SetAttitude` command on the
    /// focused vessel's player FIFO — the sim's `SetAttitude` is the
    /// documented M2 control stand-in, so held keys mean a commanded
    /// body-frame angular rate, not a torque model. Axes (§3.7's
    /// convention): nose = body +Z (pitch about +X), dorsal = +Y (yaw
    /// about +Y), roll about +Z. Reads `sim` for the freshest pose —
    /// the command carries an absolute `rot`, and the staged mirror's
    /// one-frame-old pose would visibly snap the craft back.
    fn apply_attitude(&mut self) {
        if self.read_only {
            self.attitude_held.clear();
            return;
        }
        let Some(vessel) = self.view.flight_focus() else {
            return;
        };
        let Some(body) = self.view.sim.vessels().get(&vessel).and_then(|v| v.body()) else {
            // No InBubble body — a surface-latched pad craft or a
            // rails vessel has no attitude surface to command (and
            // refusing keeps an accidental WASD press from unlatching
            // the parked craft).
            return;
        };
        let rate = |neg: KeyAction, pos: KeyAction| -> f64 {
            f64::from(
                self.attitude_held.contains(&pos) as i8 - self.attitude_held.contains(&neg) as i8,
            ) * ATTITUDE_RATE_RAD_S
        };
        // PanUp/Down are the W/S positions — KSP's pitch down/up;
        // PanLeft/Right the A/D yaw; Roll Q/E.
        let ang_vel = csp_core::DVec3::new(
            rate(KeyAction::PanDown, KeyAction::PanUp),
            rate(KeyAction::PanLeft, KeyAction::PanRight),
            rate(KeyAction::RollRight, KeyAction::RollLeft),
        );
        self.view.enqueue_command(
            SourceClass::Ui,
            csp_view::PLAYER_SOURCE_ID,
            SimCommand::SetAttitude {
                vessel,
                rot: body.rot,
                ang_vel,
            },
        );
    }

    /// One `ThrottleUp`/`ThrottleDown` event — steps the focused
    /// vessel's throttle fraction (read live from the sim body; a
    /// latched craft reads 0 — its first step unlatches at
    /// `THROTTLE_STEP`). `accepts_repeat` walks it while held.
    fn throttle_nudge(&mut self, up: bool) {
        if self.read_only {
            self.view.vab_event(VabEvent::Notice(
                "session is read-only — throttle not applied".to_string(),
            ));
            return;
        }
        let Some(vessel) = self.view.flight_focus() else {
            return;
        };
        let current = self
            .view
            .sim
            .vessels()
            .get(&vessel)
            .and_then(|v| v.body())
            .map_or(0.0, |b| b.throttle);
        let step = if up { THROTTLE_STEP } else { -THROTTLE_STEP };
        self.view.enqueue_command(
            SourceClass::Ui,
            csp_view::PLAYER_SOURCE_ID,
            SimCommand::SetThrottle {
                vessel,
                throttle: (current + step).clamp(0.0, 1.0),
            },
        );
    }

    /// The `Stage` key — the view drains the launch-captured
    /// [`csp_view::flight::StagePlan`] into logged `JettisonPart`s.
    fn stage_press(&mut self) {
        if self.read_only {
            self.view.vab_event(VabEvent::Notice(
                "session is read-only — staging not applied".to_string(),
            ));
        } else {
            self.view.fire_stage();
        }
    }

    /// A focus loss can drop the releases that clear a captured key
    /// and held attitude keys — clear both here (the craft gets a
    /// zero-rate hold instead of spinning offscreen forever).
    fn focus_lost(&mut self) {
        self.captured_key = None;
        if !self.attitude_held.is_empty() {
            self.attitude_held.clear();
            self.apply_attitude();
        }
    }

    /// Record the glyph a physical key produced on this layout — the
    /// hint bars show an AZERTY `z` where a QWERTY hint says `w`.
    fn note_key_label(&mut self, event: &WindowEvent) {
        if let WindowEvent::KeyboardInput { event, .. } = event
            && let Some((token, label)) = settings_io::event_label(event)
        {
            self.view.set_key_label(token, label);
        }
    }

    /// The KeyboardInput arm — presses dispatch the action (see
    /// [`Self::key_press`]); releases clear held attitude verbs.
    /// The release is matched on the action (not the key) so a key
    /// the press bound still releases under a rebind.
    fn keyboard_event(&mut self, event: &winit::event::KeyEvent) {
        match event.state {
            ElementState::Pressed => self.key_press(event),
            ElementState::Released => {
                if let Some(action) = self.action_of(event)
                    && action.is_attitude()
                {
                    self.attitude_release(action);
                }
            }
        }
    }

    /// The pre-egui half of key routing (R17-F7): returns `true` when
    /// the event belongs to the rebind-capture machinery and must not
    /// reach egui or the normal dispatch. While the settings panel
    /// listens, the next press is the new binding even when a widget
    /// (a focused TextEdit) would otherwise consume it — and egui must
    /// not see it at all, or a captured 'x' would also land as typed
    /// text. A just-captured key stays swallowed until its release so
    /// its repeats cannot fire the live keymap's old binding (the
    /// capture wrote only the staged map, pending Apply).
    fn intercept_rebind_key(&mut self, event: &winit::event::KeyEvent) -> bool {
        if event.state == ElementState::Pressed && self.view.rebind_target().is_some() {
            self.key_press(event);
            return true;
        }
        // Match on the physical key, not the logical token — the token
        // shifts with modifiers (Shift+1 presses "!", releases "1"),
        // so a token match could miss the release and wedge the
        // captured key. `physical_key` is stable across modifier
        // state.
        let is_captured = self.captured_key.is_some_and(|c| c == event.physical_key);
        if !is_captured {
            return false;
        }
        if event.state == ElementState::Released {
            self.captured_key = None;
            // The release never dispatches to egui or the keymap, but
            // the held-attitude set still clears — the key's press may
            // have inserted its action before the capture armed, and a
            // swallowed release would leave the rate stuck on.
            self.keyboard_event(event);
        }
        true
    }

    /// Route one resolved [`KeyAction`] — mode choreography lives in
    /// `View::set_mode`, camera input rides `View::{map,vab,flight}_event`
    /// into the Input schedule, and sim mutations ride the §7.6-logged
    /// command channel like every other mutation.
    fn dispatch(&mut self, action: KeyAction) {
        let mode = self.view.mode();
        match action {
            // Session toggles acknowledge with the same blip the
            // outbox's UI-driven mode switch and Apply play — a
            // key-driven session event is the same class of event.
            KeyAction::OpenSettings => {
                self.ui_confirm();
                self.view.toggle_settings_panel();
                // Restage on open — the window always edits the
                // applied set; unapplied edits from a previous visit
                // are discarded, not resurrected.
                if self.view.settings_panel_open() {
                    self.view.stage_settings(&self.settings);
                }
            }
            KeyAction::ToggleVab => {
                self.ui_confirm();
                self.view.set_mode(if mode == ViewMode::Vab {
                    ViewMode::Flight
                } else {
                    ViewMode::Vab
                });
            }
            KeyAction::ToggleMap => {
                self.ui_confirm();
                self.view.set_mode(if mode == ViewMode::Map {
                    ViewMode::Flight
                } else {
                    ViewMode::Map
                });
            }
            // §2.6.3: request_step lands in the controller and becomes a
            // logged `SetWarp` on the next decide().
            KeyAction::WarpUp => self.warp.request_step(1),
            KeyAction::WarpDown => self.warp.request_step(-1),
            // M4c flight throttle — the §7.6 command channel. The VAB
            // suppresses it (throttle is a flight verb).
            KeyAction::ThrottleFull | KeyAction::ThrottleCut if mode != ViewMode::Vab => {
                if self.read_only {
                    // §2.13: a halted session takes no new sim commands —
                    // the throttle would pile up undrained behind a
                    // clock that never ticks again. Same refusal notice
                    // the outbox's launch/maneuver gates raise.
                    self.view.vab_event(VabEvent::Notice(
                        "session is read-only — throttle not applied".to_string(),
                    ));
                } else if let Some(vessel) = self.view.flight_focus() {
                    self.view.enqueue_command(
                        SourceClass::Ui,
                        // The shared player-command FIFO — launch's
                        // `SpawnPhysicalVessel` rides it too.
                        csp_view::PLAYER_SOURCE_ID,
                        SimCommand::SetThrottle {
                            vessel,
                            throttle: if action == KeyAction::ThrottleFull {
                                1.0
                            } else {
                                0.0
                            },
                        },
                    );
                }
            }
            // Fine throttle — same §7.6 channel and the same read-only
            // discipline as the full/cut verbs.
            KeyAction::ThrottleUp | KeyAction::ThrottleDown if mode != ViewMode::Vab => {
                self.throttle_nudge(action == KeyAction::ThrottleUp);
            }
            // Space fires the next stage — the view resolves the
            // session-captured drop set into logged `JettisonPart`s.
            KeyAction::Stage if mode == ViewMode::Flight => self.stage_press(),
            KeyAction::CycleFocus if mode == ViewMode::Map => {
                self.view.map_event(MapKeyEvent::CycleFocus)
            }
            // The controls card and the dev debug scaffold — session
            // toggles on the view, no sim involvement; same blip
            // discipline as the other session toggles.
            KeyAction::ToggleHelp => {
                self.ui_confirm();
                self.view.toggle_help_panel();
            }
            KeyAction::ToggleDebug => {
                self.ui_confirm();
                self.view.toggle_debug_panel();
            }
            // WASD positions are mode-split verbs: camera pans on the
            // map/VAB, held-to-turn attitude in flight (with the Q/E
            // roll pair — pure flight verbs, inert elsewhere).
            KeyAction::PanUp
            | KeyAction::PanDown
            | KeyAction::PanLeft
            | KeyAction::PanRight
            | KeyAction::RollLeft
            | KeyAction::RollRight => {
                if mode == ViewMode::Flight {
                    self.attitude_press(action);
                } else if !matches!(action, KeyAction::RollLeft | KeyAction::RollRight) {
                    // Roll keys are flight-only; the pan cluster still
                    // steers the map/VAB cameras per event.
                    let (dx_px, dy_px) = match action {
                        KeyAction::PanUp => (0.0, -PAN_STEP_PX),
                        KeyAction::PanDown => (0.0, PAN_STEP_PX),
                        KeyAction::PanLeft => (-PAN_STEP_PX, 0.0),
                        _ => (PAN_STEP_PX, 0.0),
                    };
                    match mode {
                        ViewMode::Map => self.view.map_event(MapKeyEvent::Pan { dx_px, dy_px }),
                        ViewMode::Vab => self.view.vab_event(VabEvent::Pan { dx_px, dy_px }),
                        ViewMode::Flight => {}
                    }
                }
            }
            _ => {}
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_none() {
            self.init_graphics(event_loop);
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let window = match &self.window {
            Some(w) => w.clone(),
            None => return,
        };

        // Rebind capture intercepts key events BEFORE egui (see
        // `intercept_rebind_key`).
        if let WindowEvent::KeyboardInput { event, .. } = &event
            && self.intercept_rebind_key(event)
        {
            return;
        }
        if matches!(event, WindowEvent::Focused(false)) {
            self.focus_lost();
        }
        self.note_key_label(&event);

        // egui gets first pass at raw input, but `consumed` must guard the
        // individual handlers rather than early-returning: egui swallows
        // the LMB release when the pointer is over a panel, and skipping
        // the whole match would leave `map_dragging` stuck on — the camera
        // would keep orbiting forever after button-up.
        let consumed = self
            .egui_state
            .as_mut()
            .is_some_and(|state| state.on_window_event(&window, &event).consumed);

        match event {
            WindowEvent::CloseRequested => {
                // Persist on close — this is how the tracked window
                // size (and any applied changes) reach the file without
                // a per-resize write storm.
                #[cfg(not(target_arch = "wasm32"))]
                self.persist_settings();
                event_loop.exit();
            }
            WindowEvent::Resized(size) => {
                // Track the live size into settings — persisted on
                // Apply/close, restored at the next `init_graphics`.
                // Minimized/hidden windows can report a zero dimension:
                // skip those so a transient collapse isn't persisted as
                // a 1-px window next launch.
                if size.width > 0 && size.height > 0 {
                    self.settings.graphics.window_size = [size.width, size.height];
                }
                if let Some(gpu) = &mut self.gpu {
                    gpu.resize(size.width, size.height);
                }
                // The renderer resize stays here (its target must match
                // the surface), but the *view* viewport is re-fed per
                // frame in `run_frame` — an event that lands before
                // `finish_gpu_init` no longer loses the aspect.
                if let (Some(g), Some(f)) = (&self.gpu, &mut self.flight_renderer) {
                    f.resize(&g.device, size.width, size.height);
                }
                if let (Some(g), Some(v)) = (&self.gpu, &mut self.vab_renderer) {
                    v.resize(&g.device, size.width, size.height);
                }
            }
            // Key presses and releases. Presses stay `!consumed`-gated
            // so egui-owned keys (a focused text field) never reach the
            // sim — but a RELEASE always runs `keyboard_event`: a
            // release egui swallowed would leak `attitude_held` and the
            // craft would hold its last commanded rate until a focus
            // loss cleared it (the same reason LMB releases clear
            // `map_dragging` unconditionally — consumption gates
            // presses, never releases).
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state == ElementState::Released || !consumed {
                    self.keyboard_event(&event);
                }
            }
            // Map camera mouse input — left-drag orbits, wheel zooms.
            // A consumed PRESS (clicking a panel) must not start a camera
            // drag, so the flag is `!consumed && pressed`. The RELEASE
            // clears unconditionally — that is the point of routing
            // consumption per-handler: a button-up over a panel must
            // still end the drag.
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => {
                self.map_dragging = !consumed && state == ElementState::Pressed;
                match state {
                    ElementState::Pressed => {
                        // VAB pick is decided at RELEASE, not here: an
                        // unconsumed press is only a click *candidate* —
                        // it may be the start of a camera orbit drag,
                        // and a drag must not also select.
                        self.lmb_press = if consumed { None } else { self.last_cursor };
                    }
                    ElementState::Released => {
                        // VAB: an unconsumed click that stayed inside
                        // the drag threshold requests a GPU pick at the
                        // cursor — the readback lands via the pick
                        // channel a frame or two later.
                        if let Some(press) = self.lmb_press.take() {
                            let pos = self.last_cursor.unwrap_or(press);
                            let dragged =
                                (pos.0 - press.0).hypot(pos.1 - press.1) > CLICK_MAX_DRAG_PX;
                            if !consumed && !dragged && self.view.mode() == ViewMode::Vab {
                                self.pending_pick =
                                    Some((pos.0.max(0.0) as u32, pos.1.max(0.0) as u32));
                            }
                        }
                    }
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let pos = (position.x, position.y);
                if !consumed
                    && self.map_dragging
                    && let Some(last) = self.last_cursor
                {
                    let (dyaw, dpitch) = (
                        (pos.0 - last.0) * DRAG_RAD_PER_PX,
                        (pos.1 - last.1) * DRAG_RAD_PER_PX,
                    );
                    match self.view.mode() {
                        ViewMode::Map => self.view.map_event(MapKeyEvent::Orbit { dyaw, dpitch }),
                        ViewMode::Vab => self.view.vab_event(VabEvent::Orbit { dyaw, dpitch }),
                        ViewMode::Flight => {
                            self.view.flight_event(FlightEvent::Orbit { dyaw, dpitch })
                        }
                    }
                }
                // `last_cursor` updates even on egui-consumed moves —
                // otherwise a drag crossing a panel exits with a stale
                // position and the camera takes one large delta jump.
                self.last_cursor = Some(pos);
            }
            WindowEvent::MouseWheel { delta, .. } if !consumed => {
                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => f64::from(y) * ZOOM_LN_PER_LINE,
                    MouseScrollDelta::PixelDelta(p) => p.y * ZOOM_LN_PER_PX,
                };
                let factor = f64::exp(lines);
                match self.view.mode() {
                    ViewMode::Map => self.view.map_event(MapKeyEvent::Zoom { factor }),
                    ViewMode::Vab => self.view.vab_event(VabEvent::Zoom { factor }),
                    ViewMode::Flight => self.view.flight_event(FlightEvent::Zoom { factor }),
                }
            }
            WindowEvent::RedrawRequested => self.render(),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        // wasm: the spawned GPU init resolves on event-loop turns —
        // check the channel here so the first frames don't need a
        // RedrawRequested to land it.
        #[cfg(target_arch = "wasm32")]
        self.poll_gpu_init();
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }
}

/// The §2.14 panic path: install a hook that writes a minimal
/// bug-report bundle (no sim access inside a hook — the bundle is the
/// panic text + system info + log tail) and then chains to the
/// previous hook for the normal stderr report. Best-effort by
/// contract: `write_panic_bundle` swallows its own failures.
#[cfg(not(target_arch = "wasm32"))]
fn install_panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Some(dir) = bugreport::write_panic_bundle(
            &bugreport::bug_report_root(),
            &info.to_string(),
            logring::tail(),
        ) {
            eprintln!("bug-report bundle written to {}", dir.display());
        }
        prev(info);
    }));
}

/// The `csp --bug-report` command (§2.14): write a bundle for a fresh
/// session headlessly and print its path. `Ok(())` → exit 0.
#[cfg(not(target_arch = "wasm32"))]
pub fn bug_report_cli() -> Result<(), String> {
    let view = View::new().map_err(|e| e.to_string())?;
    let root = bugreport::bug_report_root();
    let save = view
        .sim
        .snapshot()
        .map_err(|e| format!("snapshot refused: {e}"))
        .and_then(|snapshot| {
            csp_view::save::encode_save(&csp_view::save::Save {
                game_version: env!("CARGO_PKG_VERSION").into(),
                label: None,
                data_set_hash: csp_view::data_set_hash(&csp_view::data_root()).ok(),
                snapshot,
            })
            .map_err(|e| e.to_string())
        });
    let input = bugreport::BugReportInput {
        title: "manual report",
        error: "requested with --bug-report".into(),
        tick: Some(view.sim.clock().ticks()),
        save,
        command_log: csp_view::save::encode_command_log(view.sim.command_log())
            .map_err(|e| e.to_string()),
        data_set_hash: csp_view::data_set_hash(&csp_view::data_root()).ok(),
        state_hash: view.sim.state_hash().ok(),
        gpu_adapter: None,
        gpu_backend: None,
        log_tail: logring::tail(),
    };
    let dir = bugreport::write_bug_report(&root, "manual", &input)
        .map_err(|e| format!("bundle write failed: {e}"))?;
    println!("bug-report bundle written to {}", dir.display());
    Ok(())
}

/// Native entry point.
// Event-loop creation/run failure is unrecoverable startup — outside the
// determinism domain, `expect` is the honest failure mode (§2.13).
#[cfg(not(target_arch = "wasm32"))]
#[allow(clippy::expect_used)]
pub fn run() {
    install_panic_hook();
    let event_loop = EventLoop::new().expect("failed to create event loop");
    let mut app = App::new();
    event_loop.run_app(&mut app).expect("event loop error");
}

/// wasm entry point — called automatically by wasm-bindgen.
// Event-loop creation failure is unrecoverable startup — outside the
// determinism domain, `expect` is the honest failure mode (§2.13).
#[cfg(target_arch = "wasm32")]
#[allow(clippy::expect_used)]
#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub fn wasm_start() {
    console_error_panic_hook::set_once();
    let _ = console_log::init_with_level(log::Level::Info);

    let event_loop = EventLoop::new().expect("failed to create event loop");
    let app = App::new();
    winit::platform::web::EventLoopExtWebSys::spawn_app(event_loop, app);
}

/// Native: `crafts/` next to the working directory (injected in tests)
/// — session artifacts live outside `data/` (that tree is authored
/// content + the attribution lint). On wasm the store is memory-only.
#[cfg(not(target_arch = "wasm32"))]
fn load_craft_store(dir: &std::path::Path) -> BTreeMap<String, Vec<u8>> {
    let mut store = BTreeMap::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "cspcraft")
                && let (Ok(bytes), Some(name)) = (
                    std::fs::read(&p),
                    p.file_stem().and_then(|s| s.to_str()).map(String::from),
                )
                // A hand-placed file whose stem the save path would
                // refuse stays out of the store too — one name rule
                // for both directions keeps the key set portable.
                && validate_craft_name(&name).is_ok()
            {
                store.insert(name, bytes);
            }
        }
    }
    store
}

/// wasm: in-memory only — the §6.3 demo has no filesystem and refuses
/// craft IO anyway; `_dir` keeps the signature shared.
#[cfg(target_arch = "wasm32")]
fn load_craft_store(_dir: &std::path::Path) -> BTreeMap<String, Vec<u8>> {
    BTreeMap::new()
}

/// A craft name becomes `<name>.cspcraft` under `craft_dir` — it must
/// be one safe filename component, nothing more. Reject:
/// - an empty name (the UI hides it, this is the enforcement half);
/// - `.`/`..`-style names — the traversal class (`dir.join("a/../b")`
///   escapes `craft_dir`; even the degenerate `.` produces a file the
///   flat loader skips);
/// - path separators `/` and `\` (a name containing one writes into a
///   subdirectory or — joined absolute — outside `dir` entirely,
///   leaving a store entry the non-recursive loader can never find);
/// - `:` (a Windows drive prefix like `C:x` also escapes the join);
/// - control characters (legal on unix, broken everywhere else and
///   unprintable in notices).
fn validate_craft_name(name: &str) -> Result<(), &'static str> {
    if name.is_empty() {
        return Err("empty name");
    }
    if name == "." || name == ".." || name.contains("..") {
        return Err("path traversal");
    }
    if name
        .chars()
        .any(|c| c == '/' || c == '\\' || c == ':' || c.is_control())
    {
        return Err("path separator or control character");
    }
    Ok(())
}

/// Native: mirror the store entry to `<dir>/<name>.cspcraft`. The
/// caller validates `name` first ([`validate_craft_name`]), so the
/// join can never escape `dir`; a failed write is returned, not
/// logged, so the caller can refuse the store insert and keep the
/// memory store honest with the disk mirror.
#[cfg(not(target_arch = "wasm32"))]
fn save_craft_file(dir: &std::path::Path, name: &str, bytes: &[u8]) -> Result<(), String> {
    std::fs::create_dir_all(dir)
        .and_then(|()| std::fs::write(dir.join(format!("{name}.cspcraft")), bytes))
        .map_err(|e| format!("{}: {e}", dir.display()))
}

/// wasm: nothing to write — the store in `App` is the whole store.
#[cfg(target_arch = "wasm32")]
fn save_craft_file(_dir: &std::path::Path, _name: &str, _bytes: &[u8]) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
// §2.13's no-panic rule covers shipping fallible paths; tests signal
// failure by panicking, so unwrap/expect stays legal inside test code.
#[allow(clippy::unwrap_used, clippy::expect_used)]
#[cfg(not(target_arch = "wasm32"))]
mod tests {
    use super::*;
    use csp_view::settings::{KeyAction, KeyBinding};

    /// A throwaway directory per test — `std::env::temp_dir` + a unique
    /// name (no tempfile dep).
    fn tempdir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("csp-app-test-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn key_token_maps_characters_and_named_keys() {
        use winit::keyboard::{Key, NamedKey, SmolStr};
        assert_eq!(
            settings_io::key_token(&Key::Character(SmolStr::new("M"))).as_deref(),
            Some("m")
        );
        assert_eq!(
            settings_io::key_token(&Key::Character(SmolStr::new(","))).as_deref(),
            Some(",")
        );
        assert_eq!(
            settings_io::key_token(&Key::Named(NamedKey::ArrowUp)).as_deref(),
            Some("arrow-up")
        );
        assert_eq!(
            settings_io::key_token(&Key::Named(NamedKey::Escape)).as_deref(),
            Some("escape")
        );
        assert_eq!(
            settings_io::key_token(&Key::Named(NamedKey::F5)).as_deref(),
            Some("f5")
        );
        // Every produced token parses into a valid binding.
        assert!(settings_io::binding_for(&Key::Named(NamedKey::Tab)).is_some());
    }

    #[test]
    fn settings_file_round_trips() {
        let dir = tempdir("settings");
        let path = dir.join("settings.ron");
        let mut s = Settings::default();
        s.audio.master_volume = 0.4;
        s.difficulty.dormant_stress = false;
        s.keys
            .bind(KeyAction::WarpUp, KeyBinding::parse("f7").unwrap());
        settings_io::save_settings_to(&path, &s).unwrap();
        assert_eq!(settings_io::load_settings_from(&path), s);
        // Missing file → defaults (the first-run path).
        assert_eq!(
            settings_io::load_settings_from(&dir.join("absent.ron")),
            Settings::default()
        );
        // Corrupt file → defaults + warn, never a crash.
        std::fs::write(&path, "not ron ((").unwrap();
        assert_eq!(settings_io::load_settings_from(&path), Settings::default());
    }

    /// The §2.14 bundle, file by file — the save inside decodes through
    /// the §7.7 codec back to the snapshot that wrote it.
    #[test]
    fn bug_report_bundle_writes_all_artifacts() {
        let dir = tempdir("bundle");
        let snapshot = View::default().sim.snapshot().unwrap();
        let save_bytes = csp_view::save::encode_save(&csp_view::save::Save {
            game_version: "0.0.0".into(),
            label: Some("crash".into()),
            data_set_hash: None,
            snapshot: snapshot.clone(),
        })
        .unwrap();
        let input = bugreport::BugReportInput {
            title: "fatal sim error",
            error: "injected invariant violation (test)".into(),
            tick: Some(snapshot.clock_ticks),
            save: Ok(save_bytes),
            command_log: Ok(csp_view::save::encode_command_log(&[]).unwrap()),
            data_set_hash: Some([0x5Au8; 32]),
            state_hash: Some([0xA5u8; 32]),
            gpu_adapter: Some("test adapter".into()),
            gpu_backend: Some("test backend".into()),
            log_tail: vec!["info  csp: hello".into()],
        };
        let bundle = dir.join("report");
        bugreport::write_bundle_dir(&bundle, &input).unwrap();
        for name in [
            "README.txt",
            "error.txt",
            "system.txt",
            "fingerprint.json",
            "save.cspsave",
            "commands.bin",
            "log.txt",
        ] {
            assert!(bundle.join(name).exists(), "{name} missing");
        }
        let decoded =
            csp_view::save::decode_save(&std::fs::read(bundle.join("save.cspsave")).unwrap())
                .unwrap();
        assert_eq!(decoded.snapshot, snapshot);
        assert_eq!(decoded.label.as_deref(), Some("crash"));
        let err = std::fs::read_to_string(bundle.join("error.txt")).unwrap();
        assert!(err.contains("injected invariant violation"));
        let fp = std::fs::read_to_string(bundle.join("fingerprint.json")).unwrap();
        assert!(fp.contains("\"save_version\": 2"));
        assert!(fp.contains("5a5a5a5a")); // data_set_hash hex prefix
        assert!(fp.contains("\"state_hash\": \"a5a5a5a5")); // world hash hex
    }

    /// The §2.13 crash path end-to-end: an injected `Fatal` halts the
    /// app, marks the session read-only, writes the bundle to the
    /// redirected root, and mirrors the path for the crash dialog.
    #[test]
    fn forced_fatal_writes_bundle_and_marks_read_only() {
        let dir = tempdir("fatal");
        // Hermetic settings: a throwaway path — never the real
        // `$XDG_CONFIG_HOME`, so a developer's settings file cannot
        // change the world under test.
        let mut app = App::new_with_settings_path(&dir.join("settings.ron"));
        app.bug_report_root = dir.clone();
        // A Fatal only exists after a stepped frame in production —
        // and the §7.4 snapshot refuses while commands sit undrained
        // OR a vessel holds live ground contact — the seeded pad craft
        // now *rests* in contact from tick one, so step past its
        // settle → surface-latch (~128 settle ticks after first
        // contact; 320 covers it with margin).
        app.view.sim_step(320).unwrap();
        app.on_sim_fatal(SimError::Fatal("injected invariant violation (test)"));
        assert!(app.read_only);
        assert_eq!(
            app.fatal,
            Some(SimError::Fatal("injected invariant violation (test)"))
        );
        let mirror = app.view.world.resource::<csp_view::SessionMirror>();
        assert_eq!(mirror.fatal, app.fatal);
        let path = mirror
            .fatal_bundle
            .clone()
            .expect("bundle path mirrored for the dialog");
        let bundle = std::path::Path::new(&path);
        assert!(bundle.join("save.cspsave").exists());
        assert!(bundle.join("commands.bin").exists());
        assert!(bundle.join("log.txt").exists());
        // The bundle save decodes and restores through the §7.7 codec.
        let decoded =
            csp_view::save::decode_save(&std::fs::read(bundle.join("save.cspsave")).unwrap())
                .unwrap();
        assert_eq!(decoded.snapshot.clock_ticks, app.view.sim.clock().ticks());
        // The fingerprint carries the halted world's §7.1 state hash —
        // the postmortem's first replay comparison.
        let fp = std::fs::read_to_string(bundle.join("fingerprint.json")).unwrap();
        let expect = app
            .view
            .sim
            .state_hash()
            .unwrap()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        assert!(fp.contains(&expect), "fingerprint lacks the state hash");
    }

    /// M10 first run: the production constructor seeds the stock pad
    /// craft — the app must not boot onto an empty Flight void (the
    /// "uniform purple window" regression: the scene pass had nothing
    /// to draw and every pixel stayed the clear color). The seed rides
    /// the §7.6 command queue, landing on the first stepped tick.
    #[test]
    fn new_game_boots_with_a_pad_vessel() {
        let dir = tempdir("first-run");
        let mut app = App::new_with_settings_path(&dir.join("settings.ron"));
        app.view.begin_frame();
        app.view.sim_step(1).unwrap();
        app.view.end_frame();
        assert_eq!(app.view.mode(), ViewMode::Flight);
        assert!(
            app.view.flight_focus().is_some(),
            "no pad vessel at boot — the flight scene would be empty"
        );
        // …and the same craft is loaded in the VAB for editing.
        app.view
            .with_vab_session(|v| assert_eq!(v.session().len(), 3));
    }

    /// A rebound key resolves to its new action — the input layer reads
    /// `settings.keys`, never a hardcoded table: binding "m" to
    /// `WarpUp` makes the next "m" press a warp step, not a map toggle.
    #[test]
    fn rebound_key_dispatches_its_new_action() {
        let dir = tempdir("rebind-dispatch");
        let mut app = App::new_with_settings_path(&dir.join("settings.ron"));
        // Default: the M-position key → ToggleMap (physical token).
        assert_eq!(
            app.settings.keys.action_for("key-m"),
            Some(KeyAction::ToggleMap)
        );
        app.dispatch(KeyAction::ToggleMap);
        assert_eq!(app.view.mode(), ViewMode::Map);
        app.dispatch(KeyAction::ToggleMap);
        assert_eq!(app.view.mode(), ViewMode::Flight);
        // Rebind "m" to WarpUp — the same key now drives the controller.
        app.settings
            .keys
            .bind(KeyAction::WarpUp, KeyBinding::parse("m").unwrap());
        let action = app.settings.keys.action_for(
            settings_io::key_token(&winit::keyboard::Key::Character(
                winit::keyboard::SmolStr::new("m"),
            ))
            .as_deref()
            .unwrap_or(""),
        );
        assert_eq!(action, Some(KeyAction::WarpUp));
        app.dispatch(action.unwrap());
        // The request folded into the controller (one ladder step up).
        app.warp.decide();
        assert_eq!(app.warp.requested_multiplier(), 2);
    }

    /// The Apply path end-to-end: staged settings become the live set,
    /// persist to the injected file, and reach their live targets —
    /// `ui_scale` sets the egui zoom factor, `max_warp_multiplier` caps
    /// the controller's ladder, and the app-tracked `window_size`
    /// survives the staged copy's stale value.
    #[test]
    fn settings_apply_persists_and_reaches_live_targets() {
        let dir = tempdir("apply");
        let path = dir.join("settings.ron");
        let mut app = App::new_with_settings_path(&path);
        app.settings.graphics.window_size = [1920, 1080]; // a resize landed
        let mut staged = app.view.staged_settings();
        staged.graphics.ui_scale = 1.5;
        staged.gameplay.max_warp_multiplier = 100;
        staged.graphics.window_size = [1280, 720]; // stale staged value
        app.view.stage_settings(&staged);
        app.handle_ui_outbox(csp_view::UiOutbox {
            settings_apply: true,
            ..Default::default()
        });
        // Applied live: the egui zoom factor lands at the start of the
        // next pass (`set_zoom_factor` defers), and the warp cap holds.
        app.view.begin_frame();
        app.view.end_frame();
        assert!((app.view.egui_context().zoom_factor() - 1.5).abs() < 1e-6);
        app.warp.request_step(99);
        app.warp.decide();
        assert_eq!(app.warp.requested_multiplier(), 100);
        // The tracked window size won over the stale staged copy.
        assert_eq!(app.settings.graphics.window_size, [1920, 1080]);
        // Persisted: the injected path now holds the applied file.
        let on_disk = settings_io::load_settings_from(&path);
        assert_eq!(on_disk.gameplay.max_warp_multiplier, 100);
        assert!((on_disk.graphics.ui_scale - 1.5).abs() < 1e-6);
        assert_eq!(on_disk.graphics.window_size, [1920, 1080]);
    }

    /// The rebind plumbing: an armed target + a landed key updates the
    /// staged keymap and reports the eviction. The capture is
    /// open-gated — a closed window never reports a listening target.
    #[test]
    fn rebind_capture_updates_staged_keymap() {
        let dir = tempdir("rebind");
        let mut app = App::new_with_settings_path(&dir.join("settings.ron"));
        // A closed window reports no target even when armed — the
        // X-close path clears it and `rebind_target` gates on open.
        app.view.set_rebind_target(Some(KeyAction::ThrottleFull));
        assert_eq!(app.view.rebind_target(), None);
        app.view.toggle_settings_panel(); // open
        app.view.set_rebind_target(Some(KeyAction::ThrottleFull));
        assert_eq!(app.view.rebind_target(), Some(KeyAction::ThrottleFull));
        app.view
            .bind_key(KeyAction::ThrottleFull, KeyBinding::parse("w").unwrap());
        assert_eq!(app.view.rebind_target(), None);
        assert_eq!(
            app.view.staged_settings().keys.action_for("w"),
            Some(KeyAction::ThrottleFull)
        );
        // "w" was evicted from its default owner but the arrow
        // alternative still pans.
        assert_eq!(
            app.view.staged_settings().keys.action_for("arrow-up"),
            Some(KeyAction::PanUp)
        );
    }

    /// §6.3 demo mode: the app flag comes from the view, craft-file
    /// save/load outbox requests are dropped with a notice, and the
    /// store stays empty — the session-level `file_io` gate is the
    /// deeper layer, this is the app's half.
    #[test]
    fn demo_drops_craft_file_io() {
        let dir = tempdir("demo");
        let mut app = App::new_demo_with_settings_path(&dir.join("settings.ron"));
        assert!(app.demo);
        app.handle_ui_outbox(csp_view::UiOutbox {
            craft_save: Some(("demo-craft".to_string(), vec![1, 2, 3])),
            craft_load: Some("demo-craft".to_string()),
            ..Default::default()
        });
        assert!(app.craft_store.is_empty());
        // A full session is not demo — craft IO passes through.
        let mut app = App::new_with_settings_path(&dir.join("settings.ron"));
        assert!(!app.demo);
        app.handle_ui_outbox(csp_view::UiOutbox {
            craft_save: Some(("demo-craft".to_string(), vec![1, 2, 3])),
            ..Default::default()
        });
        assert!(app.craft_store.contains_key("demo-craft"));
        // …but launching is still playable in the demo (§6.3 caps
        // parts and refuses craft IO, not flight) — the launch gate
        // is `read_only`, not `demo`.
        let mut app = App::new_demo_with_settings_path(&dir.join("settings.ron"));
        app.view.set_mode(ViewMode::Vab);
        app.handle_ui_outbox(csp_view::UiOutbox {
            launch: true,
            ..Default::default()
        });
        assert_eq!(app.view.mode(), ViewMode::Flight);
    }

    /// A raw TextEdit name must never reach the filesystem: separator
    /// and traversal names are refused before the write *and* before
    /// the store insert, so no phantom `craft_store` entry or escaped
    /// file can appear. A clean name writes to disk first, then the
    /// store — insert-after-write.
    #[test]
    fn unsafe_craft_names_are_refused() {
        let dir = tempdir("craft-names");
        let mut app = App::new_with_settings_path(&dir.join("settings.ron"));
        for bad in ["../escape", "a/b", "a\\b", "c:drive", "", "x\0y", ".."] {
            app.handle_ui_outbox(csp_view::UiOutbox {
                craft_save: Some((bad.to_string(), vec![1])),
                ..Default::default()
            });
            assert!(app.craft_store.is_empty(), "{bad:?} entered the store");
        }
        // Nothing escaped `craft_dir` into its parent.
        assert!(!dir.join("escape.cspcraft").exists());
        // The notice row explains the refusal honestly — the event
        // lands via the Input schedule, so pump one frame first.
        app.view.begin_frame();
        app.view.end_frame();
        app.view.with_vab_session(|v| {
            assert!(v.notice().unwrap().contains("not a usable craft name"));
        });
        // A clean name saves — the disk mirror lands, then the store.
        app.handle_ui_outbox(csp_view::UiOutbox {
            craft_save: Some(("good-name".to_string(), vec![1])),
            ..Default::default()
        });
        assert!(app.craft_store.contains_key("good-name"));
        assert!(app.craft_dir.join("good-name.cspcraft").exists());
    }

    /// §2.13's read-only halt gates the command paths the craft-save
    /// gate already covered: `outbox.launch` and `outbox.maneuver` are
    /// refused with a notice instead of queueing behind a clock that
    /// never ticks — and `launch_craft`'s inert-but-misleading switch
    /// to Flight never happens.
    #[test]
    fn read_only_refuses_launch_and_maneuver() {
        let dir = tempdir("read-only-io");
        let mut app = App::new_with_settings_path(&dir.join("settings.ron"));
        // Drain the seeded pad launch so the command log is quiet.
        app.view.begin_frame();
        app.view.sim_step(1).unwrap();
        app.view.end_frame();
        let logged = app.view.sim.command_log().len();
        let vessel = app.view.flight_focus().expect("seeded pad vessel");
        app.view.set_mode(ViewMode::Vab);
        app.read_only = true;
        app.handle_ui_outbox(csp_view::UiOutbox {
            launch: true,
            maneuver: Some(SimCommand::SetManeuver {
                vessel,
                tick: 10_000,
                dv_prograde_mps: 1.0,
                dv_normal_mps: 0.0,
                dv_radial_mps: 0.0,
            }),
            ..Default::default()
        });
        // No misleading switch to Flight, nothing was enqueued.
        assert_eq!(app.view.mode(), ViewMode::Vab);
        app.view.begin_frame();
        app.view.sim_step(2).unwrap();
        app.view.end_frame();
        assert_eq!(app.view.sim.command_log().len(), logged);
        app.view
            .with_vab_session(|v| assert!(v.notice().unwrap().contains("read-only")));
        // And the un-gated path still works — launch is only refused
        // while halted.
        let mut app = App::new_with_settings_path(&dir.join("settings.ron"));
        app.view.set_mode(ViewMode::Vab);
        app.handle_ui_outbox(csp_view::UiOutbox {
            launch: true,
            ..Default::default()
        });
        assert_eq!(app.view.mode(), ViewMode::Flight);
    }
}
