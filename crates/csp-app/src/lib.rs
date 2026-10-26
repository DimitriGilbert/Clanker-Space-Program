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
/// §2.2.1 source id for the Program window's progression commands
/// (research, science runs/banking, contract accepts; Ui class, own
/// FIFO). None of them is control-affecting, so a separate channel
/// cannot reorder them against the player's craft verbs.
const PROGRESSION_SOURCE_ID: u32 = 4;
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
/// Throttle fraction applied instantly on a `ThrottleUp`/`ThrottleDown`
/// press — the tap: a quick press nudges the needle one fine step
/// (and, on a latched pad craft, that first `SetThrottle` is the
/// unlatching command). Holding continues into [`THROTTLE_RAMP_PER_S`].
const THROTTLE_TAP_STEP: f64 = 0.01;
/// Peak throttle fraction per second while a `ThrottleUp`/`ThrottleDown`
/// key is held — [`Self::tick_throttle_ramp`] integrates the frame's
/// wall dt into one `SetThrottle` per frame on the player FIFO. The
/// rate does not snap on: it eases in over [`THROTTLE_RAMP_SPINUP_S`],
/// so a hold that lingers a frame or two still reads as fine trim and
/// a sustained hold climbs smoothly to full rate (full travel ≈ 4.7 s
/// from zero). Releasing the key just stops enqueuing — the last
/// commanded value persists in the sim.
const THROTTLE_RAMP_PER_S: f64 = 0.25;
/// Seconds of actively-ramping hold time over which the ramp rate
/// eases from zero to [`THROTTLE_RAMP_PER_S`], quadratically
/// (`rate = max · (t/spinup)²`): the first half-second of a hold moves
/// ~1% on its own — less than the tap — and the axis reaches full rate
/// by one second. This is what keeps a "tap that lingered" from
/// reading as a jump while leaving a real hold fully responsive.
const THROTTLE_RAMP_SPINUP_S: f64 = 1.0;
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
    /// The `VabPacket::craft_epoch` snapshotted beside
    /// `pick_decode_ids` — the stale-pick stamp (P10-F3). A craft
    /// edit between the pick encode and the map_async landing makes
    /// the snapshotted id belong to a different generation: the
    /// readback is dropped, not applied to the wrong node.
    pick_decode_epoch: Option<u64>,
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
    /// The `saves/` directory — sibling of `craft_dir` (the same
    /// config-dir convention, the same test-injection precedent).
    /// Native only — wasm file IO refuses before it can read it.
    #[cfg(not(target_arch = "wasm32"))]
    save_dir: std::path::PathBuf,
    /// The session op (§7.2 save/load/revert/new-game) applied at
    /// this frame's drained boundary — a §7.4 save only exists at a
    /// moment where the queue is empty, so the verb waits for the
    /// step rather than firing mid-apply.
    session_op: Option<SessionOp>,
    /// The world was rebuilt this frame (load/revert/new-game) —
    /// the stale `AdvanceReport` from the pre-rebuild step is
    /// discarded rather than surfaced against the new world.
    world_rebuilt: bool,
    /// Left-drag state for map camera orbit — pointer position at the
    /// last `CursorMoved`, and whether the left button is held.
    last_cursor: Option<(f64, f64)>,
    map_dragging: bool,
    /// The last unconsumed LMB press — a candidate VAB click. The
    /// outer `Some` records the press itself; the inner `Option` is
    /// the cursor anchor — `None` when no position has been delivered
    /// yet (winit's `MouseInput` carries no position and `Window`
    /// exposes no cursor-position query, so a click can precede the
    /// first `CursorMoved`; the anchor then binds to the first
    /// position delivered during the press). A release within
    /// `CLICK_MAX_DRAG_PX` of the anchor becomes a pick request; a
    /// release past it was an orbit drag and does not select.
    lmb_press: Option<Option<(f64, f64)>>,
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
    /// Attitude keys currently held (flight mode) — `held-token → the
    /// action its press resolved to`. The held-token is the same
    /// physical-first token [`Self::key_press`] resolves (`key-w` for
    /// the W-position key, a logical token for non-positional keys,
    /// `verb:<action>` for programmatic dispatches with no key event).
    /// Keying by token — not by action — is what makes a release
    /// remove exactly what its own press added: two bindings on one
    /// action (W + ArrowUp) hold independently instead of aliasing,
    /// and a rebind mid-hold can't strand a stale action the release
    /// re-resolution would miss. [`Self::apply_attitude`] folds the
    /// held action *values* into one `SetAttitude` per change; a focus
    /// loss clears the map (the missed release could never arrive) and
    /// re-issues a zero-rate hold.
    attitude_held: std::collections::BTreeMap<String, KeyAction>,
    /// Set when a press left `attitude_held` non-empty but no
    /// `SetAttitude` could issue — no commandable body (pad-latched,
    /// on rails) or a halted session. [`Self::tick_attitude`] retries
    /// every frame, so a key held through the latch → liftoff edge
    /// starts steering the moment a bubble body exists again.
    attitude_deferred: bool,
    /// `ThrottleUp`/`ThrottleDown` currently held — the axis
    /// [`Self::tick_throttle_ramp`] integrates per frame. Populated on
    /// press, depopulated on release; a focus loss clears it (the
    /// missed release could never arrive — the throttle simply holds
    /// its last commanded value, so no cancellation is needed).
    throttle_held: std::collections::BTreeSet<KeyAction>,
    /// The wall-side throttle axis position — what the tap steps and
    /// the ramp *command*, independent of what the sim last applied
    /// (an unlatch rebuilds the body at throttle 0, a dry engine
    /// clamps to 0 — the player's axis must not snap back under
    /// either). Re-initialized from the applied value when a ramp
    /// starts from released; the Z/X snaps write it too, so a snap
    /// mid-hold rebases the ramp instead of fighting it.
    throttle_axis: f64,
    /// The vessel `throttle_axis` currently describes — set whenever
    /// a `SetThrottle` is enqueued and on every focus rebase. `None`
    /// until the first command goes out, in which case the axis is
    /// the player's intent verbatim (the P02-F2 rule: a fresh ramp on
    /// a body-less focus must not squash a nonzero axis a queued
    /// `SetThrottle` will still land). When the focus moves to a
    /// DIFFERENT vessel the axis cannot keep the old craft's
    /// commanded value — a delivered `SurfaceLatched` craft's applied
    /// throttle is 0 by construction, and a stale axis at the rail
    /// makes `throttle_axis_step` a permanent no-op (the Shift key
    /// literally cannot unlatch or move the needle).
    throttle_axis_vessel: Option<csp_view::VesselId>,
    /// Wall seconds the current hold has spent *actively ramping* —
    /// the ease-in clock for [`THROTTLE_RAMP_SPINUP_S`]. Reset when
    /// the held set empties (the next press starts a fresh ramp), and
    /// advanced only on frames that actually command: a hold spent
    /// blocked (rails, no vessel) or cancelled (Up+Down both held)
    /// still eases in from zero the frame it starts moving the axis.
    throttle_hold_s: f64,
    /// The flight-focus vessel the current throttle hold began on —
    /// recorded by [`Self::throttle_press`]. A focus change mid-hold
    /// (recover-control pinning, the newest-staged fallback moving)
    /// must not carry the OLD vessel's commanded axis onto the new
    /// focus: [`Self::tick_throttle_ramp`] rebases the axis from the
    /// new vessel's applied value and says so (C15-lib2-F2).
    throttle_hold_vessel: Option<csp_view::VesselId>,
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
    /// The last rails-warp refusal text — the persistent-notice
    /// latch: the refusal predicate re-reports every frame while it
    /// holds, so the cue posts only when the reason *changes* (a
    /// polling script named once, not spammed per frame).
    last_refusal: Option<String>,
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
        let ((settings, load_notes), settings_path) = (
            settings_io::load_settings(),
            Some(settings_io::settings_path()),
        );
        #[cfg(target_arch = "wasm32")]
        let ((settings, load_notes), settings_path) = (settings_io::load_settings(), None);
        Self::new_inner(
            settings,
            load_notes,
            settings_path,
            std::path::PathBuf::from("crafts"),
        )
    }

    /// The settings-path-injecting constructor — what tests drive so the
    /// real `$XDG_CONFIG_HOME/csp/settings.ron` can never leak into a
    /// test process (see the `settings_path` field's doc). Production
    /// goes through `new()` → `settings_io::settings_path()`.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    fn new_with_settings_path(settings_path: &std::path::Path) -> Self {
        let (settings, load_notes) = settings_io::load_settings_from(settings_path);
        Self::new_inner(
            settings,
            load_notes,
            Some(settings_path.to_path_buf()),
            settings_path.with_file_name("crafts"),
        )
    }

    /// The §6.3 demo build for tests — `View::new_demo` through the same
    /// [`Self::assemble`] the production constructor uses.
    #[cfg(all(test, not(target_arch = "wasm32")))]
    #[allow(clippy::expect_used)]
    fn new_demo_with_settings_path(settings_path: &std::path::Path) -> Self {
        let (settings, load_notes) = settings_io::load_settings_from(settings_path);
        Self::assemble(
            View::new_demo().expect("demo data load failed"),
            settings,
            load_notes,
            Some(settings_path.to_path_buf()),
            settings_path.with_file_name("crafts"),
        )
    }

    #[allow(clippy::expect_used)]
    fn new_inner(
        settings: Settings,
        load_notes: Vec<String>,
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
        Self::assemble(view, settings, load_notes, settings_path, craft_dir)
    }

    /// The `View`-injecting half of construction — the demo build and
    /// the demo-mode tests route `View::new_demo()` through the same
    /// assembly (settings staging, difficulty flags, live settings).
    /// `load_notes` carries the settings-file repairs the load
    /// performed ([`settings_io::load_settings`]) — surfaced as
    /// notices below, never swallowed.
    fn assemble(
        view: View,
        mut settings: Settings,
        load_notes: Vec<String>,
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
        // "layout-dependent bindings exist" notice instead. Every
        // repair the file load already performed (P14-F2/F3's salvage,
        // backfill, non-finite rejects — `load_notes`) joins the same
        // notice batch: the file's changes surface in every mode, not
        // just the log.
        let mut notes = load_notes;
        let migrated = settings_io::migrate_char_bindings(&mut settings.keys);
        if migrated > 0 {
            let dropped = settings.keys.sanitize();
            notes.push(format!(
                "migrated {migrated} layout-dependent character binding(s) to positional keys ({dropped} collision(s) dropped)"
            ));
        }
        let layout_bound = settings_io::layout_dependent_binding_count(&settings.keys);
        if layout_bound > 0 {
            notes.push(format!(
                "{layout_bound} key binding(s) use layout-dependent characters — rebind them or Reset to defaults"
            ));
        }
        if !notes.is_empty() {
            for note in &notes {
                log::warn!("settings: {note}");
            }
            view.flight_notice(format!("SETTINGS — {}", notes.join("; ").to_uppercase()));
            view.set_settings_notice(Some(notes.join("; ")));
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
            pick_decode_epoch: None,
            craft_store: load_craft_store(&craft_dir),
            craft_dir: craft_dir.clone(),
            // `saves/` sits beside `crafts/` — same convention, same
            // test injection (the dir never exists until a save does).
            #[cfg(not(target_arch = "wasm32"))]
            save_dir: craft_dir
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."))
                .join("saves"),
            session_op: None,
            world_rebuilt: false,
            last_cursor: None,
            map_dragging: false,
            lmb_press: None,
            captured_key: None,
            attitude_held: std::collections::BTreeMap::new(),
            attitude_deferred: false,
            throttle_held: std::collections::BTreeSet::new(),
            throttle_axis: 0.0,
            throttle_axis_vessel: None,
            throttle_hold_s: 0.0,
            throttle_hold_vessel: None,
            view,
            warp: WarpController::new(),
            last_frame: Instant::now(),
            last_tick_ms: 0.0,
            fatal: None,
            settings,
            settings_path,
            read_only: false,
            last_refusal: None,
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
        app.stage_save_list();
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
        // A hand-edited file can carry a literal `NaN` — `f32::clamp`
        // passes it through and `set_zoom_factor(NaN)` would NaN the
        // whole egui UI persistently (P14-F3). The load path already
        // rejects non-finite values; the guard here covers every other
        // source (a future widget, an in-memory edit) the same way.
        let raw_scale = self.settings.graphics.ui_scale;
        let scale = if raw_scale.is_finite() {
            raw_scale.clamp(UI_SCALE_MIN, UI_SCALE_MAX)
        } else {
            1.0
        };
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
                self.view
                    .flight_notice("SESSION IS READ-ONLY — WARP DEMOTION NOT APPLIED");
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
        // The §3.1 data-set hash rides both the save envelope and the
        // fingerprint — one walk of `data/`, never two.
        let data_set_hash = csp_view::data_set_hash(&csp_view::data_root()).ok();
        let save = self
            .view
            .sim
            .snapshot()
            .map_err(|e| format!("snapshot refused: {e}"))
            .and_then(|snapshot| {
                csp_view::save::encode_save(&csp_view::save::Save {
                    game_version: env!("CARGO_PKG_VERSION").into(),
                    label: Some(format!("crash at tick {}", snapshot.clock_ticks)),
                    data_set_hash,
                    state_hash: self.view.sim.state_hash().ok(),
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
            data_set_hash,
            state_hash: self.view.sim.state_hash().ok(),
            save_seed: Some(self.view.sim.save_seed()),
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
    /// presentation setup, failure reports to the page through
    /// [`csp_report_gpu_error`] so the demo shell can swap its loading
    /// screen for the styled fallback instead of hanging on a black
    /// canvas (the old path only `log::error!`ed — `init()` had long
    /// resolved, so nothing ever reached JS).
    #[cfg(target_arch = "wasm32")]
    fn poll_gpu_init(&mut self) {
        let Some(rx) = &self.gpu_rx else {
            return;
        };
        match rx.try_recv() {
            Ok(Ok(gpu)) => {
                self.gpu_rx = None;
                self.finish_gpu_init(gpu);
                csp_report_gpu_ready();
            }
            Ok(Err(e)) => {
                self.gpu_rx = None;
                log::error!("GPU init failed: {e}");
                csp_report_gpu_error(&e.to_string());
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.gpu_rx = None;
                log::error!("GPU init task was dropped");
                csp_report_gpu_error("GPU init task was dropped");
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
            // The snapshotted id table is only meaningful while the
            // session still reports the epoch it was rasterized from
            // (P10-F3): a craft edit between encode and landing makes
            // the decoded id stale — drop the readback rather than
            // select or attach a different generation's node.
            let live_epoch = self.view.with_vab_session(|v| v.craft_epoch());
            if matches!(self.pick_decode_epoch, Some(ep) if ep != live_epoch) {
                // The snapshotted id table belongs to another craft
                // generation, so the decoded node cannot be trusted —
                // but a pure drop swallows the click outright. Resolve
                // it as a background pick instead: `Select(None)` is
                // id-safe (the session filters by `contains`, and the
                // new generation's selection is cleared anyway).
                log::info!(
                    "stale VAB pick readback — craft_epoch moved since encode; resolving as background"
                );
                self.view.vab_event(VabEvent::Pick(None));
                continue;
            }
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

        // The app's wall-side held-key axes, ahead of the Input
        // schedule: the throttle ramp integrates this frame's wall dt
        // into one `SetThrottle` on the player FIFO (drained by the
        // `sim_step` below with every other queued command), and a
        // deferred attitude request retries — a key held through the
        // latch → liftoff edge starts steering immediately.
        self.tick_attitude();
        self.tick_throttle_ramp(dt_wall);

        // §2.5 order: Input → SimStep → Sync → Presentation → UiBuild →
        // RenderExtract. One sim_step call per frame, always.
        self.view.begin_frame();
        if let Some(target) = target {
            let step_start = Instant::now();
            match self.view.sim_step(target) {
                Ok(report) => {
                    // The drained boundary — the one moment §7.4's
                    // save guarantees hold (queue empty). Session ops
                    // run here; a world-replacing op discards the
                    // stale report rather than surfacing it.
                    self.run_session_op();
                    if !self.world_rebuilt {
                        self.on_step_report(&report);
                    }
                    self.world_rebuilt = false;
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

    /// The Game menu / session-key verbs — drained from the outbox
    /// into `session_op` for application at the drained boundary.
    fn handle_game_verbs(&mut self, outbox: &mut csp_view::UiOutbox) {
        if let Some(slot) = outbox.game_save.take() {
            self.request_session_op(SessionOp::Save(slot));
        }
        if let Some(slot) = outbox.game_load.take() {
            self.request_session_op(SessionOp::Load(slot));
        }
        if outbox.revert {
            self.request_session_op(SessionOp::Revert);
        }
        if outbox.new_game {
            self.request_session_op(SessionOp::NewGame);
        }
    }

    /// Queue a session op — a §2.13 read-only session and the §6.3
    /// demo refuse the verb up front rather than failing it
    /// silently at the boundary.
    fn request_session_op(&mut self, op: SessionOp) {
        if self.read_only || self.demo {
            self.view
                .flight_notice("saves are off while the session is read-only / in the web demo");
            return;
        }
        self.session_op = Some(op);
    }

    /// Apply this frame's queued session op at the drained boundary.
    fn run_session_op(&mut self) {
        let Some(op) = self.session_op.take() else {
            return;
        };
        #[cfg(target_arch = "wasm32")]
        match op {
            SessionOp::Save(slot) | SessionOp::Load(slot) => {
                let _ = slot;
            }
            SessionOp::Revert | SessionOp::NewGame => {}
        }
        #[cfg(not(target_arch = "wasm32"))]
        match op {
            SessionOp::Save(slot) => self.do_save(&slot),
            SessionOp::Load(slot) => self.do_load(&slot),
            SessionOp::Revert => match self.view.revert_to_launch() {
                Ok(_) => {
                    self.world_rebuilt = true;
                    self.view.flight_notice("REVERTED TO LAUNCH");
                }
                Err(e) => self
                    .view
                    .flight_notice(format!("cannot revert: {}", e.reason())),
            },
            SessionOp::NewGame => match self.view.new_game() {
                Ok(()) => {
                    self.world_rebuilt = true;
                    self.fatal = None;
                    self.read_only = false;
                    self.warp = WarpController::new();
                    self.apply_startup_difficulty();
                    self.view.flight_notice("NEW GAME");
                }
                Err(e) => self.view.flight_notice(format!("new game failed: {e}")),
            },
        }
    }

    /// Encode + write the live world to `saves/<slot>.cspsave` —
    /// §7.2's codec end to end, the same format `session.save`
    /// writes (report-04's format-unification fix).
    #[cfg(not(target_arch = "wasm32"))]
    fn do_save(&mut self, slot: &str) {
        if !safe_slot_name(slot) {
            self.view
                .flight_notice(format!("refused save name '{slot}'"));
            return;
        }
        match self.view.save_game() {
            Err(e) => {
                let reason = self
                    .view
                    .sim
                    .save_blocked_reason()
                    .unwrap_or_else(|| e.reason());
                self.view.flight_notice(format!("SAVE REFUSED — {reason}"));
            }
            Ok(mut save) => {
                save.game_version = env!("CARGO_PKG_VERSION").into();
                save.label = Some(slot.to_string());
                save.data_set_hash = csp_view::data_set_hash(&csp_view::data_root()).ok();
                let bytes = match csp_view::save::encode_save(&save) {
                    Ok(b) => b,
                    Err(e) => {
                        self.view.flight_notice(format!("save encode failed: {e}"));
                        return;
                    }
                };
                let dir = self.save_dir.clone();
                if let Err(e) = std::fs::create_dir_all(&dir) {
                    self.view.flight_notice(format!("save failed: {e}"));
                    return;
                }
                // The atomic-rename rule — a killed writer never
                // leaves a half file for `inspect_save` to trip on.
                let path = dir.join(format!("{slot}.cspsave"));
                let tmp = dir.join(format!(".{slot}.cspsave.tmp"));
                if let Err(e) =
                    std::fs::write(&tmp, &bytes).and_then(|()| std::fs::rename(&tmp, &path))
                {
                    self.view.flight_notice(format!("save failed: {e}"));
                    return;
                }
                self.view.flight_notice(format!("SAVED — {slot}"));
                self.stage_save_list();
            }
        }
    }

    /// Decode `saves/<slot>.cspsave` and rebuild the world — the
    /// §7.5 integrity check (recorded state hash vs restored hash)
    /// and the §7.7 data-set drift warning both surface as notices.
    #[cfg(not(target_arch = "wasm32"))]
    fn do_load(&mut self, slot: &str) {
        if !safe_slot_name(slot) {
            self.view
                .flight_notice(format!("refused save name '{slot}'"));
            return;
        }
        let path = self.save_dir.join(format!("{slot}.cspsave"));
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => {
                self.view.flight_notice(format!("load failed: {e}"));
                return;
            }
        };
        let save = match csp_view::save::decode_save(&bytes) {
            Ok(s) => s,
            Err(e) => {
                self.view.flight_notice(format!("load failed: {e}"));
                return;
            }
        };
        // §7.7's data-set drift warning — the save's recorded hash vs
        // the live data root's. `None` reads as "unknown data set".
        match (
            save.data_set_hash,
            csp_view::data_set_hash(&csp_view::data_root()).ok(),
        ) {
            (Some(recorded), Some(current)) if recorded != current => {
                self.view.flight_notice(
                    "DATA DRIFT — this save was written against a different data set",
                );
            }
            (None, _) => self
                .view
                .flight_notice("load: save predates data-set hashing"),
            _ => {}
        }
        match self.view.load_game(save.snapshot) {
            Ok(warnings) => {
                self.world_rebuilt = true;
                self.view.flight_notice(format!("LOADED — {slot}"));
                for w in warnings {
                    self.view.flight_notice(w);
                }
                if let Some(recorded) = save.state_hash
                    && self.view.sim.state_hash().ok() != Some(recorded)
                {
                    self.view.flight_notice(
                        "DIVERGED — restored world hash differs from the save's recorded hash",
                    );
                }
            }
            Err(e) => self
                .view
                .flight_notice(format!("load failed: {}", e.reason())),
        }
    }

    /// Rescan the save dir and stage the Game menu's slot list.
    /// O(header) per save — `inspect_save` never inflates a body.
    fn stage_save_list(&mut self) {
        #[cfg(target_arch = "wasm32")]
        {}
        #[cfg(not(target_arch = "wasm32"))]
        {
            let mut rows = Vec::new();
            if let Ok(read) = std::fs::read_dir(&self.save_dir) {
                for entry in read.flatten() {
                    let path = entry.path();
                    if path.extension().is_none_or(|e| e != "cspsave") {
                        continue;
                    }
                    let Some(name) = path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .map(str::to_string)
                    else {
                        continue;
                    };
                    let meta = std::fs::read(&path)
                        .ok()
                        .and_then(|b| csp_view::save::inspect_save(&b).ok());
                    rows.push(csp_view::game::SaveSlotRow {
                        name,
                        tick: meta.as_ref().and_then(|m| m.tick).unwrap_or(0),
                        label: meta.and_then(|m| m.label),
                    });
                }
            }
            rows.sort_by(|a, b| b.tick.cmp(&a.tick).then_with(|| a.name.cmp(&b.name)));
            self.view.set_save_list(rows);
        }
    }

    /// The frame's successful `AdvanceReport` — rejected/degraded
    /// channels, clamping + rails-refusal warp demotion, the W1.5
    /// partial-backlog demotion, and the fired-event notice drain.
    /// Extracted from `run_frame` under the complexity budget.
    fn on_step_report(&mut self, report: &csp_view::AdvanceReport) {
        // §2.6.5: a fired Clamping event demotes warp — now, before
        // the next frame's step — so the player cannot overshoot
        // (PlayerAlarm, tracked-vessel SoiTransition). The demotion
        // itself is a logged SetWarp; re-promotion still runs the
        // §2.6.3 hysteresis.
        let clamping = report
            .fired_events
            .iter()
            .any(|e| e.class == EventClass::Clamping);
        // §2.2.1/§2.13's honest channels: rejected commands and
        // degraded events surface as log lines — they are the app's
        // view of Recoverable, never a halt. A rejected PLAYER command
        // is also a *refusal* — the press that issued it did nothing —
        // so it lands on the global cue channel too, not just the log.
        // The cue is scoped to `SourceClass::Ui`: a Script/Agent
        // channel's reject is machine traffic, and a buggy script
        // could otherwise spam cues + blips every frame.
        for rej in &report.rejected {
            log::warn!(
                "command rejected on originating channel {:?}: {}",
                rej.key,
                rej.error
            );
            if rej.key.source_class == SourceClass::Ui {
                self.view.flight_notice(format!(
                    "COMMAND REFUSED — {}",
                    rej.error.reason().to_uppercase()
                ));
            }
        }
        for e in &report.degraded {
            log::warn!("event degraded (fallback applied): {e}");
        }
        // §2.6.2's rails-warp refusal seam: the sim-side predicate,
        // checked here every frame on post-step state — the only
        // place it can be. Honest about the one-frame lag: the
        // predicate reads state THIS frame's step produced, and the
        // demote is a queued SetWarp that lands at the next frame's
        // boundary — the frame the refusal appeared in already ran at
        // the old level. Bounded, not compounding: the refusal
        // conditions (thrust, contact, atmosphere) persist until
        // commanded away, so the predicate re-reports every frame
        // until the state changes.
        let refusal = self.view.sim.rails_warp_refusal();
        // The refusal reason is also a persistent HUD notice —
        // `rails_warp_refusal` names the offender (a polling script,
        // a burning vessel) and stderr was its only outlet before.
        // Latch on the text so a held refusal doesn't cue-spam.
        if refusal != self.last_refusal
            && let Some(reason) = &refusal
        {
            self.view
                .flight_notice(format!("RAILS WARP ×1 — {}", reason.to_uppercase()));
        }
        self.last_refusal = refusal.clone();
        if (clamping || refusal.is_some())
            && let Some(level) = self.warp.demote(0)
        {
            // §2.6.2: the refusal names the offending vessel.
            if let Some(reason) = &refusal {
                log::warn!("rails warp refused ({reason}) — demoting to ×{level}");
            }
            self.view
                .enqueue_command(SourceClass::Ui, WARP_SOURCE_ID, SimCommand::SetWarp(level));
        }
        // W1.5-(f): `partial` = step_frame split at a budget — the
        // advance never reached the frame's target, so time silently
        // falls behind warp. Demote toward the largest rung one call
        // can reach (TICK_HZ×mult/60 ≤ MAX_TICKS_PER_FRAME ⇒ rung 4,
        // ×10⁴) and say so — §2.6.4's "carry the back-pressure" rule
        // made visible.
        if report.partial {
            if let Some(level) = self.warp.demote(4) {
                log::warn!("partial advance — warp demoted to ×{level}");
                self.view.enqueue_command(
                    SourceClass::Ui,
                    WARP_SOURCE_ID,
                    SimCommand::SetWarp(level),
                );
            }
            self.view.flight_notice("WARP LIMITED — EVENT/TICK BACKLOG");
        }
        // W1.5-(g): fired event kinds the player asked for or pays
        // for — alarms, deadlines, completions — post to the cue
        // channel, bounded. The heap firing used to be invisible
        // until its side-effects landed.
        for text in fired_event_notices(&report.fired_events) {
            self.view.flight_notice(text);
        }
        // §2.10's script console: `log`/`print` lines and the host's
        // diagnostics (strike-disables, wake refusals, poll demotes)
        // had zero consumers outside the bench agent — fold them into
        // the AUTOMATION section's tail mirror AND flash them on the
        // cue channel (bounded — a chatty script can only spend four
        // cue slots a frame).
        if !report.script_log.is_empty() {
            self.view
                .push_script_log(self.view.sim.clock().ticks(), &report.script_log);
            for (id, line) in report.script_log.iter().take(4) {
                self.view
                    .flight_notice(format!("script {} — {}", id.0, line));
            }
            if report.script_log.len() > 4 {
                self.view.flight_notice(format!(
                    "script log — +{} more lines (Program → AUTOMATION)",
                    report.script_log.len() - 4
                ));
            }
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
    fn handle_ui_outbox(&mut self, mut outbox: csp_view::UiOutbox) {
        self.handle_game_verbs(&mut outbox);
        if outbox.warp_step != 0 {
            if self.read_only {
                // Same §2.13 discipline as the key-driven warp verbs:
                // a halted session's `decide` never runs, so the
                // request would change nothing — say so.
                self.view
                    .flight_notice("SESSION IS READ-ONLY — WARP NOT APPLIED");
            } else {
                self.warp.request_step(outbox.warp_step);
            }
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
                self.craft_refusal("craft save is off in the web demo".to_string());
            } else if self.read_only {
                // §2.13: a halted session is read-only — a craft file
                // written from a possibly-corrupt world is an artifact
                // nobody should trust.
                self.craft_refusal(format!("session is read-only — '{name}' not saved"));
            } else if let Err(reason) = validate_craft_name(&name) {
                // Reject before either half of the store can diverge:
                // a separator or traversal name would write outside —
                // or below — `craft_dir` while the store key and the
                // file list showed a phantom entry the non-recursive
                // loader can never find.
                self.craft_refusal(format!("'{name}' is not a usable craft name ({reason})"));
            } else if let Err(e) = save_craft_file(&self.craft_dir, &name, &bytes) {
                // Honest failure: no store entry, no list update — the
                // craft files row and the disk mirror stay in lockstep.
                self.craft_refusal(format!("could not save '{name}': {e}"));
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
            let old_keys = self.settings.keys.clone();
            self.settings = applied;
            self.apply_live_settings();
            // The applied keymap is now live — re-mirror it so the
            // controls card and hint bars name the keys that actually
            // dispatch (they never read the staged copy).
            self.view.set_live_keymap(&self.settings.keys);
            if old_keys != self.settings.keys {
                self.drop_rebound_holds(&old_keys);
            }
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
                // flight_notice, not the VAB row: a halted session can
                // be sitting in any mode, and the VAB's notice row is
                // invisible outside the editor.
                self.view
                    .flight_notice("SESSION IS READ-ONLY — LAUNCH REFUSED");
            } else if let Err(reason) = self.view.launch_craft() {
                // `launch_refused` already wrote the VAB session's
                // notice row; forward the same text to the global cue
                // channel so the refusal reads whatever mode the
                // session sits in (a VAB row is VAB-only).
                self.view.flight_notice(reason.to_uppercase());
            }
        }
        // M5 node panel → the §7.6-logged command path.
        if let Some(cmd) = outbox.maneuver {
            if self.read_only {
                // Same §2.13 halt discipline — the command must not
                // queue silently behind a clock that never ticks again.
                self.view
                    .flight_notice("SESSION IS READ-ONLY — MANEUVER NOT APPLIED");
            } else {
                self.view
                    .enqueue_command(SourceClass::Ui, MANEUVER_SOURCE_ID, cmd);
            }
        }
        // Program window → the progression commands (science runs,
        // data banking, research spends, contract accepts).
        self.dispatch_program(outbox.program);
        if let Some(name) = outbox.craft_load {
            if self.demo {
                // §6.3: no craft-file import either — the VabSession's
                // `file_io` gate would refuse the load anyway; this
                // keeps the honest notice at the app's layer.
                self.craft_refusal(format!("craft load is off in the web demo ('{name}')"));
            } else {
                match self.craft_store.get(&name) {
                    Some(b) => self.view.vab_event(VabEvent::LoadCraft(b.clone())),
                    None => self.craft_refusal(format!("no craft file '{name}'")),
                }
            }
        }
    }

    /// The Program window's commands — science runs, data banking,
    /// research spends, contract accepts — enqueued on the
    /// progression FIFO (§2.5's one-frame hand-off, same as the
    /// maneuver channel). Same §2.13 read-only discipline: a halted
    /// session queues nothing.
    fn dispatch_program(&mut self, cmds: Vec<SimCommand>) {
        if cmds.is_empty() {
            return;
        }
        if self.read_only {
            self.view
                .flight_notice("SESSION IS READ-ONLY — ACTION NOT APPLIED");
            return;
        }
        for cmd in cmds {
            self.view
                .enqueue_command(SourceClass::Ui, PROGRESSION_SOURCE_ID, cmd);
        }
    }

    /// A craft-file refusal lands on TWO channels: the VAB session's
    /// notice row (the panel context that produced the request) AND
    /// the global `flight_notice` cue channel — the row is invisible
    /// outside the editor, and a refused click must say so wherever
    /// the session sits.
    fn craft_refusal(&mut self, text: String) {
        self.view.flight_notice(text.clone());
        self.view.vab_event(VabEvent::Notice(text));
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
                    // 4×MSAA: every pipeline rasterizes into the
                    // multisampled target and the pass resolves into
                    // the surface view (C11-F1 — the previous frame
                    // had no AA anywhere).
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: gpu.msaa_view(),
                        resolve_target: Some(&frame.view),
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
                // R2: the surface pass first — its sky is the backdrop
                // and OPAQUE terrain writes the depth the craft tests
                // against. The ocean is a THIRD draw after the craft
                // stream: alpha-blended water must resolve over an
                // already-rasterized hull, not occlude it with depth
                // written before the craft existed (C11-F3).
                if let (Some(t), Some(tp)) = (&mut self.terrain_renderer, &packet.terrain) {
                    t.draw(&mut pass, &gpu.device, &gpu.queue, tp);
                }
                f.draw(&mut pass, &gpu.device, &gpu.queue, packet);
                if let (Some(t), Some(tp)) = (&mut self.terrain_renderer, &packet.terrain) {
                    t.draw_ocean(&mut pass, tp);
                }
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
        // same encoder submission as the frame. An in-bounds click
        // that cannot rasterize (empty vertices, an unresolved craft,
        // no packet at all) still resolves — as a background pixel —
        // so "click empty space" cancels an armed placement instead
        // of dropping silently.
        let mut encode_pick = false;
        let mut encoded_ids = None;
        let mut encoded_epoch = None;
        let mut resolve_background = false;
        if let Some((x, y)) = self.pending_pick.take() {
            if self.pick_in_flight {
                // One outstanding pick — the click can't rasterize a
                // second pass, but it is still a *click*: resolve it
                // as background so a select/cancel gesture lands
                // instead of vanishing (P10-F1).
                resolve_background = true;
            } else if x < gpu.config.width && y < gpu.config.height {
                encode_pick = match (&mut self.vab_renderer, &self.last_vab_packet) {
                    (Some(v), Some(packet)) => {
                        let ok = v.encode_pick(
                            &mut frame.encoder,
                            &gpu.device,
                            &gpu.queue,
                            packet,
                            x,
                            y,
                        );
                        if ok {
                            // Snapshot the decode table alongside the
                            // rasterized pixels — the readback decodes
                            // a frame or two later and `last_vab_packet`
                            // may have moved on by then. The craft epoch
                            // rides beside it so a pick encoded against
                            // one craft generation can't decode into the
                            // next (P10-F3).
                            encoded_ids = Some(packet.pick_ids.clone());
                            encoded_epoch = Some(packet.craft_epoch);
                        }
                        ok
                    }
                    _ => false,
                };
                resolve_background = !encode_pick;
            } else {
                // The release landed outside the (possibly just-
                // resized) surface — definitionally a background
                // pixel: no rasterize exists for it, but the click
                // still deselects and disarms a pending placement
                // (P10-F2).
                resolve_background = true;
            }
        }
        if let Some(ids) = encoded_ids {
            self.pick_decode_ids = Some(ids);
            self.pick_decode_epoch = encoded_epoch;
        }
        gpu.submit(*frame);
        if encode_pick {
            if let Some(v) = &self.vab_renderer {
                v.map_pick(self.pick_tx.clone());
            }
            self.pick_in_flight = true;
        } else if resolve_background {
            // The pick pass would have cleared this pixel to the
            // background id anyway — resolve it through the same
            // `Pick(None)` the readback decodes (deselect + disarm a
            // pending placement).
            self.view.vab_event(VabEvent::Pick(None));
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
    /// warp steps or mode toggles. The held verbs — attitude and the
    /// throttle axis — ignore repeats too: their press inserts into a
    /// held set the per-frame tick drives, so repeats are deduped no-ops.
    fn key_press(&mut self, event: &winit::event::KeyEvent) {
        self.press_edge(&event.physical_key, &event.logical_key, event.repeat);
    }

    /// [`Self::key_press`] with the winit `KeyEvent` decomposed to its
    /// two key identities + the repeat flag — `winit::event::KeyEvent`
    /// is not constructible outside winit (its `platform_specific`
    /// field is crate-private), so tests drive THIS with real
    /// `PhysicalKey`/`Key` values and exercise the same resolution
    /// and dispatch the window event produces.
    fn press_edge(
        &mut self,
        physical: &winit::keyboard::PhysicalKey,
        logical: &winit::keyboard::Key,
        repeat: bool,
    ) {
        // Rebind capture — before all other dispatch, swallowing the key.
        if self.view.rebind_target().is_some() {
            if repeat {
                // A held key must not count as a second "next press".
                return;
            }
            match settings_io::binding_of(physical, logical) {
                None => { /* unbindable key — keep listening */ }
                Some(b) if b.as_str() == "escape" => {
                    self.captured_key = Some(*physical);
                    self.view.set_rebind_target(None);
                }
                Some(b) => {
                    let Some(action) = self.view.rebind_target() else {
                        return;
                    };
                    self.captured_key = Some(*physical);
                    self.view.bind_key(action, b);
                }
            }
            return;
        }
        // Physical-first resolution (§ M10 AZERTY): the `key-*` tokens
        // name the key's *position* — a bound `key-w` fires on the
        // W-position key whether the layout prints `w` or `z` on it.
        // The logical token is the fallback for character bindings a
        // user (or an old settings file) recorded by glyph. The held
        // token is whichever produced the action — it is what the
        // release must remove from `attitude_held`.
        let phys = settings_io::keycode_token(physical);
        let (action, held_token) = match phys
            .as_deref()
            .and_then(|t| self.settings.keys.action_for(t))
        {
            Some(a) => (a, phys),
            None => {
                let logical = settings_io::key_token(logical);
                let Some(a) = logical
                    .as_deref()
                    .and_then(|t| self.settings.keys.action_for(t))
                else {
                    return;
                };
                (a, logical)
            }
        };
        if repeat && !action.accepts_repeat() {
            return;
        }
        self.dispatch_with_key(action, held_token);
    }

    /// The release-side resolver — the same physical-first order
    /// [`Self::press_edge`] uses, so a key that fired an attitude action
    /// releases exactly that action regardless of layout. Decomposed:
    /// a `KeyEvent` is not constructible outside winit.
    fn action_of_parts(
        &self,
        physical: &winit::keyboard::PhysicalKey,
        logical: &winit::keyboard::Key,
    ) -> Option<KeyAction> {
        settings_io::keycode_token(physical)
            .and_then(|t| self.settings.keys.action_for(&t))
            .or_else(|| {
                settings_io::key_token(logical).and_then(|t| self.settings.keys.action_for(&t))
            })
    }

    /// Mark an attitude action held and re-issue the combined rate
    /// command. `held_token` identifies the *key* — the same physical
    /// token the press resolved, or `verb:<action>` for a keyless
    /// programmatic dispatch (tests, future UI holds). The `BTreeMap`
    /// insert dedup makes OS key repeats free: a repeat press of the
    /// same key replaces its own entry, changing nothing.
    /// A press with no commandable body is NOT silent: the reason lands
    /// as a HUD cue ([`View::flight_notice`]) and the hold is deferred —
    /// [`Self::tick_attitude`] picks it up the frame a body exists.
    fn attitude_press(&mut self, action: KeyAction, held_token: String) {
        if self.attitude_held.insert(held_token, action).is_some() {
            return; // already held — an OS repeat, nothing changed
        }
        match self.attitude_blocker() {
            Some(reason) => {
                self.attitude_deferred = true;
                self.view.flight_notice(reason);
            }
            None => self.apply_attitude(),
        }
    }

    /// Clear an attitude hold on release and re-issue the combined
    /// rate — an empty set commands a zero angular rate (the stand-in
    /// control authority's "stop rotating" until M3+ real SAS). The
    /// removal is keyed on the released key's *token* so a second
    /// binding held for the same action keeps steering (two bindings,
    /// one verb). `false` when the token held nothing — the caller
    /// may then try the event's other token form.
    /// While blocked there is no live command to update — the held set
    /// just shrinks and the deferred flag tracks whether anything
    /// remains for [`Self::tick_attitude`] to pick up.
    fn attitude_release(&mut self, held_token: &str) -> bool {
        if self.attitude_held.remove(held_token).is_none() {
            return false;
        }
        if self.attitude_blocker().is_none() {
            self.attitude_deferred = false;
            self.apply_attitude();
        } else {
            self.attitude_deferred = !self.attitude_held.is_empty();
        }
        true
    }

    /// Per-frame retry for a held attitude request that had no
    /// commandable body when pressed — the pad case: WASD held through
    /// "throttle to liftoff" starts steering the frame the vessel is
    /// back in the bubble instead of requiring a second press.
    fn tick_attitude(&mut self) {
        if self.attitude_deferred
            && !self.attitude_held.is_empty()
            && self.attitude_blocker().is_none()
        {
            self.attitude_deferred = false;
            self.apply_attitude();
        }
    }

    /// Why the focused vessel cannot take a `SetAttitude` right now —
    /// `None` means it can. The `Some` text is the HUD cue the press
    /// surfaces, so a dead key always says why. The checks are the same
    /// reads [`Self::apply_attitude`] made silently: `InBubble` bodies
    /// have an attitude surface; a surface-latched pad craft or a rails
    /// vessel does not (and refusing keeps an accidental WASD press
    /// from unlatching the parked craft).
    fn attitude_blocker(&self) -> Option<&'static str> {
        if self.read_only {
            return Some("SESSION IS READ-ONLY — ATTITUDE NOT APPLIED");
        }
        let Some(vessel) = self.view.flight_focus() else {
            return Some("NO VESSEL — ATTITUDE LOCKED");
        };
        match self.view.sim.vessels().get(&vessel).map(|v| &v.state) {
            // An InBubble body the backend's settle pass latched —
            // resting on the pad's contact sphere, or stacked on
            // another vessel's static collider while that supporter
            // is still moving — never makes it to `SurfaceLatched`,
            // yet the backend skips it entirely while `throttle == 0`:
            // a `SetAttitude` would apply and do literally nothing —
            // a silent dead key. Same parked refusal + deferral as the
            // surface-latched case: `tick_attitude` lands the rate the
            // tick a throttle command unlatches it.
            Some(csp_view::VesselState::InBubble(body)) if body.settle.latched => {
                Some("PARKED — ATTITUDE LOCKED UNTIL LAUNCH (THROTTLE TO LIFTOFF)")
            }
            Some(csp_view::VesselState::InBubble(_)) => None,
            Some(csp_view::VesselState::SurfaceLatched(_)) => {
                Some("PARKED — ATTITUDE LOCKED UNTIL LAUNCH (THROTTLE TO LIFTOFF)")
            }
            Some(csp_view::VesselState::OnRails(_)) => Some("ATTITUDE LOCKED — VESSEL IS ON RAILS"),
            None => Some("NO VESSEL — ATTITUDE LOCKED"),
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
        let held = |a: KeyAction| self.attitude_held.values().any(|v| *v == a);
        let rate = |neg: KeyAction, pos: KeyAction| -> f64 {
            f64::from(held(pos) as i8 - held(neg) as i8) * ATTITUDE_RATE_RAD_S
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

    /// Why the focused vessel cannot take a `SetThrottle` right now —
    /// `None` means it can. The `Some` text is the HUD cue the press
    /// surfaces, the `attitude_blocker` analog the throttle verbs
    /// lacked (P02-F2): without it a rails or missing focus enqueued
    /// a `SetThrottle` the drain rejects `Recoverable` (a `log::warn`
    /// nobody in-game sees) or, for no vessel at all, returned
    /// silently — a dead key either way. `SurfaceLatched` is
    /// deliberately NOT a blocker: `SetThrottle` *is* the §2.6.4
    /// unlatch command ("throttle to liftoff").
    fn throttle_blocker(&self) -> Option<&'static str> {
        if self.read_only {
            return Some("SESSION IS READ-ONLY — THROTTLE NOT APPLIED");
        }
        let Some(vessel) = self.view.flight_focus() else {
            return Some("NO VESSEL — THROTTLE LOCKED");
        };
        match self.view.sim.vessels().get(&vessel).map(|v| &v.state) {
            Some(csp_view::VesselState::InBubble(_))
            | Some(csp_view::VesselState::SurfaceLatched(_)) => None,
            Some(csp_view::VesselState::OnRails(_)) => Some("VESSEL ON RAILS — THROTTLE LOCKED"),
            None => Some("NO VESSEL — THROTTLE LOCKED"),
        }
    }

    /// The throttle verbs' shared dispatch — the VAB suppresses the
    /// axis entirely (throttle is a flight verb; a press there says so
    /// rather than dying), and everywhere else `throttle_blocker`
    /// names the refusal — a snap press on a rails focus or no vessel
    /// used to be a dead key (P02-F2).
    fn throttle_dispatch(&mut self, action: KeyAction, mode: ViewMode) {
        if mode == ViewMode::Vab {
            self.view.flight_notice("THROTTLE — FLIGHT/MAP VIEW ONLY");
            return;
        }
        if matches!(action, KeyAction::ThrottleUp | KeyAction::ThrottleDown) {
            // Fine throttle — the held-key axis: the press inserts
            // into `throttle_held` (the per-frame ramp drives it from
            // there) and lands the tap step.
            self.throttle_press(action);
            return;
        }
        match self.throttle_blocker() {
            Some(reason) => self.view.flight_notice(reason),
            None => {
                // Snap keys rebase the axis too — a Z/X pressed
                // mid-hold moves the ramp's commanded value to the
                // rail instead of fighting it from the old base.
                self.throttle_axis = if action == KeyAction::ThrottleFull {
                    1.0
                } else {
                    0.0
                };
                if let Some(vessel) = self.view.flight_focus() {
                    self.throttle_axis_vessel = Some(vessel);
                    self.view.enqueue_command(
                        SourceClass::Ui,
                        // The shared player-command FIFO — launch's
                        // `SpawnPhysicalVessel` rides it too.
                        csp_view::PLAYER_SOURCE_ID,
                        SimCommand::SetThrottle {
                            vessel,
                            throttle: self.throttle_axis,
                        },
                    );
                }
            }
        }
    }

    /// Press edge of a throttle axis key: insert into the held set —
    /// [`Self::tick_throttle_ramp`] picks it up next frame — and apply
    /// the tap step so a quick press still moves the needle one fine
    /// notch (`BTreeSet::insert` dedup makes OS repeats free). A
    /// blocked press (no commandable focus) is NOT silent: the reason
    /// lands as a HUD cue via [`Self::throttle_blocker`], and the key
    /// stays held so the ramp picks the axis up the frame the focus
    /// is commandable again — the same held-through-a-transition
    /// contract the attitude verbs' deferral provides.
    fn throttle_press(&mut self, action: KeyAction) {
        if !self.throttle_held.insert(action) {
            return; // already held — an OS repeat
        }
        if self.throttle_held.len() == 1 {
            // A fresh hold restarts the ease-in clock — the ramp
            // always spools up from zero rate, never resumes the
            // speed a previous hold had spun to.
            self.throttle_hold_s = 0.0;
            // …and pins the focus it started on, so a focus change
            // mid-hold is detected as a retarget, not silently
            // applied to the new vessel (C15-lib2-F2).
            let focus = self.view.flight_focus();
            self.throttle_hold_vessel = focus;
            // A ramp starting from released re-reads the applied
            // throttle — but only a bubble-resident body HAS an
            // applied value to read (P02-F2). A latched or rails
            // focus keeps the axis where it was — while it still
            // describes THIS vessel: a queued `SetThrottle` may still
            // be in flight, and squashing to 0 would diverge the
            // wall-side axis from a nonzero commanded value. The rule
            // does NOT extend to a vessel the axis never described —
            // a delivered latched craft's applied throttle is 0 by
            // construction, and carrying the old craft's rail value
            // over makes this press a permanent no-op.
            let applied = focus
                .and_then(|v| self.view.sim.vessels().get(&v))
                .and_then(|v| v.body())
                .map(|b| b.throttle);
            if self.throttle_axis_vessel.is_some_and(|v| Some(v) != focus) {
                self.throttle_axis = applied.unwrap_or(0.0);
            } else if let Some(t) = applied {
                self.throttle_axis = t;
            }
            self.throttle_axis_vessel = focus;
        }
        match self.throttle_blocker() {
            // No doomed `SetThrottle` rides the player FIFO to a
            // log-only reject — the refusal says why instead.
            Some(reason) => self.view.flight_notice(reason),
            None => self.throttle_axis_step(if action == KeyAction::ThrottleUp {
                THROTTLE_TAP_STEP
            } else {
                -THROTTLE_TAP_STEP
            }),
        }
    }

    /// Release edge of a throttle axis key — just leaves the held set.
    /// Nothing is re-issued: throttle is persistent sim state, so the
    /// last commanded value is the hold ("released = hold"). An emptied
    /// set also zeroes the ease-in clock so the NEXT press's ramp
    /// spools up from zero rate again.
    fn throttle_release(&mut self, action: KeyAction) {
        self.throttle_held.remove(&action);
        if self.throttle_held.is_empty() {
            self.throttle_hold_s = 0.0;
            self.throttle_hold_vessel = None;
        }
    }

    /// The held-key throttle axis, driven once per frame from
    /// [`Self::run_frame`]: while `throttle_held` is non-empty the
    /// commanded value rides `±THROTTLE_RAMP_PER_S` of wall dt — a real
    /// axis, not a repeat-stepped switch — with the rate eased in
    /// quadratically over [`THROTTLE_RAMP_SPINUP_S`] of commanding hold
    /// (`throttle_hold_s`), so a brief hold is a fine trim and only a
    /// sustained one reaches full slew. Up+Down cancel (hold position,
    /// and the clock pauses with it). Nothing is enqueued when the
    /// axis is already clamped at the rail, so a held key cannot spam
    /// the §7.6 log with no-op commands.
    fn tick_throttle_ramp(&mut self, dt_wall: f64) {
        if self.throttle_held.is_empty() {
            return;
        }
        if self.read_only || self.view.mode() == ViewMode::Vab {
            // A halt mid-hold or a switch into the VAB ends the ramp —
            // the VAB suppresses throttle verbs (it's a flight axis).
            self.throttle_held.clear();
            self.throttle_hold_s = 0.0;
            self.throttle_hold_vessel = None;
            return;
        }
        let dir = i8::from(self.throttle_held.contains(&KeyAction::ThrottleUp))
            - i8::from(self.throttle_held.contains(&KeyAction::ThrottleDown));
        if dir == 0 {
            return;
        }
        // A blocked focus (rails, no vessel — `throttle_blocker`'s
        // whole list) has no throttle surface: keep the axis held but
        // don't feed the drain a reject every frame — the press's
        // notice already said why once. The ease-in clock pauses too:
        // a key held through a transition spools up when the focus
        // becomes commandable instead of resuming at full slew.
        if self.throttle_blocker().is_some() {
            return;
        }
        // C15-lib2-F2: a flight-focus change mid-hold must not carry
        // the OLD vessel's commanded axis onto the new focus — that
        // would ramp a newly-recovered vessel's throttle from the
        // previous craft's value with no signal. Rebase instead: the
        // axis takes the new vessel's applied value when it has a
        // body to read (the same rule a fresh hold uses — a body-less
        // focus keeps the axis), the ease-in clock restarts, and the
        // retarget lands on the cue channel. Checked here — after the
        // blocker — so the notice fires exactly when the hold would
        // actually command the new focus.
        let focus = self.view.flight_focus();
        if focus != self.throttle_hold_vessel {
            self.throttle_hold_vessel = focus;
            self.throttle_hold_s = 0.0;
            // A body-less retarget (a delivered latched craft, a
            // railed hull) reads 0 — the latch destroyed the applied
            // throttle with the body, so keeping the old axis would
            // carry the previous vessel's command onto this one.
            self.throttle_axis = focus
                .and_then(|v| self.view.sim.vessels().get(&v))
                .and_then(|v| v.body())
                .map_or(0.0, |b| b.throttle);
            self.throttle_axis_vessel = focus;
            self.view
                .flight_notice("THROTTLE HOLD — REBASED ON NEW FOCUS VESSEL");
        }
        let ease = (self.throttle_hold_s / THROTTLE_RAMP_SPINUP_S).min(1.0);
        self.throttle_hold_s += dt_wall;
        self.throttle_axis_step(f64::from(dir) * THROTTLE_RAMP_PER_S * ease * ease * dt_wall);
    }

    /// Move the throttle axis by `delta` — shared by the press tap and
    /// the per-frame ramp: clamp into `[0, 1]`, and when the value
    /// actually moved, enqueue the `SetThrottle` on the player FIFO
    /// (launch's `SpawnPhysicalVessel` rides it too — issue order is
    /// preserved in the §7.6 log).
    ///
    /// The no-movement early-return suppresses repeat commands on a
    /// vessel already AT the axis's value — but only that case is a
    /// real no-op: a §2.6.4 `SurfaceLatched` focus has no body to have
    /// received the axis's claim (the latch destroyed it; the latched
    /// throttle is 0 by construction — §2.6.2), so an axis pinned at
    /// the rail still has an undelivered command to send, and
    /// `SetThrottle` is the unlatch. Without it the key is dead: an
    /// axis left at full by a landed flight clamps every Shift tap to
    /// the same 1.0 the latch already erased (a landed craft can
    /// never be throttled back up, tap or held ramp alike). A zero
    /// axis commands nothing — a throttle-down-at-0 press must not
    /// unlatch a parked craft for nothing. And a `SetThrottle` still
    /// QUEUED for that craft means the claim is in transit, not lost
    /// — re-sending it would just spam the player FIFO (`sim.throttle_
    /// queued` is the in-flight check).
    fn throttle_axis_step(&mut self, delta: f64) {
        let next = (self.throttle_axis + delta).clamp(0.0, 1.0);
        if (next - self.throttle_axis).abs() <= f64::EPSILON {
            let Some(vessel) = self.view.flight_focus() else {
                return;
            };
            let undelivered = matches!(
                self.view.sim.vessels().get(&vessel).map(|v| &v.state),
                Some(csp_view::VesselState::SurfaceLatched(_))
            ) && !self.view.sim.throttle_queued(vessel);
            if !(undelivered && next > 0.0) {
                return;
            }
        }
        self.throttle_axis = next;
        let Some(vessel) = self.view.flight_focus() else {
            return;
        };
        self.throttle_axis_vessel = Some(vessel);
        self.view.enqueue_command(
            SourceClass::Ui,
            csp_view::PLAYER_SOURCE_ID,
            SimCommand::SetThrottle {
                vessel,
                throttle: next,
            },
        );
    }

    /// The `Stage` key — the view drains the launch-captured
    /// [`csp_view::flight::StagePlan`] into logged `JettisonPart`s.
    fn stage_press(&mut self) {
        if self.read_only {
            self.view
                .flight_notice("SESSION IS READ-ONLY — STAGING NOT APPLIED");
        } else {
            self.view.fire_stage();
        }
    }

    /// A live keymap swap invalidates the held sets: they store the
    /// resolved ACTION while a release resolves through whatever
    /// keymap is live *then* — a verb whose binding moved under a
    /// held finger never sees its release and would ramp (or steer)
    /// until a focus loss cleared it. Keep only the held verbs whose
    /// binding list did not move; when the attitude set shrank,
    /// re-issue the combined rate — [`Self::focus_lost`]'s "stopped
    /// keys command a zero rate" discipline.
    fn drop_rebound_holds(&mut self, old: &csp_view::settings::Keymap) {
        let new = &self.settings.keys;
        let attitude_before = self.attitude_held.len();
        self.attitude_held
            .retain(|_, a| old.bindings.get(a) == new.bindings.get(a));
        self.throttle_held
            .retain(|a| old.bindings.get(a) == new.bindings.get(a));
        if self.throttle_held.is_empty() {
            self.throttle_hold_s = 0.0;
            self.throttle_hold_vessel = None;
        }
        if self.attitude_held.len() != attitude_before {
            if self.attitude_held.is_empty() {
                self.attitude_deferred = false;
            }
            self.apply_attitude();
        }
    }

    /// A focus loss can drop the releases that clear a captured key,
    /// held attitude keys, the throttle axis — and the pointer state:
    /// `map_dragging` and the LMB click candidate live off `MouseInput`
    /// releases a focus change drops the same way, so they are cleared
    /// here too (C15-lib2-F1 — a stale drag used to orbit the camera
    /// on refocus with no button held).
    fn focus_lost(&mut self) {
        self.captured_key = None;
        self.map_dragging = false;
        self.lmb_press = None;
        self.throttle_held.clear();
        self.throttle_hold_s = 0.0;
        self.throttle_hold_vessel = None;
        self.attitude_deferred = false;
        if !self.attitude_held.is_empty() {
            self.attitude_held.clear();
            self.apply_attitude();
        }
    }

    /// LMB press/release for the VAB click-vs-drag decision — lifted
    /// out of `window_event` so the match stays a routing table.
    ///
    /// The pick is decided at RELEASE: an unconsumed press is only a
    /// click *candidate* — it may be the start of a camera orbit drag,
    /// and a drag must not also select. The press is recorded even
    /// with no known cursor position (winit's `MouseInput` carries
    /// none and `Window` exposes no cursor-position query, so a click
    /// with no prior `CursorMoved` would otherwise be dropped); the
    /// anchor then binds to the first position delivered during the
    /// press, and a no-motion release picks at the release position.
    fn lmb_click_candidate(&mut self, state: ElementState, consumed: bool) {
        match state {
            ElementState::Pressed => {
                self.lmb_press = if consumed {
                    None
                } else {
                    Some(self.last_cursor)
                };
            }
            ElementState::Released => {
                // VAB: an unconsumed click that stayed inside the drag
                // threshold requests a GPU pick at the cursor — the
                // readback lands via the pick channel a frame or two
                // later.
                if let Some(press) = self.lmb_press.take() {
                    // Release-side cursor wins — the click targets
                    // where it lands; the press anchor is the fallback
                    // for a press that saw no delivered motion.
                    let pos = self.last_cursor.or(press);
                    let dragged = match (press, pos) {
                        (Some(p), Some(q)) => (q.0 - p.0).hypot(q.1 - p.1) > CLICK_MAX_DRAG_PX,
                        _ => false,
                    };
                    if !consumed && !dragged && self.view.mode() == ViewMode::Vab {
                        match pos {
                            Some(q) => {
                                self.pending_pick =
                                    Some((q.0.max(0.0) as u32, q.1.max(0.0) as u32));
                            }
                            // Press+release with zero `CursorMoved`
                            // ever delivered — the click has no pixel
                            // to pick at (winit's `MouseInput` carries
                            // no position and `Window` exposes no
                            // cursor query). A silent drop is input
                            // loss; say so on the session's notice row.
                            None => {
                                self.view.vab_event(VabEvent::Notice(
                                    "click had no cursor position — nothing picked".to_string(),
                                ));
                            }
                        }
                    }
                }
            }
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
    /// [`Self::key_press`]); releases clear the held attitude verbs
    /// and the throttle axis. The release is matched on the action
    /// (not the key) so a key the press bound still releases under a
    /// rebind.
    fn keyboard_event(&mut self, event: &winit::event::KeyEvent) {
        match event.state {
            ElementState::Pressed => self.key_press(event),
            ElementState::Released => {
                self.release_edge(&event.physical_key, &event.logical_key);
            }
        }
    }

    /// The release half of [`Self::keyboard_event`] on the decomposed
    /// key identities — the `press_edge` counterpart (a `KeyEvent` is
    /// not constructible outside winit; tests drive this directly).
    fn release_edge(
        &mut self,
        physical: &winit::keyboard::PhysicalKey,
        logical: &winit::keyboard::Key,
    ) {
        // Attitude holds key off the *token* the press stored —
        // try the physical token first, then the logical (a
        // press whose physical key produced no token — an
        // `Unidentified` key or an unbound `key-*` — stored the
        // logical). One key = one entry; the first hit wins.
        let _ = settings_io::keycode_token(physical)
            .as_deref()
            .is_some_and(|t| self.attitude_release(t))
            || settings_io::key_token(logical)
                .as_deref()
                .is_some_and(|t| self.attitude_release(t));
        // The throttle axis still resolves the action (its
        // held set is action-keyed).
        if let Some(action) = self.action_of_parts(physical, logical)
            && action.is_throttle_axis()
        {
            self.throttle_release(action);
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
    /// command channel like every other mutation. The keyless dispatch
    /// path — tests and future programmatic verb triggers reach the
    /// held verbs through the `verb:<action>` fallback token; the real
    /// input path is [`Self::key_press`] → [`Self::dispatch_with_key`].
    #[cfg(test)]
    fn dispatch(&mut self, action: KeyAction) {
        self.dispatch_with_key(action, None);
    }

    /// [`Self::dispatch`] carrying the held-key token the press
    /// resolved (`key_press` passes it; programmatic dispatches pass
    /// `None` and the held verbs fall back to a `verb:<action>` token
    /// — a per-action placeholder no real key can produce).
    #[allow(clippy::too_many_lines)] // one arm per verb — the enum IS the table
    fn dispatch_with_key(&mut self, action: KeyAction, held_token: Option<String>) {
        let mode = self.view.mode();
        match action {
            // Session toggles acknowledge with the same blip the
            // outbox's UI-driven mode switch and Apply play — a
            // key-driven session event is the same class of event.
            KeyAction::OpenSettings => {
                // C14-F4: the default `escape` binding collides with
                // the VAB's universal cancel (the egui pass puts a
                // carried part back, or drops the selection, on the
                // same press). A settings toggle that fires *alongside*
                // the cancel turns one press into two actions — so
                // while the editor has something armed to cancel, the
                // toggle yields: Esc cancels first, and only opens
                // settings once there is nothing left to put back.
                let vab_armed = mode == ViewMode::Vab
                    && self
                        .view
                        .with_vab_session(|v| v.carried().is_some() || v.selected().is_some());
                if !vab_armed {
                    self.ui_confirm();
                    self.view.toggle_settings_panel();
                    // Restage on open — the window always edits the
                    // applied set; unapplied edits from a previous
                    // visit are discarded, not resurrected.
                    if self.view.settings_panel_open() {
                        self.view.stage_settings(&self.settings);
                    }
                }
            }
            // §7.2/§11.80 player session verbs — every one lands as a
            // `SessionOp` applied at this frame's drained boundary.
            // RevertToLaunch only ARMS the menu's disclosure prompt —
            // the key must show the same "not a reroll" text the
            // button does, never an instant restore.
            KeyAction::QuickSave => self.request_session_op(SessionOp::Save("quicksave".into())),
            KeyAction::QuickLoad => self.request_session_op(SessionOp::Load("quicksave".into())),
            KeyAction::RevertToLaunch => {
                if self.view.revert_available() {
                    self.view.arm_revert_confirm();
                } else if let Some(r) = self.view.revert_reason() {
                    self.view.flight_notice(format!("cannot revert: {r}"));
                } else {
                    self.view
                        .flight_notice("nothing to revert to — no launch yet");
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
            // logged `SetWarp` on the next decide(). While halted the
            // decide never runs — the press used to change literally
            // nothing; say so instead of dying silently.
            KeyAction::WarpUp | KeyAction::WarpDown => {
                if self.read_only {
                    self.view
                        .flight_notice("SESSION IS READ-ONLY — WARP NOT APPLIED");
                } else {
                    self.warp
                        .request_step(if action == KeyAction::WarpUp { 1 } else { -1 });
                }
            }
            // M4c flight throttle — the §7.6 command channel, snap
            // verbs and the held axis share the mode gate and the
            // refusal blocker (`throttle_dispatch` keeps `dispatch`
            // under the complexity budget).
            KeyAction::ThrottleFull
            | KeyAction::ThrottleCut
            | KeyAction::ThrottleUp
            | KeyAction::ThrottleDown => self.throttle_dispatch(action, mode),
            // Space fires the next stage — the view resolves the
            // session-captured drop set into logged `JettisonPart`s.
            // The fire itself is mode-agnostic (the plan binds the
            // launch's vessel, not the camera), so Flight's old
            // `mode ==` gate just made Space a dead key in Map —
            // widened to everything but the editor, where it still
            // says so rather than staging silently.
            KeyAction::Stage if mode != ViewMode::Vab => self.stage_press(),
            KeyAction::Stage => self.view.flight_notice("STAGE — FLIGHT/MAP VIEW ONLY"),
            KeyAction::CycleFocus if mode == ViewMode::Map => {
                self.view.map_event(MapKeyEvent::CycleFocus)
            }
            KeyAction::CycleFocus => {
                self.view.flight_notice("CYCLE FOCUS — MAP VIEW ONLY");
            }
            // Pass-1 dead-stick recovery: pull the focused (or newest)
            // on-rails vessel back into the bubble and take its
            // control lease.
            KeyAction::RecoverControl if mode != ViewMode::Vab => {
                if self.read_only {
                    self.view
                        .flight_notice("SESSION IS READ-ONLY — RECOVER-CONTROL NOT APPLIED");
                } else {
                    self.view.recover_control();
                }
            }
            KeyAction::RecoverControl => {
                self.view
                    .flight_notice("RECOVER-CONTROL — FLIGHT/MAP VIEW ONLY");
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
            // The Program window — the science loop's panel verb
            // (W1.5-c's tutorial points players here).
            KeyAction::OpenProgram => {
                self.ui_confirm();
                self.view.toggle_program_panel();
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
                    self.attitude_press(
                        action,
                        held_token.unwrap_or_else(|| format!("verb:{action:?}")),
                    );
                } else if matches!(action, KeyAction::RollLeft | KeyAction::RollRight) {
                    // Roll keys are flight-only — say so instead of
                    // dying as a dead key.
                    self.view.flight_notice("ROLL — FLIGHT VIEW ONLY");
                } else {
                    // The pan cluster still steers the map/VAB cameras
                    // per event.
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
            } // No catch-all: the match stays exhaustive on purpose —
              // a new `KeyAction` variant is a compile error here, not
              // another silently-dead verb.
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
            // Key presses and releases. egui reports `consumed` for
            // EVERY key press whenever *any* widget holds keyboard
            // focus (`wants_keyboard_input` = `focused().is_some()`) —
            // and hardcodes it for Tab besides, which also hands focus
            // to the first focusable widget. Honoring that verbatim
            // dead-keys the entire board the moment a TextEdit,
            // DragValue, slider, or Tab-focused button holds egui
            // focus — every verb swallowed with zero feedback (the
            // "no attitude response" trap). Only *text entry* justifies
            // hiding a press from the game: gate on
            // `text_edit_focused` instead, and then only for keys that
            // actually produce text — `Key::Character`. A focused field
            // has no use for Esc, the F-keys, or the arrows, but the
            // game does (OpenSettings, the pans): hiding those too
            // dead-keyed them whenever a TextEdit held focus
            // (C15-lib2-F3). A focused button/slider keeps its own egui
            // key handling (egui sees the event regardless) without
            // stealing the game's. A RELEASE always runs
            // `keyboard_event`: a release egui swallowed would
            // leak `attitude_held` (the craft would hold its last
            // commanded rate) or keep the throttle axis ramping until
            // a focus loss cleared it (the same reason LMB releases
            // clear `map_dragging` unconditionally — consumption gates
            // presses, never releases).
            WindowEvent::KeyboardInput { event, .. } => {
                let typing = self.view.egui_context().text_edit_focused()
                    && matches!(event.logical_key, winit::keyboard::Key::Character(_));
                if event.state == ElementState::Released || !typing {
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
                self.lmb_click_candidate(state, consumed);
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
                // A press still awaiting its first position binds its
                // anchor now — the drag threshold then measures real
                // movement during the press, while a no-motion click
                // keeps the release position as its pick pixel.
                if matches!(self.lmb_press, Some(None)) {
                    self.lmb_press = Some(Some(pos));
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
                // factor multiplies camera distance; scroll-UP (lines > 0)
                // is zoom-IN in every camera convention, so the factor
                // must be < 1 there — exp(−lines), not exp(lines).
                let factor = f64::exp(-lines);
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

/// The `csp --serve` command (§2.12, reports-19/20 F1): the shipped
/// hosting path — a headless `GameHost` (the sim's `World` is not
/// `Send`, so the dedicated-game-thread host is the only correct
/// shape), `Server::start`, and the telemetry SPA's packaged `dist/`
/// as `spa_dir`. The session shares the game's own `saves/` dir so a
/// `session.save` is a real §7.2 CSPSAVE the menu can load.
#[cfg(not(target_arch = "wasm32"))]
pub fn serve_cli() -> Result<(), String> {
    use std::sync::Arc;
    let save_dir = settings_io::config_dir().join("saves");
    // The world builds inside the game thread — `World` is `!Send`
    // (the script engine), so constructing it app-side and handing it
    // over is not an option. `View::new()` does the whole §3.1 data
    // load + engine install; the returned `View` is dropped there and
    // its `sim` lives on the game thread end to end.
    let host = csp_server::backend::GameHost::spawn_full(
        move || match csp_view::View::new() {
            Ok(v) => v.sim,
            Err(e) => panic!("data/ startup load failed: {e}"),
        },
        save_dir,
        csp_server::ingress::IngressQueue::new(),
        Some(Box::new(|w: &mut csp_view::SimWorld| {
            // §7.3's channel pair the snapshot cannot carry — the
            // same engines `View::from_source` installs.
            w.set_script_engine(Box::new(csp_view::RhaiHost::new()));
            w.set_autopilot_driver(Box::new(csp_view::AscentAutopilot::new()));
        })),
    )
    .map_err(|e| e.to_string())?;
    let spa_dir = packaged_spa_dir();
    let config = csp_server::serve::ServerConfig {
        spa_dir: Some(spa_dir),
        ..Default::default()
    };
    csp_server::serve::run_blocking(config, Arc::new(host)).map_err(|e| e.to_string())
}

/// The packaged telemetry SPA dist root — `apps/telemetry/dist/client`
/// next to the workspace (dev checkout), then a `share/csp/telemetry`
/// fallback next to the exe for packaged layouts.
#[cfg(not(target_arch = "wasm32"))]
fn packaged_spa_dir() -> std::path::PathBuf {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(std::path::PathBuf::from))
        .unwrap_or_default();
    for cand in [
        // Dev checkout: binary at target/{profile}/csp — walk up to
        // the workspace root.
        exe_dir
            .ancestors()
            .nth(2)
            .map(|r| r.join("apps/telemetry/dist/client")),
        Some(exe_dir.join("../share/csp/telemetry")),
        Some(std::path::PathBuf::from("apps/telemetry/dist/client")),
    ] {
        if let Some(dir) = cand
            && dir.join("index.html").exists()
        {
            return dir;
        }
    }
    std::path::PathBuf::from("apps/telemetry/dist/client")
}

/// The `csp --bug-report` command (§2.14): write a bundle for a fresh
/// session headlessly and print its path. `Ok(())` → exit 0.
#[cfg(not(target_arch = "wasm32"))]
pub fn bug_report_cli() -> Result<(), String> {
    let view = View::new().map_err(|e| e.to_string())?;
    let root = bugreport::bug_report_root();
    // One `data/` walk — the hash lands in both the save envelope and
    // the fingerprint (the `collect_bug_report` precedent).
    let data_set_hash = csp_view::data_set_hash(&csp_view::data_root()).ok();
    let save = view
        .sim
        .snapshot()
        .map_err(|e| format!("snapshot refused: {e}"))
        .and_then(|snapshot| {
            csp_view::save::encode_save(&csp_view::save::Save {
                game_version: env!("CARGO_PKG_VERSION").into(),
                label: None,
                data_set_hash,
                state_hash: view.sim.state_hash().ok(),
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
        data_set_hash,
        state_hash: view.sim.state_hash().ok(),
        save_seed: Some(view.sim.save_seed()),
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

/// The wasm→page init-status channel. GPU init resolves *after*
/// `init()` returned (it is spawned onto the winit loop because the
/// adapter/device requests are browser promises), so a failure can
/// never propagate through `init()`'s promise — the page instead
/// exposes `cspGpuInitFailed`/`cspGpuReady` hooks and reads the
/// `__csp_gpu_*` globals this snippet sets. Both are set defensively:
/// a page that only polls the globals still sees the outcome.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(inline_js = r#"
    export function csp_report_gpu_error(msg) {
        globalThis.__csp_gpu_init_error = String(msg);
        if (typeof globalThis.cspGpuInitFailed === "function") {
            globalThis.cspGpuInitFailed(globalThis.__csp_gpu_init_error);
        }
    }
    export function csp_report_gpu_ready() {
        globalThis.__csp_gpu_ready = true;
        if (typeof globalThis.cspGpuReady === "function") {
            globalThis.cspGpuReady();
        }
    }
"#)]
extern "C" {
    /// Report a GPU-init failure to the page (reason string).
    fn csp_report_gpu_error(msg: &str);
    /// Report that GPU init + renderer setup completed.
    fn csp_report_gpu_ready();
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

/// One player session op, applied at the next drained boundary.
///
/// §7.4: a save only exists at a moment where the queue is empty.
#[derive(Debug)]
enum SessionOp {
    /// Write the live world to this save slot.
    Save(String),
    /// Restore this save slot's world.
    Load(String),
    /// PLAN:1180 — restore the pre-launch snapshot.
    Revert,
    /// Abandon the session and start a fresh world.
    NewGame,
}

/// The `*.cspsave` slot-name rule — the craft-name rule's shape (no
/// traversal, no control chars, bounded length) so a hostile UI
/// string can never escape `saves/`. Native only — wasm file IO
/// refuses before it can check a name.
#[cfg(not(target_arch = "wasm32"))]
fn safe_slot_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ' '))
        && name.trim() == name
        && !name.starts_with('.')
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

/// The fired-event → cue-text map (W1.5-g/h): which §2.6.5 event
/// kinds post a flight notice, and with what text. Events the player
/// ordered (alarms, transfer windows, construction, transfers,
/// maneuver nodes, bench runs) or pays for (contract deadlines,
/// insolvency, colony power/life-support, crew life-support) must be
/// seen when they fire — never learned only from the side-effects.
/// Bookkeeping kinds (rescans, terminator sweeps, store pulses,
/// inspections, script wakes, salary accrual, part-failure shadow
/// events — the notification ring covers the player-facing one)
/// stay in the sim log only. Bounded by the caller's `.take`.
fn fired_event_notices(events: &[csp_view::events::ScheduledEvent]) -> Vec<String> {
    use csp_view::events::EventKind as K;
    events
        .iter()
        .take(8)
        .filter_map(|e| {
            Some(match &e.kind {
                K::PlayerAlarm { label } => format!("ALARM — {label}"),
                K::ContractDeadline { contract } => {
                    format!("CONTRACT DEADLINE — #{} expired", contract.0)
                }
                K::ConstructionComplete { build } => {
                    format!("CONSTRUCTION COMPLETE — build #{}", build.0)
                }
                K::TransferWindow {
                    origin,
                    destination,
                } => format!("TRANSFER WINDOW — {origin} → {destination}"),
                K::TransferComplete { transfer } => {
                    format!("TRANSFER COMPLETE — #{}", transfer.0)
                }
                K::ManeuverNodeStart { vessel } => {
                    format!("vessel {} — MANEUVER NODE STARTED", vessel.0)
                }
                K::SoiTransition {
                    vessel,
                    body,
                    direction,
                } => format!(
                    "vessel {} — {} {} SOI",
                    vessel.0,
                    match direction {
                        csp_view::orbit::SoiDirection::Enter => "ENTERED",
                        csp_view::orbit::SoiDirection::Exit => "LEFT",
                    },
                    body
                ),
                K::Insolvent { shortfall_micros } => format!(
                    "INSOLVENT — ₣{:.0} unpaid (funds clamped at zero)",
                    *shortfall_micros as f64 / 1.0e6
                ),
                K::ResearchProjectComplete { project } | K::ResearchDataExhausted { project } => {
                    format!("RESEARCH COMPLETE — project #{}", project.0)
                }
                K::BenchTestComplete { run } => format!("BENCH TEST COMPLETE — run #{}", run.0),
                K::DebrisDecay { vessel } => format!("vessel {} — DEBRIS DECAYED", vessel.0),
                K::VesselLifeSupportDepleted { vessel } => {
                    format!("vessel {} — LIFE SUPPORT DEPLETED (crew lost)", vessel.0)
                }
                K::LifeSupportDepleted { colony } => {
                    format!("colony #{} — LIFE SUPPORT DEPLETED", colony.0)
                }
                K::PowerLost { colony } => format!("colony #{} — POWER LOST", colony.0),
                K::PowerRestored { colony } => format!("colony #{} — POWER RESTORED", colony.0),
                _ => return None,
            })
        })
        .collect()
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
        assert_eq!(settings_io::load_settings_from(&path), (s, Vec::new()));
        // Missing file → defaults (the first-run path), no notes.
        assert_eq!(
            settings_io::load_settings_from(&dir.join("absent.ron")),
            (Settings::default(), Vec::new())
        );
        // Corrupt file → defaults + a visible repair note, never a
        // crash and never a silent reset (P14-F2).
        std::fs::write(&path, "not ron ((").unwrap();
        let (got, notes) = settings_io::load_settings_from(&path);
        assert_eq!(got, Settings::default());
        assert!(!notes.is_empty(), "a corrupt file must leave a note");
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
            state_hash: None,
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
            save_seed: Some(42),
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
        assert!(fp.contains(&format!(
            "\"save_version\": {}",
            csp_view::save::SAVE_VERSION_CURRENT
        )));
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
        // …and the same craft is loaded in the VAB for editing —
        // pod + tank + engine + canister + battery (W1.5-b).
        app.view
            .with_vab_session(|v| assert_eq!(v.session().len(), 5));
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
        let (on_disk, _) = settings_io::load_settings_from(&path);
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
        // is `read_only`, not `demo`. W2.6: the seeded craft holds
        // the pad's single berth (delivered latched at seed), so
        // retire it like a recovery would before ordering a second
        // build.
        let mut app = App::new_demo_with_settings_path(&dir.join("settings.ron"));
        app.view.sim.enqueue(
            csp_view::SourceClass::Ui,
            0,
            SimCommand::TerminateVessel {
                vessel: csp_view::VesselId(0),
            },
        );
        app.view.begin_frame();
        app.view.sim_step(app.view.sim.clock().ticks() + 1).unwrap();
        app.view.end_frame();
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
        // The refusals surface on the HUD's cue channel — the VAB's
        // notice row is invisible outside the editor, and these gates
        // fire in any mode.
        {
            let sfx = app.view.world.resource::<csp_view::flight::FlightSfx>();
            assert!(
                sfx.cues.iter().any(|c| c.text.contains("READ-ONLY")),
                "halted launch/maneuver refusals must surface a flight notice"
            );
        }
        // And the un-gated path still works — launch is only refused
        // while halted. W2.6: the seeded craft holds the pad's
        // single berth (delivered latched at seed), so retire it
        // like a recovery would before ordering the second build.
        let mut app = App::new_with_settings_path(&dir.join("settings.ron"));
        app.view.sim.enqueue(
            csp_view::SourceClass::Ui,
            0,
            SimCommand::TerminateVessel {
                vessel: csp_view::VesselId(0),
            },
        );
        app.view.begin_frame();
        app.view.sim_step(app.view.sim.clock().ticks() + 1).unwrap();
        app.view.end_frame();
        app.view.set_mode(ViewMode::Vab);
        app.handle_ui_outbox(csp_view::UiOutbox {
            launch: true,
            ..Default::default()
        });
        assert_eq!(app.view.mode(), ViewMode::Flight);
    }

    /// The held-key throttle axis: the press lands the tap step, the
    /// per-frame tick then ramps the commanded value — eased in
    /// quadratically over `THROTTLE_RAMP_SPINUP_S` up to
    /// `THROTTLE_RAMP_PER_S` of wall dt — and releasing just stops
    /// enqueuing: the sim holds the last value. Asserted on the §7.6
    /// command log (the values the axis actually commanded), which is
    /// also what a replay sees.
    #[test]
    fn held_throttle_ramps_then_holds_on_release() {
        let dir = tempdir("throttle-ramp");
        let mut app = App::new_with_settings_path(&dir.join("settings.ron"));
        // Settle the seeded pad craft to its surface latch.
        app.view.begin_frame();
        app.view.sim_step(320).unwrap();
        app.view.end_frame();
        let vessel = app.view.flight_focus().unwrap();
        // The logged `SetThrottle` values for the focused vessel, in
        // drain order — the axis's commanded trail.
        let logged = |app: &App| -> Vec<f64> {
            app.view
                .sim
                .command_log()
                .iter()
                .filter_map(|(_, c)| match c {
                    SimCommand::SetThrottle {
                        vessel: v,
                        throttle,
                    } if *v == vessel => Some(*throttle),
                    _ => None,
                })
                .collect()
        };
        // One "frame": the ramp tick, then a sim step that drains the
        // queued commands (the order `run_frame` uses).
        fn frame(app: &mut App, ticks: u64) {
            let target = app.view.sim.clock().ticks() + ticks;
            app.view.begin_frame();
            app.tick_throttle_ramp(1.0 / 60.0);
            app.view.sim_step(target).unwrap();
            app.view.end_frame();
        }
        // Expected ramp step on commanding-hold frame `k` at
        // dt = 1/60: rate = RAMP·(t/spinup)² until the ease-in tops
        // out, then the flat full slew. k=0 is the zero-rate frame —
        // it commands nothing at all.
        let ramp_step = |k: usize| -> f64 {
            let ease = (k as f64 / 60.0 / THROTTLE_RAMP_SPINUP_S).min(1.0);
            THROTTLE_RAMP_PER_S * ease * ease / 60.0
        };
        let full_step = THROTTLE_RAMP_PER_S / 60.0;

        // QUICK TAP: a press released ~80 ms later moves ~one tap
        // step — the still-spooling ramp contributes orders of
        // magnitude less (the user-visible fine-tune contract).
        app.dispatch(KeyAction::ThrottleUp);
        assert!(app.throttle_held.contains(&KeyAction::ThrottleUp));
        for _ in 0..5 {
            frame(&mut app, 30);
        }
        app.throttle_release(KeyAction::ThrottleUp);
        frame(&mut app, 30);
        let tap_vals = logged(&app);
        assert_eq!(tap_vals.len(), 5, "tap + 4 nonzero spin-up frames");
        assert!(
            (tap_vals[0] - THROTTLE_TAP_STEP).abs() < 1e-9,
            "tap step first"
        );
        for (i, w) in tap_vals.windows(2).enumerate() {
            assert!(
                (w[1] - w[0] - ramp_step(i + 1)).abs() < 1e-9,
                "tap frame {i}: ramp moved {} → {}",
                w[0],
                w[1]
            );
        }
        assert!(
            tap_vals[4] - tap_vals[0] < 1e-4,
            "an ~80 ms press should stay a fine nudge: {}",
            tap_vals[4]
        );

        // SUSTAINED HOLD: the tap re-reads the applied axis, then the
        // first second spools up and every frame after is the full
        // ±THROTTLE_RAMP_PER_S slew — smooth, never a jump.
        let base = tap_vals.len();
        app.dispatch(KeyAction::ThrottleUp);
        for _ in 0..150 {
            frame(&mut app, 30);
        }
        let vals = logged(&app);
        // Second tap + ramp frames k=1..=149 (k=0 commands nothing).
        assert_eq!(vals.len(), base + 1 + 149);
        assert!(
            (vals[base] - (tap_vals[4] + THROTTLE_TAP_STEP)).abs() < 1e-9,
            "second press taps from the applied axis"
        );
        for i in (base + 1)..vals.len() {
            let k = i - base; // ramp frame index, 1..=149
            let delta = vals[i] - vals[i - 1];
            assert!(
                (delta - ramp_step(k)).abs() < 1e-9,
                "hold frame {k}: expected +{}, got {delta}",
                ramp_step(k)
            );
        }
        // Ease-in sanity: a mid-spool frame moves well under full
        // slew; past the spin-up every frame is exactly the rate cap.
        assert!(vals[base + 30] - vals[base + 29] < full_step);
        assert!((vals[base + 120] - vals[base + 119] - full_step).abs() < 1e-9);

        // Release: the axis stops — no further commands, and the sim
        // keeps the last commanded value.
        app.throttle_release(KeyAction::ThrottleUp);
        frame(&mut app, 60);
        assert_eq!(logged(&app).len(), vals.len());
        // The snap key still jumps straight to full and rebases the
        // axis (a fresh hold would start from 1.0, not the old ramp).
        app.dispatch(KeyAction::ThrottleFull);
        frame(&mut app, 60);
        assert_eq!(*logged(&app).last().unwrap(), 1.0);
        assert_eq!(app.throttle_axis, 1.0);
    }

    /// A pad craft has no attitude surface — WASD used to die silently
    /// inside `apply_attitude`. Now the refused press lands a HUD cue
    /// naming the lock, the key stays held as a deferred request, and
    /// `tick_attitude` issues the real `SetAttitude` the frame a bubble
    /// body exists again (throttle → liftoff), so a key held through
    /// the launch starts steering without a second press.
    #[test]
    fn pad_attitude_press_notices_and_defers_until_liftoff() {
        let dir = tempdir("pad-attitude");
        let mut app = App::new_with_settings_path(&dir.join("settings.ron"));
        app.view.begin_frame();
        app.view.sim_step(320).unwrap();
        app.view.end_frame();
        let vessel = app.view.flight_focus().unwrap();
        let in_bubble = |app: &App| {
            app.view
                .sim
                .vessels()
                .get(&vessel)
                .and_then(|v| v.body())
                .is_some()
        };
        assert!(!in_bubble(&app), "seeded craft should be surface-latched");
        // Parked: the press is refused — loudly, on the HUD channel.
        app.dispatch(KeyAction::PanUp);
        {
            let sfx = app.view.world.resource::<csp_view::flight::FlightSfx>();
            assert!(
                sfx.cues.iter().any(|c| c.text.contains("ATTITUDE LOCKED")),
                "pad attitude press must surface a flight notice"
            );
        }
        assert!(app.attitude_deferred);
        // Throttle to liftoff still holding the pitch key — one frame
        // per iteration, the app's tick ahead of the sim step. The
        // unlatch lands inside a `sim_step`; the NEXT frame's
        // `tick_attitude` issues the deferred `SetAttitude`, so check
        // the commanded rate on that same frame before free
        // integration bleeds it.
        app.dispatch(KeyAction::ThrottleFull);
        let mut saw_command = false;
        for _ in 0..40 {
            let target = app.view.sim.clock().ticks() + 60;
            app.view.begin_frame();
            app.tick_attitude();
            app.view.sim_step(target).unwrap();
            app.view.end_frame();
            if in_bubble(&app) && !app.attitude_deferred {
                let body = app.view.sim.vessels()[&vessel].body().unwrap();
                saw_command = body.ang_vel.norm() > 0.1;
                break;
            }
        }
        assert!(in_bubble(&app), "full throttle should unlatch");
        assert!(
            saw_command,
            "held pitch key should command a rate once airborne"
        );
    }

    /// The held roll verb end-to-end: a `RollRight` press while airborne
    /// lands a nonzero `ang_vel` on the bubble body, the §2.9 integrator
    /// turns it into real rotation the render path stages, and the
    /// release leaves the craft holding the attitude it reached — the
    /// rotation is integrated state, not something the next event
    /// rewinds or cancels.
    #[test]
    fn held_roll_rotates_and_persists_airborne() {
        let dir = tempdir("roll-hold");
        let mut app = App::new_with_settings_path(&dir.join("settings.ron"));
        let frame = |app: &mut App, ticks: u64| {
            let target = app.view.sim.clock().ticks() + ticks;
            app.view.begin_frame();
            app.tick_attitude();
            app.view.sim_step(target).unwrap();
            app.view.end_frame();
        };
        // Quaternion angle between two orientations — how far the craft
        // actually turned, independent of any axis convention.
        let angle = |a: csp_core::DQuat, b: csp_core::DQuat| -> f64 {
            let d = (a.x * b.x + a.y * b.y + a.z * b.z + a.w * b.w)
                .abs()
                .min(1.0);
            2.0 * d.acos()
        };
        frame(&mut app, 320); // settle the seeded pad craft
        app.dispatch(KeyAction::ThrottleFull);
        for _ in 0..20 {
            frame(&mut app, 60); // liftoff — ~19 sim-seconds of climb
        }
        let vessel = app.view.flight_focus().unwrap();
        let body_of = |app: &App| {
            app.view.sim.vessels()[&vessel]
                .body()
                .map(|b| (b.rot, b.ang_vel))
        };
        let (rot0, _) = body_of(&app).expect("airborne body");
        // Hold roll: the first stepped frame after the press must carry
        // a nonzero commanded rate.
        app.dispatch(KeyAction::RollRight);
        frame(&mut app, 1);
        let (_, w1) = body_of(&app).unwrap();
        assert!(
            w1.norm() > 0.1,
            "held roll key commanded no angular rate: {w1:?}"
        );
        for _ in 0..5 {
            frame(&mut app, 60);
        }
        let (rot1, _) = body_of(&app).unwrap();
        let turned = angle(rot0, rot1);
        assert!(
            turned > 0.1,
            "held roll key produced no rotation ({turned:.4} rad)"
        );
        // Release: the rate stops and the craft KEEPS the attitude it
        // reached — the release's `SetAttitude` snapshot carries the
        // freshest rot, so the turn persists instead of rewinding.
        assert!(app.attitude_release("verb:RollRight"));
        frame(&mut app, 1);
        // A rewind bug snaps the attitude ~the whole held turn back
        // toward `rot0`; one frame of torque-driven drift is
        // sub-radian by comparison.
        let (rot_rel, w2) = body_of(&app).unwrap();
        assert!(
            angle(rot1, rot_rel) < 1.0,
            "release rewound the attitude: {:.4} rad from post-hold",
            angle(rot1, rot_rel)
        );
        // The release zeroes the COMMANDED rate — a climbing craft
        // still carries real angular velocity from aero/gyroscopic
        // torque, so the bound is well below the held 0.8 rad/s
        // command, not zero. Nine more seconds gives residual torque
        // time to matter if it were real; a live command cannot read
        // this low.
        for _ in 0..9 {
            frame(&mut app, 60);
        }
        let (_, w3) = body_of(&app).unwrap();
        assert!(
            w3.norm() < ATTITUDE_RATE_RAD_S * 0.5,
            "released roll key still commands ~full rate: {w2:?} → {w3:?}"
        );
    }

    /// Two bindings on one attitude verb (the shipped `key-w` +
    /// `arrow-up` pair): each key's hold is tracked by its token, so
    /// releasing the second binding does NOT drop the first's hold —
    /// the action-keyed set used to alias them and zero the rate while
    /// W was still held.
    #[test]
    fn second_binding_release_keeps_first_hold() {
        let dir = tempdir("alias-hold");
        let mut app = App::new_with_settings_path(&dir.join("settings.ron"));
        app.view.begin_frame();
        app.view.sim_step(320).unwrap();
        app.view.end_frame();
        // Parked craft: both presses defer (the same blocked path a
        // pad press takes) — the held set is what this test audits.
        app.attitude_press(KeyAction::PanUp, "key-w".to_string());
        app.attitude_press(KeyAction::PanUp, "arrow-up".to_string());
        assert_eq!(app.attitude_held.len(), 2, "each held key is an entry");
        // Releasing the arrow must NOT clear W's hold — the alias bug
        // emptied the set on the action.
        assert!(app.attitude_release("arrow-up"));
        assert!(
            app.attitude_held.values().any(|a| *a == KeyAction::PanUp),
            "releasing the second binding killed the first key's hold"
        );
        assert!(app.attitude_deferred);
        // And the remaining W hold still releases cleanly by its token.
        assert!(app.attitude_release("key-w"));
        assert!(app.attitude_held.is_empty());
    }

    /// P02-F2: throttle verbs name their refusal. With no vessel at
    /// all the axis press lands "NO VESSEL — THROTTLE LOCKED" on the
    /// cue channel and enqueues nothing; in the VAB the verbs are a
    /// mode refusal; a halted session's warp press says why too —
    /// none of them a dead key anymore.
    #[test]
    fn throttle_verbs_notice_instead_of_silent_refuse() {
        let dir = tempdir("throttle-blocker");
        // `View::default()` — no vessels, no pad seed.
        let mut app = App::assemble(
            View::default(),
            Settings::default(),
            Vec::new(),
            Some(dir.join("settings.ron")),
            dir.join("crafts"),
        );
        let cues = |app: &App| -> Vec<String> {
            app.view
                .world
                .resource::<csp_view::flight::FlightSfx>()
                .cues
                .iter()
                .map(|c| c.text.clone())
                .collect()
        };
        app.dispatch(KeyAction::ThrottleUp);
        assert!(cues(&app).iter().any(|c| c.contains("NO VESSEL")));
        // No doomed SetThrottle was enqueued for a nonexistent focus.
        assert!(app.view.sim.command_log().is_empty());
        app.dispatch(KeyAction::ThrottleFull);
        assert_eq!(
            cues(&app)
                .iter()
                .filter(|c| c.contains("NO VESSEL"))
                .count(),
            2
        );
        // VAB mode: every flight verb says it's the wrong view.
        app.view.set_mode(ViewMode::Vab);
        for a in [
            KeyAction::ThrottleUp,
            KeyAction::ThrottleFull,
            KeyAction::Stage,
            KeyAction::RecoverControl,
        ] {
            app.dispatch(a);
            assert!(
                cues(&app).iter().any(|c| c.contains("VIEW ONLY")),
                "{a:?} died silently in VAB"
            );
        }
        // Outside Map, CycleFocus is a mode refusal too.
        app.view.set_mode(ViewMode::Flight);
        app.dispatch(KeyAction::CycleFocus);
        assert!(cues(&app).iter().any(|c| c.contains("MAP VIEW ONLY")));
        // Halted: a warp keypress is a dead verb no longer.
        app.read_only = true;
        app.dispatch(KeyAction::WarpUp);
        assert!(cues(&app).iter().any(|c| c.contains("WARP NOT APPLIED")));
        assert_eq!(app.warp.requested_multiplier(), 1);
    }

    /// P02-F2's axis half: a ramp start on a focus with no bubble
    /// body — the latched pad craft — must NOT squash `throttle_axis`
    /// to 0; the axis is the wall-side commanded value and the sim's
    /// stored throttle may not be readable through `body()`.
    #[test]
    fn latched_focus_keeps_the_throttle_axis() {
        let dir = tempdir("axis-no-squash");
        let mut app = App::new_with_settings_path(&dir.join("settings.ron"));
        // Settle the seeded pad craft to its surface latch.
        app.view.begin_frame();
        app.view.sim_step(320).unwrap();
        app.view.end_frame();
        let vessel = app.view.flight_focus().unwrap();
        assert!(
            app.view.sim.vessels()[&vessel].body().is_none(),
            "seeded craft should be surface-latched (no body to read)"
        );
        // A commanded axis value in flight (e.g. just-snapped full)
        // survives the next press's re-read — latched craft have no
        // `body()` to rebase from, so the axis holds its own value
        // and the tap steps it up from there.
        app.throttle_axis = 0.4;
        app.dispatch(KeyAction::ThrottleUp);
        assert!(
            (app.throttle_axis - 0.41).abs() < 1e-9,
            "axis squashed to the tap base: {}",
            app.throttle_axis
        );
        app.throttle_release(KeyAction::ThrottleUp);
    }

    /// P14-F2a: a settings file written before the held-throttle /
    /// stage / recover / roll verbs existed leaves them absent from
    /// `bindings` — the load must backfill the shipped defaults so
    /// upgraders don't get six dead verbs, while a present-but-empty
    /// row stays the user's deliberate unbind.
    #[test]
    fn load_backfills_verbs_the_file_predates() {
        let dir = tempdir("backfill");
        let path = dir.join("settings.ron");
        let mut s = Settings::default();
        // Simulate the pre-M10-verbs file: six actions absent, and a
        // deliberately-cleared WarpUp that must stay unbound.
        for a in [
            KeyAction::ThrottleUp,
            KeyAction::ThrottleDown,
            KeyAction::Stage,
            KeyAction::RecoverControl,
            KeyAction::RollLeft,
            KeyAction::RollRight,
        ] {
            s.keys.bindings.remove(&a);
        }
        s.keys.bindings.insert(KeyAction::WarpUp, Vec::new());
        settings_io::save_settings_to(&path, &s).unwrap();
        let (loaded, notes) = settings_io::load_settings_from(&path);
        for a in [KeyAction::ThrottleUp, KeyAction::Stage, KeyAction::RollLeft] {
            assert!(
                loaded.keys.bindings.get(&a).is_some_and(|v| !v.is_empty()),
                "{a:?} stayed unbound"
            );
        }
        assert!(loaded.keys.action_for("space") == Some(KeyAction::Stage));
        // …but the deliberate unbind is honored — absent ≠ empty.
        assert_eq!(loaded.keys.bindings[&KeyAction::WarpUp], Vec::new());
        assert!(
            notes.iter().any(|n| n.contains("no binding")),
            "backfill must leave a visible note: {notes:?}"
        );
    }

    /// P14-F3: a hand-edited `NaN` literal parses through RON and
    /// `f32::clamp` lets it straight through — the load must reject
    /// it before `set_zoom_factor`/`master_gain` can see it.
    #[test]
    fn load_rejects_nonfinite_floats() {
        let dir = tempdir("nonfinite");
        let path = dir.join("settings.ron");
        std::fs::write(
            &path,
            "(graphics: (ui_scale: NaN), audio: (master_volume: inf))",
        )
        .unwrap();
        let (s, notes) = settings_io::load_settings_from(&path);
        assert!(s.graphics.ui_scale.is_finite());
        assert_eq!(s.audio.master_volume, 0.8); // shipped default
        assert!(
            notes.iter().any(|n| n.contains("not a finite number")),
            "nonfinite rejection must leave a note: {notes:?}"
        );
    }

    /// P14-F2b: a file with one variant this build doesn't know — the
    /// downgrade case — used to reset EVERYTHING with only a
    /// `log::warn`. The lenient load keeps every field and binding row
    /// that still parses and reports the dropped row.
    #[test]
    fn corrupt_keymap_salvages_readable_fields() {
        let dir = tempdir("salvage");
        let path = dir.join("settings.ron");
        std::fs::write(
            &path,
            r#"(
                audio: (master_volume: 0.5),
                keys: (bindings: {r#warp-up: ["f7"], stage: ["space"], r#future-verb: ["f9"]}),
            )"#,
        )
        .unwrap();
        let (s, notes) = settings_io::load_settings_from(&path);
        // The unknown verb row is dropped; the readable rows and
        // other sections survive; missing verbs backfill.
        assert_eq!(s.keys.action_for("f7"), Some(KeyAction::WarpUp));
        assert_eq!(s.keys.action_for("space"), Some(KeyAction::Stage));
        assert!((s.audio.master_volume - 0.5).abs() < 1e-6);
        assert!(s.keys.action_for("shift") == Some(KeyAction::ThrottleUp));
        assert!(
            notes.iter().any(|n| n.contains("doesn't know")),
            "the dropped row must be named: {notes:?}"
        );
        assert!(
            notes.iter().any(|n| n.contains("not fully readable")),
            "the salvage itself must be disclosed: {notes:?}"
        );
    }

    /// C15-lib2-F1: a focus loss drops the pointer/button releases
    /// `map_dragging` and `lmb_press` live off — the same argument the
    /// held-key clears always used. A stale `map_dragging` made the
    /// camera orbit on refocus with no button held; a stale
    /// `lmb_press` armed a click candidate whose press never landed.
    #[test]
    fn focus_lost_clears_drag_and_click_candidate() {
        let dir = tempdir("focus-loss");
        let mut app = App::new_with_settings_path(&dir.join("settings.ron"));
        // Stage every state `focus_lost` owns.
        app.map_dragging = true;
        app.lmb_press = Some(Some((10.0, 10.0)));
        app.captured_key = Some(winit::keyboard::PhysicalKey::Code(
            winit::keyboard::KeyCode::KeyW,
        ));
        app.throttle_held.insert(KeyAction::ThrottleUp);
        app.throttle_hold_s = 0.4;
        app.throttle_hold_vessel = app.view.flight_focus();
        app.focus_lost();
        assert!(!app.map_dragging, "stale drag survives focus loss");
        assert_eq!(app.lmb_press, None, "stale click candidate survives");
        assert_eq!(app.captured_key, None);
        assert!(app.throttle_held.is_empty());
        assert_eq!(app.throttle_hold_s, 0.0);
        assert_eq!(app.throttle_hold_vessel, None);
    }

    /// C15-lib2-F2: a throttle key held through a flight-focus change
    /// must NOT carry the old vessel's commanded axis onto the new
    /// focus — the hold rebases (the ease-in clock restarts, the axis
    /// takes the new vessel's applied value when it has one) and the
    /// retarget lands on the cue channel. The real triggers are
    /// recover-control pinning and the newest-staged fallback; the
    /// test stages the state directly — a recorded vessel that is not
    /// the live focus — and runs one ramp tick.
    #[test]
    fn throttle_hold_rebases_on_focus_change() {
        let dir = tempdir("throttle-retarget");
        let mut app = App::new_with_settings_path(&dir.join("settings.ron"));
        // Settle the seeded pad craft so `flight_focus` is live and
        // `throttle_blocker` is clear (a latched pad craft commands).
        app.view.begin_frame();
        app.view.sim_step(320).unwrap();
        app.view.end_frame();
        let vessel = app.view.flight_focus().unwrap();
        app.dispatch(KeyAction::ThrottleUp);
        assert_eq!(app.throttle_hold_vessel, Some(vessel));
        // Simulate the focus moving: the recorded vessel stays behind.
        app.throttle_hold_vessel = Some(csp_view::VesselId(u64::MAX));
        app.throttle_axis = 0.9;
        app.throttle_hold_s = 2.0;
        app.view.begin_frame();
        app.tick_throttle_ramp(1.0 / 60.0);
        app.view.end_frame();
        assert_eq!(
            app.throttle_hold_vessel,
            Some(vessel),
            "the hold must rebind to the live focus, not keep the stale id"
        );
        assert_eq!(
            app.throttle_hold_s,
            1.0 / 60.0,
            "the ease-in clock restarts on the rebase frame"
        );
        {
            let sfx = app.view.world.resource::<csp_view::flight::FlightSfx>();
            assert!(
                sfx.cues.iter().any(|c| c.text.contains("REBASED")),
                "a focus retarget must surface a notice"
            );
        }
        // And the hold still commands the LIVE vessel — the stale id
        // never reaches the §7.6 log.
        assert!(app.view.sim.command_log().iter().all(|(_, c)| match c {
            SimCommand::SetThrottle { vessel: v, .. } => *v == vessel,
            _ => true,
        }));
    }

    /// C14-F4: Esc is bound to `OpenSettings` AND is the VAB's
    /// universal cancel. While the editor has a carried part or a
    /// selection the press resolves as the cancel (the egui pass's
    /// `CancelPlacement`/`Select(None)`), and the settings toggle
    /// yields — one press, one action. With nothing armed the same
    /// key opens settings.
    #[test]
    fn esc_cancels_vab_carry_before_toggling_settings() {
        let dir = tempdir("esc-route");
        let mut app = App::new_with_settings_path(&dir.join("settings.ron"));
        app.view.set_mode(ViewMode::Vab);
        // Arm a placement — the seeded VAB craft supplies a real part id.
        app.view.with_vab_session(|v| {
            let root = v.session().root().expect("seeded VAB craft");
            let part = v.session().node(root).unwrap().built().part_id.clone();
            v.set_drag_part(Some(part));
            assert!(v.carried().is_some());
        });
        app.dispatch(KeyAction::OpenSettings);
        assert!(
            !app.view.settings_panel_open(),
            "Esc while carrying must cancel, not toggle settings"
        );
        // Disarm the carry but keep a selection armed — the same
        // cancel-yields-to-Esc rule covers it.
        app.view.with_vab_session(|v| {
            v.apply(csp_ui::vab::VabAction::CancelPlacement).unwrap();
            let root = v.session().root().unwrap();
            v.apply(csp_ui::vab::VabAction::Select(Some(root))).unwrap();
            assert!(v.selected().is_some());
        });
        app.dispatch(KeyAction::OpenSettings);
        assert!(
            !app.view.settings_panel_open(),
            "Esc with a selection armed must deselect, not toggle settings"
        );
        // Fully disarmed — the next Esc reaches the settings toggle.
        app.view.with_vab_session(|v| {
            v.apply(csp_ui::vab::VabAction::Select(None)).unwrap();
            assert!(v.carried().is_none());
            assert!(v.selected().is_none());
        });
        app.dispatch(KeyAction::OpenSettings);
        assert!(
            app.view.settings_panel_open(),
            "Esc with nothing armed still opens settings"
        );
    }

    /// The reported break's mechanism: a throttle axis left at the
    /// rail by the previous flight makes throttle-up a permanently
    /// dead key on the next delivered craft — the tap no-ops at the
    /// rail, so no `SetThrottle` ever reaches the latched vessel and
    /// it can never unlatch. The axis must rebase on the focus change.
    #[test]
    fn stale_axis_at_full_rebases_on_the_delivered_craft() {
        let dir = tempdir("stale-axis");
        let mut app = App::new_with_settings_path(&dir.join("settings.ron"));
        let frame = |app: &mut App, ticks: u64| {
            let target = app.view.sim.clock().ticks() + ticks;
            app.view.begin_frame();
            app.tick_throttle_ramp(1.0 / 60.0);
            app.view.sim_step(target).unwrap();
            app.view.end_frame();
        };
        frame(&mut app, 8);
        app.handle_ui_outbox(csp_view::UiOutbox {
            launch: true,
            ..Default::default()
        });
        frame(&mut app, 1);
        let deadline = app.view.sim.builds().values().next().unwrap().complete_tick;
        while app.view.sim.clock().ticks() < deadline {
            frame(&mut app, 64);
        }
        frame(&mut app, 8);
        let focus = app.view.flight_focus().unwrap();
        // The state a full-throttle climb leaves behind — the axis
        // still describes the OLD (recovered) vessel.
        app.throttle_axis = 1.0;
        app.throttle_axis_vessel = Some(csp_view::VesselId(u64::MAX));
        app.dispatch(KeyAction::ThrottleUp);
        assert!(
            app.throttle_axis < 1.0,
            "the axis must rebase to the delivered craft's applied throttle (0)"
        );
        frame(&mut app, 1); // drain the player FIFO
        let logged =
            app.view.sim.command_log().iter().any(
                |(_, c)| matches!(c, SimCommand::SetThrottle { vessel, .. } if *vessel == focus),
            );
        assert!(
            logged,
            "throttle-up must land a SetThrottle — the unlatch command"
        );
    }

    /// The real player flow end to end: the seeded pad craft (itself
    /// a delivered build), a VAB LAUNCH that recovers the parked
    /// build and orders a new one, the `ConstructionComplete`
    /// delivery, then the Z snap — the delivered craft must unlatch,
    /// open the throttle, and climb.
    #[test]
    fn delivered_craft_flies_on_throttle() {
        let dir = tempdir("delivered-flies");
        let mut app = App::new_with_settings_path(&dir.join("settings.ron"));
        let frame = |app: &mut App, ticks: u64| {
            let target = app.view.sim.clock().ticks() + ticks;
            app.view.begin_frame();
            app.tick_throttle_ramp(1.0 / 60.0);
            app.view.sim_step(target).unwrap();
            app.view.end_frame();
        };
        frame(&mut app, 8); // settle the seeded pad craft
        // The LAUNCH click — the parked build recovers, the order lands.
        app.handle_ui_outbox(csp_view::UiOutbox {
            launch: true,
            ..Default::default()
        });
        frame(&mut app, 1);
        assert_eq!(app.view.sim.builds().len(), 1, "the launch ordered a build");
        let deadline = app.view.sim.builds().values().next().unwrap().complete_tick;
        while app.view.sim.clock().ticks() < deadline {
            frame(&mut app, 64);
        }
        frame(&mut app, 8); // let the delivery stage
        let focus = app.view.flight_focus().expect("the delivery took focus");
        assert!(
            matches!(
                app.view.sim.vessels()[&focus].state,
                csp_view::VesselState::SurfaceLatched(_)
            ),
            "delivered surface-latched"
        );
        // Z — the snap-to-full throttle key.
        app.dispatch(KeyAction::ThrottleFull);
        frame(&mut app, 1);
        let body = app.view.sim.vessels()[&focus]
            .body()
            .expect("throttle unlatched the delivered craft");
        assert!(body.throttle > 0.0, "the throttle command applied");
        let r0 = body.pos.norm();
        for _ in 0..4 {
            frame(&mut app, 60);
        }
        let body = app.view.sim.vessels()[&focus]
            .body()
            .expect("still InBubble");
        assert!(
            body.pos.norm() > r0 + 1.0,
            "the delivered craft climbs: r {r0} -> {}",
            body.pos.norm()
        );
    }

    /// The still-broken half of the stale-axis bug, on the REAL key
    /// path: a flight that left `throttle_axis` at the rail lands and
    /// the same vessel re-latches — the latch destroys the body the
    /// axis's claim rode on, so the axis is stale *for the same
    /// vessel* (`throttle_axis_vessel == focus` — the cross-vessel
    /// rebase does not fire). Every Shift press then clamped
    /// `1.0 + tap → 1.0`, saw no axis movement, and enqueued nothing:
    /// throttle-up was a permanently dead key. A `press_edge` of the
    /// real Shift `PhysicalKey`/`Key` pair must deliver the axis's
    /// undelivered value — `SetThrottle` is the unlatch — and the
    /// craft must fly again.
    #[test]
    fn shift_press_unlatches_the_relanded_craft() {
        use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};
        let dir = tempdir("relanded-throttle");
        let mut app = App::new_with_settings_path(&dir.join("settings.ron"));
        let frame = |app: &mut App, ticks: u64| {
            let target = app.view.sim.clock().ticks() + ticks;
            app.view.begin_frame();
            app.tick_throttle_ramp(1.0 / 60.0);
            app.view.sim_step(target).unwrap();
            app.view.end_frame();
        };
        let shift = (
            PhysicalKey::Code(KeyCode::ShiftLeft),
            Key::Named(NamedKey::Shift),
        );
        frame(&mut app, 320); // settle the seeded pad craft to its latch
        let vessel = app.view.flight_focus().unwrap();
        assert!(app.view.sim.vessels()[&vessel].body().is_none());
        // The stale state a full-throttle flight that LANDED leaves
        // behind: the axis still describes THIS vessel, still reads
        // the rail — but the latch destroyed the body the claim rode
        // on (`throttle_axis_vessel == focus`, so the cross-vessel
        // rebase never fires — this is the hole dd3612a left).
        app.throttle_axis = 1.0;
        app.throttle_axis_vessel = Some(vessel);
        // The real Shift key-press edge → the tap must STILL enqueue:
        // the rail-clamped axis's claim was never delivered to the
        // re-latched body (§2.6.2 zeroes latched throttle), and the
        // `SetThrottle` is the unlatch command.
        app.press_edge(&shift.0, &shift.1, false);
        assert!(app.throttle_held.contains(&KeyAction::ThrottleUp));
        frame(&mut app, 1); // drain the player FIFO
        let logged = app.view.sim.command_log().iter().any(|(_, c)| {
            matches!(c, SimCommand::SetThrottle { vessel: v, throttle } if *v == vessel && *throttle > 0.0)
        });
        assert!(logged, "the Shift press must land a SetThrottle");
        let body = app.view.sim.vessels()[&vessel]
            .body()
            .expect("the throttle command unlatched the craft");
        assert_eq!(body.throttle, 1.0, "the axis's rail value applied");
        let r0 = body.pos.norm();
        // Held through the liftoff frames — the kept-alive hold keeps
        // ramp-frames issuing; the engine must carry it off the pad.
        for _ in 0..4 {
            frame(&mut app, 60);
        }
        let body = app.view.sim.vessels()[&vessel]
            .body()
            .expect("still InBubble");
        assert!(
            body.pos.norm() > r0 + 1.0,
            "the relanded craft flies again: r {r0} -> {}",
            body.pos.norm()
        );
        // The real release edge clears the held axis.
        app.release_edge(&shift.0, &shift.1);
        assert!(app.throttle_held.is_empty());
    }

    /// The guard the rail-deliver must not blow through: a
    /// throttle-DOWN tap on a parked craft whose axis is already 0
    /// commands nothing — a `SetThrottle{0}` would unlatch the parked
    /// craft into an inert bubble body for literally no effect. And
    /// a rail no-op on a bubble-resident craft (axis == applied)
    /// stays a no-op — no repeat-command spam.
    #[test]
    fn rail_noop_stays_quiet_when_throttle_is_already_delivered() {
        let dir = tempdir("rail-quiet");
        let mut app = App::new_with_settings_path(&dir.join("settings.ron"));
        let frame = |app: &mut App, ticks: u64| {
            let target = app.view.sim.clock().ticks() + ticks;
            app.view.begin_frame();
            app.tick_throttle_ramp(1.0 / 60.0);
            app.view.sim_step(target).unwrap();
            app.view.end_frame();
        };
        frame(&mut app, 320);
        let vessel = app.view.flight_focus().unwrap();
        let logged = |app: &App| {
            app.view
                .sim
                .command_log()
                .iter()
                .filter(|(_, c)| matches!(c, SimCommand::SetThrottle { .. }))
                .count()
        };
        // Ctrl at axis 0 on the parked craft: the axis didn't move and
        // there's nothing to deliver — no unlatch-for-nothing command.
        let before = logged(&app);
        app.dispatch(KeyAction::ThrottleDown);
        app.throttle_release(KeyAction::ThrottleDown);
        frame(&mut app, 1);
        assert_eq!(logged(&app), before, "a 0-axis down tap must not unlatch");
        assert!(app.view.sim.vessels()[&vessel].body().is_none());
        // Up to full, fly a tick — the craft holds throttle 1.0 in the
        // bubble, the axis == applied: a held-Shift frame at the rail
        // enqueues nothing more.
        app.dispatch(KeyAction::ThrottleFull);
        frame(&mut app, 30);
        assert!(app.view.sim.vessels()[&vessel].body().is_some());
        let before = logged(&app);
        app.dispatch(KeyAction::ThrottleUp);
        for _ in 0..3 {
            frame(&mut app, 30);
        }
        assert_eq!(
            logged(&app),
            before,
            "rail no-op on an in-bubble craft must not spam commands"
        );
    }
}
