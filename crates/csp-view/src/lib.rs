//! bevy_ecs presentation world and the six named schedules (PLAN §2.5).
//!
//! Authoritative sim state never lives in this `World` — it mirrors `csp-sim`
//! for presentation. `bevy_ecs` is confined to this crate
//! (`default-features = false`), so replacing it later is cheap.
//!
//! Frame contract (§2.5, Appendix A items 1–3): `csp-app` drives
//! [`View::begin_frame`] (Input), [`View::sim_step`] (SimStep — one
//! `step_frame` call, always), [`View::end_frame`] (Sync → Presentation →
//! UiBuild → RenderExtract), and consumes [`View::take_ui_frame`] to paint.
//! `csp-app` never reads `view.world` to build UI — input arrives through
//! the plumbing methods below, output leaves through `take_*`.

use bevy_ecs::prelude::*;
use bevy_ecs::schedule::SingleThreadedExecutor;
use csp_core::{SimError, SimTime};
pub use csp_sim::World as SimWorld;

/// The M1 map view (PLAN §5.1 R0 + the M1 exit row): mode/camera state,
/// orbit polylines, markers, and the `csp_render::map::MapPacket` the
/// `RenderExtract` schedule produces. Session state only — the sim is
/// read through `View::end_frame` staging, never by systems (§2.5).
pub mod flight;
pub mod map;

/// The R2 surface tier (PLAN §5.2): per-body terrain config (authored
/// `TerrainDef` seeds + ocean datums + biome palettes), the baked
/// heightfield cache, and the `TerrainPacket` the flight view emits.
pub mod terrain;

/// The launch-site space center (PLAN §5.2's static structures): the
/// authored `sites.ron` table as a `SiteTable` resource plus the pad /
/// gantry / VAB / support-building geometry `extract_flight` emits
/// into the flight vertex stream at the rendered terrain height — and
/// the launch-site marker the map view draws.
pub mod structures;

/// The M3 VAB (editor mode): working-craft session hosting, the craft
/// view's scene/camera/packet, and the GPU-pick event plumbing.
pub mod vab;

/// Per-part meshes for the VAB and flight packet producers: the real
/// baked models (`art`) emitted through [`part_mesh::emit_model`],
/// with the collider-profile procedural silhouettes as the
/// art-less fallback and the basis for selection shells.
pub mod part_mesh;

/// Real art assets — the `.csar` packs baked from CC0 sources
/// (`assets/ASSETS.md`), decoded once at startup into the
/// [`art::ArtAssets`] resource (part meshes + surface textures).
pub mod art;

/// The §6.3 web-demo tier: the embedded reduced `data/` set and the
/// fixed demo-scenario seed (Terrin only, ≤60 parts, no save
/// import/export). The wasm `View::new` builds exactly this; the module
/// is target-agnostic so native tests exercise the same construction.
pub mod demo;

/// M10 — the first-flight tutorial driver (PLAN M10 exit row): the
/// [`tutorial::TutorialRes`] session resource and the Presentation
/// schedule system folding staged mirrors into the checklist's
/// progress predicates (the model lives in `csp_ui::tutorial`).
pub mod tutorial;

/// PLAN §5.4's depth-strategy probe (R1 — C16-F5): the headless render
/// measurement that records the 10⁷ m / 1 m-object depth-fight count
/// the perf log's `render.depth-fight-1e7` row carries. Native-only —
/// it rasterizes through a caller-supplied device on the shipped
/// flight pipeline.
#[cfg(not(target_arch = "wasm32"))]
pub mod depth_probe;

/// Pad-spawn clearance (m): the lowest collider vertex starts this far
/// above the datum. Kept at zero — a nonzero drop, even centimetres,
/// reads as a one-tick `Δv/dt` load spike and fails weak joints (the
/// shipped "PART JETTISONED at t≈2 s" bug); resting exactly on the
/// contact sphere means the solver holds the craft from tick one.
const PAD_CLEARANCE_M: f64 = 0.0;

/// §2.2.1 `source_id` for the local player's direct craft verbs —
/// the launch path's `SpawnPhysicalVessel` here and `csp-app`'s
/// throttle dispatch (`SetThrottle`) there. Two producers, one FIFO
/// *on purpose*: every player-initiated craft command drains in
/// issue order. `csp-app`'s unrelated Ui-class producers take their
/// own ids (warp/maneuver/settings are 0/2/3 — never reused).
pub const PLAYER_SOURCE_ID: u32 = 1;

/// The app's view mode — flight, map, or the VAB editor. Session
/// state (§2.2-C): never sim state, never logged, never hashed.
/// `csp-app` routes input and `csp-view` derives it from the two
/// subsystem flags ([`vab::VabInput::open`] wins over the map's mode).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ViewMode {
    /// The flight view.
    #[default]
    Flight,
    /// The system map (`map::MapInput.mode == Map`).
    Map,
    /// The VAB editor (`vab::VabInput.open`).
    Vab,
}

/// The §3.1 data-set hash + its scheme version — the bug-report bundle's
/// fingerprint fields (§2.14) and §7.7's drift-warning key — plus the
/// §7.7 per-kind breakdown that lets the warning "list what changed".
pub use csp_data::{
    DATA_HASH_VERSION,
    datahash::{self, data_set_hash, data_set_hashes, data_set_kind_hashes},
};
/// The §7.2 serialized set — the payload [`save`] wraps. Re-exported on
/// the same §2.1 edge-sharing rationale.
pub use csp_sim::WorldSnapshot;
/// M10: the versioned save format + migration machinery (`csp_sim::save`)
/// — re-exported so `csp-app`'s bug-report bundle and save path can
/// encode/decode without a `csp-sim` edge of its own (§2.1).
pub use csp_sim::save;
/// `SimCommand` vocabulary, re-exported so `csp-app` can produce commands
/// without depending on `csp-sim` directly — §2.1's rule that `csp-view` is
/// the only crate depending on BOTH `csp-sim` and `csp-render` stays true.
/// `AdvanceReport`/`EventClass` ride along because `csp-app` reads the
/// fired-event classes for §2.6.5 Clamping demotion; `VesselState`
/// because its input layer classifies the focused vessel's residence
/// (bubble / surface-latched / rails) to name the reason a control verb
/// cannot apply.
pub use csp_sim::{AdvanceReport, EventClass, SimCommand, SourceClass, VesselState};
pub use csp_sim::{VesselId, events, orbit};
// Re-exported so `csp-app` names mode requests without a csp-ui edge.
/// The control-engine pair `from_source` installs — re-exported so
/// hosts (`csp --serve`'s reconfigure hook, `session.load`) can name
/// the exact same engines without a direct csp-script/csp-ai edge.
pub use csp_ai::AscentAutopilot;
pub use csp_script::RhaiHost;
pub use csp_ui::ModeSelect;
pub use csp_ui::game;

/// M10: the settings model + panel (`csp_ui::settings`) — re-exported so
/// `csp-app` owns the file IO without a `csp-ui` edge of its own.
pub use csp_ui::settings;

/// Integer sim clock mirrored for presentation — a COPY, written each frame
/// by the `sync` schedule's [`sync_mirrors`] system (PLAN §2.2, Appendix A
/// item 1). Never authoritative.
#[derive(Resource, Default)]
pub struct Clock(pub SimTime);

/// The frame's fractional tick remainder — how far past [`Clock`] the
/// frame is, in ticks, `[0, 1)`. `csp-app`'s warp controller is the one
/// source ([`View::set_tick_fraction`], every frame); the schedules
/// read it here and never touch the clock's fraction themselves. It is
/// what `flight_interp` interpolates the staged poses with (§2.5). The
/// default `0` carries no fractional time: every pose is drawn exactly
/// as staged at the clock's tick. Presentation data only — never read
/// by the sim, never part of any hash.
#[derive(Resource, Default)]
pub struct TickFraction(pub f64);

/// Proof-of-life stats shown in the debug panel — written by [`sync_mirrors`]
/// from the last [`AdvanceReport`](csp_sim::AdvanceReport), never by the sim
/// path directly (Appendix A item 1).
#[derive(Resource, Default)]
pub struct FrameStats {
    /// Frames rendered since startup.
    pub frames: u64,
    /// Ticks the last `step_frame` advanced (0 on a halted or zero-advance
    /// frame).
    pub ticks_last_frame: u64,
}

/// The sim-stored warp level, mirrored verbatim by [`sync_mirrors`] (§2.6.3:
/// stored state, in the hash; interpreting it as a ladder rung/multiplier is
/// `csp-app`'s job).
#[derive(Resource)]
pub struct WarpMirror(pub u32);

impl Default for WarpMirror {
    /// The sim boots at ×1 (real time — §2.6.2's always-allowed regime), so
    /// the mirror starts there rather than at 0.
    fn default() -> Self {
        Self(1)
    }
}

/// The §2.6.6-item-4 SOI scan report for the resolved-focus vessel —
/// mirrored each frame by [`sync_mirrors`] out of `SyncInput`, which
/// `end_frame` stages from [`SimWorld::soi_scan_strides`]. The debug
/// panel prints these `h` values verbatim: they are the scan's own
/// strides, so the overlay can never drift from the scan (the reason
/// [`orbit::PatchedConics::scan_strides`] shares the scan's helpers).
/// Never authoritative — a COPY like every mirror (Appendix A item 1).
#[derive(Resource, Default)]
pub struct SoiScanMirror(pub csp_ui::SoiScanReport);

/// Session toggles for the player-optional panels (§2.2-C session
/// data — never sim state): the M0 debug scaffold (adapter/tick
/// internals — dev-facing, off by default) and the controls-help card
/// (off by default; `ToggleHelp` key or the top-bar button).
#[derive(Resource, Default)]
pub struct PanelFlags {
    /// Show the debug scaffold panel.
    pub debug: bool,
    /// Show the controls-help card.
    pub help: bool,
    /// Show the Program window (progression: research/science/
    /// contracts/reliability) — top-bar "Program" button.
    pub program: bool,
}

/// The localization table (`data/loc/en.ron`) as a resource so
/// `stage_program` can resolve tech-node/track/part display names —
/// data-derived constants like the part catalog (§7.3). `with_sim`
/// worlds get the empty default and labels fall back to raw ids.
#[derive(Resource, Default)]
pub struct LocStrings(pub std::collections::BTreeMap<String, String>);

/// A body's decoded §3.0.1 biome map — palette indices + the legend's
/// `index → BiomeId` table. Presentation-side only: the sim never
/// reads pixels (the `RunExperiment` caller supplies the biome).
#[derive(Clone, Debug)]
pub struct BiomeLookup {
    /// Texels per row.
    pub width: u32,
    /// Rows (equirect north→south).
    pub height: u32,
    /// `width·height` raw PNG palette indices, row-major.
    pub indices: Vec<u8>,
    /// Palette index → `BiomeId`.
    pub legend: std::collections::BTreeMap<u8, csp_data::BiomeId>,
}

/// Decoded biome maps keyed by body id — built by `from_source` from
/// the loaded system; empty for `with_sim` worlds (space contexts and
/// map-less bodies use the empty `BiomeId` anyway).
#[derive(Resource, Default)]
pub struct BiomeMaps(pub std::collections::BTreeMap<String, BiomeLookup>);

/// Body radius table keyed by body id — needed to turn a rails
/// vessel's osculating radius into an altitude for the science
/// situation bands. Data-derived constants.
#[derive(Resource, Default)]
pub struct BodyRadii(pub std::collections::BTreeMap<String, f64>);

/// §3.2 situation bands (presentation-side math — the sim's module
/// doc: "the situation boundary definitions are presentation-side").
/// Flying low/high split above the datum — the KSP Kerbin figure.
pub const FLYING_SPLIT_M: f64 = 18_000.0;
/// See [`FLYING_SPLIT_M`] — low/high space split above the datum.
pub const SPACE_SPLIT_M: f64 = 250_000.0;

/// One pooled science record kept sim-side for command resolution —
/// the csp-ui row references it by `index` (`DataKey` may not cross
/// the §2.1 boundary into csp-ui).
#[derive(Clone, Debug)]
pub struct PooledKey {
    /// The science key `TransmitData`/`RecoverData` name.
    pub key: csp_sim::research::DataKey,
}

/// The Program window's staged mirror — populated by
/// [`View::stage_program`] in `end_frame` (the `stage_vab_tech`
/// precedent: a direct sim→resource write in the staging phase,
/// never read by systems as sim state). `csp_ui::program::ProgramInput`
/// is built from this inside `build_ui`; `keys`/`science_ctx` are the
/// sim-side facts the emitted commands need back.
#[derive(Resource, Default)]
pub struct ProgramMirror {
    /// The csp-ui input rows.
    pub input: csp_ui::program::ProgramInput,
    /// The focused vessel the science rows belong to.
    pub vessel: Option<csp_sim::VesselId>,
    /// The staged context for `RunExperiment`.
    pub science_ctx: Option<(csp_data::BiomeId, csp_data::Situation)>,
    /// The pooled `DataKey`s the `DataRow.index` values address.
    pub keys: Vec<PooledKey>,
    /// The site a `RecoverVessel` command names — staged when the
    /// focused vessel is `SurfaceLatched`: the launch site on the
    /// latched body when one exists (recovery_efficiency 1.0), else
    /// the body's first site, else `None` (the `abandon_recovery`
    /// remote-salvage leg — still a valid recovery).
    pub recovery_site: Option<csp_data::SiteId>,
    /// The sim clock at staging — `SetAlarm`/`ScheduleColonyInspection`
    /// resolve their ticks against it.
    pub now_ticks: u64,
    /// The focused vessel's crew resolved to `(CrewId, skill)` — the
    /// researcher pool `StartResearch` truncates to the lab's
    /// `researcher_slots`. Roster members carry their `skill`; id-only
    /// `BoardCrew` members stage at skill 1 (a zero total rate rejects
    /// at apply — an unskilled seat-warmer still runs a lab, slowly).
    pub researchers: Vec<(csp_sim::CrewId, u8)>,
    /// The focused vessel's lab nodes: `PartNodeId` → researcher slots.
    pub lab_slots: std::collections::BTreeMap<u64, u32>,
}

/// The flight part-action menu's session state (§2.2-C) — the
/// `csp_ui::program::PartMenuState` model wrapped as a resource, the
/// same convention as [`ManeuverPanelRes`]/[`SettingsPanelRes`]: the
/// right-click pick writes it, `part_menu` mutates it, and
/// `build_ui` closes it when its vessel leaves the program mirror's
/// focus.
#[derive(Resource, Default)]
pub struct PartMenuRes(pub csp_ui::program::PartMenuState);

/// The rolling `script_log` tail (W2.5) — session/presentation state,
/// never sim state. `AdvanceReport::script_log` is consumed per step
/// (mem::take'd out of the world), so `csp-app` folds each report's
/// lines back here through [`View::push_script_log`]; the Program
/// window's AUTOMATION section renders the tail and the flight
/// notices flash the lines live.
#[derive(Resource, Default)]
pub struct ScriptLogMirror {
    /// `(tick, "script N: line")` — newest last, capped at 64.
    pub lines: std::collections::VecDeque<(u64, String)>,
}

/// Session-side values `csp-app` mirrors in for the UI — written through
/// [`View::set_gpu_info`]/[`View::update_session`] before the schedules run.
/// Presentation/session data only (§2.2 C): never logged, never hashed.
#[derive(Resource, Default)]
pub struct SessionMirror {
    /// GPU adapter name (debug HUD).
    pub adapter: String,
    /// wgpu backend label (debug HUD).
    pub backend: String,
    /// Wall-clock frame delta, ms. Display-only — wall time never reaches
    /// the sim (§2.5).
    pub dt_wall_ms: f64,
    /// Warp multiplier the player asked for (controller state).
    pub warp_requested: u64,
    /// Warp multiplier the controller emits after §2.6.3 auto-derate.
    pub warp_effective: u64,
    /// §2.13 halt state — mirrored so the panel can show "SIM HALTED".
    pub fatal: Option<SimError>,
    /// The bug-report bundle path (or write-failure note) the Fatal
    /// handler recorded — the dialog's "send this to the developer"
    /// line. `None` until a Fatal lands or while the write is pending.
    pub fatal_bundle: Option<String>,
    /// The app's *live* (applied) keymap — mirrored by
    /// [`View::set_live_keymap`]. The controls card and every hint bar
    /// print *this* map — never the settings window's staged
    /// pre-Apply copy — so the on-screen key names are always the ones
    /// that actually dispatch.
    pub keys: csp_ui::settings::Keymap,
    /// Observed physical-token → printed glyph map (`"key-w" → "z"`
    /// on AZERTY) — recorded by the input layer as keys are pressed
    /// ([`View::set_key_label`]), so hint bars show what the keys say
    /// on *this* machine's layout, not the QWERTY fallback.
    pub key_labels: std::collections::BTreeMap<String, String>,
}

/// The egui pass's input/output — the resource bridge that lets the *whole*
/// egui pass live inside the `UiBuild` schedule (§2.5, Appendix A item 3).
///
/// - `ctx`: the `egui::Context` (Arc-interned, cheap to clone — `csp-app`
///   borrows a clone only to tessellate `output` for painting).
/// - `input`: this frame's `egui::RawInput`, staged by `csp-app` from
///   `egui_winit::State::take_egui_input` — the winit→egui bridge stays in
///   `csp-app`; the pass itself is the [`build_ui`] system.
/// - `output`: the completed `egui::FullOutput`, consumed by `csp-app`'s
///   painter (platform output, tessellation, textures delta).
#[derive(Resource, Default)]
pub struct EguiPass {
    /// The egui context — lives here so `UiBuild` can run the pass.
    pub ctx: egui::Context,
    /// Staged raw input for the current frame.
    pub input: egui::RawInput,
    /// The completed pass — `Some` after `UiBuild` ran this frame.
    pub output: Option<egui::FullOutput>,
}

impl Drop for EguiPass {
    /// `TexturesDelta` is `#[must_use]` — epaint debug-asserts when one is
    /// dropped unapplied. A frame whose output nobody consumed (app exit,
    /// headless use) clears it here rather than tripping the assert.
    fn drop(&mut self) {
        if let Some(output) = &mut self.output {
            output.textures_delta.clear();
        }
    }
}

/// Requests the UI emitted during `UiBuild` — drained by `csp-app` at the
/// *next* frame's Input phase, the §2.5 "UI-issued commands take effect on
/// the next tick — one-tick latency, accepted" hand-off.
#[derive(Resource, Clone, Debug, Default)]
pub struct UiOutbox {
    /// The VAB's LAUNCH button — `csp-app` calls
    /// [`View::launch_craft`] on the next Input phase.
    /// Net warp-ladder steps the panel requested this pass.
    pub warp_step: i32,
    /// A top-bar mode request.
    pub mode: Option<csp_ui::ModeSelect>,
    /// The VAB's LAUNCH button fired — `csp-app` calls
    /// [`View::launch_craft`].
    pub launch: bool,
    /// Save the working craft under this name — `(name, bytes)`.
    pub craft_save: Option<(String, Vec<u8>)>,
    /// Load the named craft file.
    pub craft_load: Option<String>,
    /// M5 node panel output — the `SetManeuver`/`DeleteManeuver`
    /// command `csp-app` enqueues at next Input (§2.5's one-frame
    /// UiBuild → Input hand-off, same as every other request here).
    pub maneuver: Option<SimCommand>,
    /// Program-window output — the `RunExperiment`/`TransmitData`/
    /// `RecoverData`/`ResearchTech`/`ResearchUpgradeLevel`/
    /// `AcceptContract` commands the panel emitted, already resolved
    /// into `SimCommand`s against the [`ProgramMirror`] context.
    pub program: Vec<SimCommand>,
    /// The settings panel's "Apply" — `csp-app` copies the staged
    /// settings into its live set and persists them.
    pub settings_apply: bool,
    /// The Game menu's save verb — write the world snapshot to the
    /// named slot (`"quicksave"` for the F5 path).
    pub game_save: Option<String>,
    /// The Game menu's load verb — restore the named slot's world.
    pub game_load: Option<String>,
    /// Revert-to-launch — restore the pre-launch snapshot
    /// [`View::pre_launch`] captured at `launch_craft`.
    pub revert: bool,
    /// Abandon the session and start a fresh world.
    pub new_game: bool,
}

/// The Game menu's session state — the slot list `csp-app` stages,
/// the menu widget's own state, and the revert mirror staged each
/// `end_frame`. Session state (§2.2-C): never sim state.
#[derive(Resource, Default)]
pub struct GameMenuRes {
    /// Save slots the app found on disk (staged via
    /// [`View::set_save_list`]).
    pub saves: Vec<csp_ui::game::SaveSlotRow>,
    /// The menu widget's mutable state (name field, armed confirms).
    pub ui: csp_ui::game::GameMenuState,
    /// The revert point's sim tick, mirrored for the prompt —
    /// staged in `end_frame` (the schedules never read `sim`).
    pub revert_tick: Option<u64>,
}

/// The maneuver panel's session state — `csp-view` owns it (§2.2 C),
/// `csp_ui::maneuver_panel` mutates it inside `UiBuild`.
#[derive(Resource, Default)]
pub struct ManeuverPanelRes(pub csp_ui::ManeuverPanelState);

/// The W2.4 planning panel's session state — `csp-view` owns it
/// (§2.2 C), `csp_ui::planning_panel` mutates it inside `UiBuild`.
#[derive(Resource, Default)]
pub struct PlanningPanelRes(pub csp_ui::PlanningPanelState);

/// The settings window's session state — `csp-view` owns it (§2.2 C),
/// `csp_ui::settings_panel` mutates it inside `UiBuild`, and `csp-app`
/// reaches it through the `settings_*`/`rebind_*` plumbing methods
/// (the app owns key capture while a rebind listens).
#[derive(Resource, Default)]
pub struct SettingsPanelRes(pub settings::SettingsPanelState);

/// Post-step values staged for the `sync` schedule — written by
/// [`View::end_frame`] from the authoritative sim, then distributed into the
/// presentation mirrors by [`sync_mirrors`]. This is what makes the mirror
/// writes *systems* (Appendix A item 1) rather than bare statements.
#[derive(Resource, Default)]
struct SyncInput {
    clock: SimTime,
    warp_level: u32,
    ticks_last_frame: u64,
    /// The §2.6.6-item-4 report staged by `end_frame` — see
    /// [`SoiScanMirror`].
    soi_scan: csp_ui::SoiScanReport,
}

/// The Sync bridge (§2.5 #3): copy the staged post-step sim values into the
/// presentation-only mirrors. The sole writer of [`Clock`], [`WarpMirror`],
/// [`SoiScanMirror`], and [`FrameStats`].
fn sync_mirrors(
    input: Res<SyncInput>,
    mut clock: ResMut<Clock>,
    mut stats: ResMut<FrameStats>,
    mut warp: ResMut<WarpMirror>,
    mut soi_scan: ResMut<SoiScanMirror>,
) {
    clock.0 = input.clock;
    warp.0 = input.warp_level;
    stats.ticks_last_frame = input.ticks_last_frame;
    stats.frames += 1;
    soi_scan.0 = input.soi_scan.clone();
}

/// The panel flags, the progression mirror, the global cue channel,
/// and the §2.6.6 scan mirror as one system param — keeps `build_ui`
/// under the system-parameter cap. `sfx` is where a dropped panel
/// command lands its notice (the §3.7 rule: a click either does
/// something or says why not).
#[derive(bevy_ecs::system::SystemParam)]
struct ProgramUi<'w> {
    /// Optional-panel toggles.
    panels: ResMut<'w, PanelFlags>,
    /// The staged Program-window mirror.
    mirror: Res<'w, ProgramMirror>,
    /// The §3.7 flight-notice cue stack (`flight_notice`'s channel).
    sfx: ResMut<'w, flight::FlightSfx>,
    /// The right-click part menu's session state.
    menu: ResMut<'w, PartMenuRes>,
    /// The debug panel's §2.6.6-item-4 SOI scan mirror — a `Sync`-written
    /// copy like `Clock`/`WarpMirror`, read-only here.
    soi_scan: Res<'w, SoiScanMirror>,
}

/// The map-mode mirrors + panel session states as one system param —
/// the W2.4 panel's model/table/planning additions pushed `build_ui`
/// past bevy's 16-parameter cap (a fn over the cap stops being a
/// `System`, breaking `map_pointer_input.after(build_ui)`).
#[derive(bevy_ecs::system::SystemParam)]
struct MapUi<'w> {
    /// Overlay input + mode + focus bookkeeping.
    ui: Res<'w, map::MapUiData>,
    /// The staged map snapshot.
    snap: Res<'w, map::MapSnapshot>,
    /// Click/focus input staging.
    input: Res<'w, map::MapInput>,
    /// The orbit model (transfer planning reads it).
    model: Res<'w, map::OrbitModelRes>,
    /// The body id → display row table.
    table: Res<'w, map::BodyTable>,
    /// The maneuver-node panel's session state.
    maneuver: ResMut<'w, ManeuverPanelRes>,
    /// The W2.4 planning panel's session state.
    planning_state: ResMut<'w, PlanningPanelRes>,
    /// The staged transfer plans.
    planning: ResMut<'w, map::MapPlanning>,
}

/// The flight-mode pointer-pick bundle — the staged scene, the solved
/// chase camera, and the focus pin, kept as one SystemParam so
/// [`build_ui`] stays under bevy's 16-parameter cap (the [`MapUi`]/
/// [`ProgramUi`] precedent). The pick resolves the vessel the SAME way
/// `stage_program` did — the menu's commands name the mirror's focused
/// vessel, so the pick must too.
#[derive(bevy_ecs::system::SystemParam)]
struct FlightScene<'w> {
    /// The mirrored flight scene — the pick targets.
    snap: Res<'w, flight::FlightSnapshot>,
    /// The poses that scene is drawn at this frame — where the parts
    /// are on screen under the solved camera.
    drawn: Res<'w, flight::DrawnScene>,
    /// The solved chase camera — the projection.
    cam: Res<'w, flight::FlightCameraF64>,
    /// The flight-focus pin/override — the shared resolver.
    focus: Res<'w, flight::FlightFocusOverride>,
}

/// The entire egui pass (§2.5 #5, Appendix A item 3): build the plain-data
/// panel input from mirrors, run `csp_ui`'s pure panel functions under
/// `egui::Context::run_ui`, store the `FullOutput` for `csp-app`'s painter,
/// and record UI-emitted requests for next frame's Input. Never reads the
/// sim — every value is a mirror or session value.
#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_lines)] // the panel roster grew past the budget
fn build_ui(
    mut egui: ResMut<EguiPass>,
    clock: Res<Clock>,
    stats: Res<FrameStats>,
    warp: Res<WarpMirror>,
    session: Res<SessionMirror>,
    vab_input: Res<vab::VabInput>,
    mut vab: ResMut<vab::VabSessionRes>,
    craft_files: Res<vab::CraftFileList>,
    flight_ui: Res<flight::FlightUiData>,
    scene: FlightScene,
    mut map: MapUi,
    mut settings_state: ResMut<SettingsPanelRes>,
    mut prog_ui: ProgramUi,
    mut tutorial: ResMut<tutorial::TutorialRes>,
    mut game: ResMut<GameMenuRes>,
    mut outbox: ResMut<UiOutbox>,
) {
    let mode = if vab_input.open {
        csp_ui::ModeSelect::Vab
    } else if map.ui.mode == map::MapMode::Map {
        csp_ui::ModeSelect::Map
    } else {
        csp_ui::ModeSelect::Flight
    };
    let input = csp_ui::DebugPanelInput {
        adapter_name: &session.adapter,
        backend: &session.backend,
        sim_seconds: clock.0.seconds(),
        tick: clock.0.ticks(),
        ticks_last_frame: stats.ticks_last_frame,
        frames: stats.frames,
        frame_dt_ms: session.dt_wall_ms,
        // The frame-phase split (the app's `set_frame_phase`, the same
        // §2.5-legal Instant site as the fps row) — F3-only readout;
        // 0.0 hides the row until the first submitted frame lands.
        cpu_frame_ms: flight_ui.cpu_ms,
        gpu_wait_ms: flight_ui.present_ms,
        warp_level: warp.0,
        warp_requested: session.warp_requested,
        warp_effective: session.warp_effective,
        soi_scan: &prog_ui.soi_scan.0,
        fatal_reason: session.fatal.map(|e| e.reason()),
    };
    let mut panel_output = csp_ui::DebugPanelOutput::default();
    let mut vab_out = csp_ui::vab::VabUiOutput::default();
    let ctx = egui.ctx.clone(); // Arc-interned clone — frees the ResMut borrow.
    let raw = egui.input.clone();
    // Overwriting a previous, unconsumed frame would drop its
    // `#[must_use]` TexturesDelta — clear it the documented way.
    if let Some(prev) = &mut egui.output {
        prev.textures_delta.clear();
    }
    let files = craft_files.0.clone(); // Vec<String> — cheap, frees the Res borrow
    egui.output = Some(ctx.run_ui(raw, |ctx| {
        // The VAB's click-through mask: every floating window drawn
        // over the 3D build area pushes its rect here, and the pointer
        // pass runs LAST so the set is complete before any gesture
        // resolves — a press on a UI card must never reach the craft
        // behind it (C14-F2).
        let mut ui_masks: Vec<egui::Rect> = Vec::with_capacity(8);
        ui_masks.push(csp_ui::vab::top_bar(
            ctx,
            mode,
            warp.0,
            &session.keys,
            &session.key_labels,
            &mut vab_out,
        ));
        // The M0 scaffold is dev-facing (adapter/backend/tick internals)
        // — gated behind the ToggleDebug key so the shipped view is a
        // game screen, not a diagnostics dump.
        if prog_ui.panels.debug && mode != csp_ui::ModeSelect::Vab {
            csp_ui::debug_panel(ctx, &input, &mut panel_output);
        }
        if prog_ui.panels.help {
            let mut open = true;
            // The LIVE keymap — the staged edit copy would show
            // pending changes the dispatch doesn't honor yet.
            ui_masks.push(csp_ui::help_panel(
                ctx,
                &session.keys,
                &session.key_labels,
                &mut open,
            ));
            if !open {
                prog_ui.panels.help = false;
            }
        }
        if map.ui.mode == map::MapMode::Map {
            map_panels(
                ctx,
                &map.ui,
                &map.snap,
                &map.input,
                &map.model.0,
                &map.table,
                clock.0.ticks(),
                &session,
                &mut map.maneuver.0,
                &mut map.planning_state.0,
                &mut map.planning,
                &mut outbox,
            );
        }
        let mut palette_rect = None;
        if mode == csp_ui::ModeSelect::Vab {
            let editor_rects = csp_ui::vab::vab_editor(
                ctx,
                &mut vab.0,
                &files,
                &session.keys,
                &session.key_labels,
                &mut vab_out,
            );
            // `vab_editor` returns [palette, inspector, action_bar] —
            // the palette rect also feeds the drag-to-bin test, which
            // deletes a lifted part released over the parts list.
            palette_rect = editor_rects.first().copied();
            ui_masks.extend(editor_rects);
        }
        // The program-command queue — every surface that emits
        // `ProgramCommand`s this pass (the Program window, the flight
        // action strip, the right-click part menu) feeds one bag;
        // resolution against the ProgramMirror happens once below.
        let mut panel_cmds: Vec<csp_ui::program::ProgramCommand> = Vec::new();
        if mode == csp_ui::ModeSelect::Flight {
            csp_ui::flight_hud(
                ctx,
                &csp_ui::FlightHudInput {
                    has_vessel: flight_ui.has_vessel,
                    latched: flight_ui.latched,
                    alt_m: flight_ui.alt_m,
                    surface_speed_mps: flight_ui.surface_speed_mps,
                    orbital_speed_mps: flight_ui.orbital_speed_mps,
                    vspeed_mps: flight_ui.vspeed_mps,
                    heading_deg: flight_ui.heading_deg,
                    pitch_deg: flight_ui.pitch_deg,
                    roll_deg: flight_ui.roll_deg,
                    throttle: flight_ui.throttle,
                    parts: flight_ui.parts,
                    navball: csp_ui::navball::NavballInput {
                        up: flight_ui.nav_up,
                        north: flight_ui.nav_north,
                        east: flight_ui.nav_east,
                        prograde: flight_ui.nav_prograde,
                    },
                    warp: warp.0,
                    vessel_count: flight_ui.vessel_count,
                    debris_count: flight_ui.debris_count,
                    apo_alt_m: flight_ui.apo_alt_m,
                    pe_alt_m: flight_ui.pe_alt_m,
                    t_apo_s: flight_ui.t_apo_s,
                    t_pe_s: flight_ui.t_pe_s,
                    period_s: flight_ui.period_s,
                    builds: &flight_ui.builds,
                    // M5 §5.2-R2 arrival polish: the heightfield bake
                    // wait — the chip shows a one-line "generating
                    // surface" row while the terrain pass is sky-only.
                    terrain_pending: flight_ui.terrain_pending,
                    // P1: the app's smoothed frame timing — fps + ms
                    // over its ~10-frame window; 0.0 hides the row.
                    fps: flight_ui.fps,
                    frame_ms: flight_ui.frame_ms,
                    propellant: &flight_ui.propellant,
                    stages_left: flight_ui.stages_left,
                    stages_total: flight_ui.stages_total,
                    stages_plan_vessel: flight_ui.stages_plan_vessel,
                    mass_kg: flight_ui.mass_kg,
                    thrust_n: flight_ui.thrust_n,
                    thrust_full_n: flight_ui.thrust_full_n,
                    twr: flight_ui.twr,
                    twr_surface: flight_ui.twr_surface,
                    accel_mps2: flight_ui.accel_mps2,
                    accel_gee: flight_ui.accel_gee,
                    isp_s: flight_ui.isp_s,
                    mass_flow_kg_s: flight_ui.mass_flow_kg_s,
                    fuel_depletion_s: flight_ui.fuel_depletion_s,
                    burn_full_s: flight_ui.burn_full_s,
                    delta_v_mps: flight_ui.delta_v_mps,
                    delta_v_stages_mps: flight_ui.delta_v_stages_mps,
                    keys: &session.keys,
                    key_labels: &session.key_labels,
                },
            );
        }
        if mode == csp_ui::ModeSelect::Flight {
            // The in-flight action surfaces — the science/recovery
            // loop reachable where the player looks: the always-
            // visible Actions strip (bottom right) and the right-
            // click part menu (the KSP-style per-part action list).
            // Both emit `ProgramCommand`s that resolve against the
            // same ProgramMirror below.
            let mut action = csp_ui::program::FlightActionOutput::default();
            ui_masks.push(csp_ui::program::flight_action_bar(
                ctx,
                &prog_ui.mirror.input,
                &session.keys,
                &session.key_labels,
                &mut action,
            ));
            // Right-click = the part menu. The egui pointer owns the
            // gesture; a click over UI is egui's business, never a
            // scene pick. The two queries stay separate calls —
            // `egui_wants_pointer_input` re-locks the input inside
            // `ctx.input`'s reader and deadlocks if nested.
            let rclick = ctx.input(|i| {
                i.pointer
                    .secondary_clicked()
                    .then(|| i.pointer.interact_pos())
                    .flatten()
            });
            let over_ui = ctx.egui_wants_pointer_input();
            if let Some(pos) = rclick
                && !over_ui
            {
                let focus = scene.focus.resolve(&scene.snap.vessels);
                let (node, open) = match flight::pick_part(
                    &scene.cam,
                    focus.map(|f| (f, scene.drawn.pose_of(f))),
                    ctx.content_rect(),
                    pos,
                ) {
                    Some(flight::FlightPick::Part(node)) => (Some(node), true),
                    Some(flight::FlightPick::Vessel) => (None, true),
                    None => (None, false),
                };
                prog_ui.menu.0 = csp_ui::program::PartMenuState {
                    open,
                    node,
                    vessel: prog_ui.mirror.vessel.map_or(u64::MAX, |v| v.0),
                    pos,
                };
            }
            // A menu whose vessel left the mirror's focus is stale —
            // a same-numbered node would fire on the new focus.
            if prog_ui.menu.0.open
                && prog_ui.menu.0.vessel != prog_ui.mirror.vessel.map_or(u64::MAX, |v| v.0)
            {
                prog_ui.menu.0.open = false;
            }
            if prog_ui.menu.0.open {
                ui_masks.push(csp_ui::program::part_menu(
                    ctx,
                    &prog_ui.mirror.input,
                    &mut prog_ui.menu.0,
                    &mut action,
                ));
            }
            if action.open_program {
                prog_ui.panels.program = true;
                prog_ui.menu.0.open = false;
            }
            panel_cmds.extend(action.commands);
        }
        // The program economy readout — funds/science/contracts drawn
        // in EVERY mode so program state is never hidden behind a view
        // switch; the strip is also a button — it opens the window.
        if csp_ui::program::program_status(
            ctx,
            prog_ui.mirror.input.funds,
            prog_ui.mirror.input.science,
            prog_ui.mirror.input.active.len(),
        ) {
            prog_ui.panels.program = true;
        }
        if prog_ui.panels.program {
            let mut open = true;
            let mut cmds = Vec::new();
            ui_masks.push(csp_ui::program::program_panel(
                ctx,
                &prog_ui.mirror.input,
                &mut open,
                &mut cmds,
            ));
            if !open {
                prog_ui.panels.program = false;
            }
            panel_cmds.extend(cmds);
        }
        // The §3.7 dead-click rule made loud: every panel command —
        // window, action strip, or part menu — resolves against the
        // ProgramMirror it was drawn from; a drop on a stale mirror
        // posts a cue, never eats a click silently (W1.2's
        // silent-`RunExperiment` class).
        {
            let mut dropped = Vec::new();
            outbox.program.extend(
                panel_cmds
                    .into_iter()
                    .filter_map(|c| program_command(&prog_ui.mirror, c, &mut dropped)),
            );
            for text in dropped {
                prog_ui.sfx.cues.push_back(flight::SfxCue {
                    text: format!("COMMAND DROPPED — {text}"),
                    ttl: flight::SFX_TTL_FRAMES,
                });
            }
        }
        // M10: the settings window + the §2.13 Fatal dialog + the
        // tutorial checklist — session-state panels, drawn every mode.
        ui_masks.extend(m10_panels(
            ctx,
            &mut settings_state.0,
            &session,
            &mut outbox,
            &mut tutorial.0,
            &mut game,
            mode == csp_ui::ModeSelect::Vab,
        ));
        // The §3.7 cue stack — the GLOBAL refusal/transition channel.
        // Drawn in every mode: a refusal that can only flash inside
        // `flight_hud` is invisible exactly where it fires (Map
        // recover-control, VAB launch, warp demotion) — the dead-click
        // complaint. `flight_ui.sfx` is the Presentation-mirrored live
        // stack; `flight_present` ages the TTLs every frame, so cues
        // pushed from any mode still fade on schedule. Its chip rect
        // joins `ui_masks` — the Area is `interactable(false)`, so
        // egui never owns a click on a cue line and an unmasked
        // right-click would resolve as a viewport gesture, deleting
        // the VAB part behind the chip (C17-F7).
        ui_masks.extend(csp_ui::notice_overlay(ctx, &flight_ui.sfx));
        // The VAB's pointer/hotkey pass runs LAST: `ui_masks` now
        // holds every floating window rect drawn this frame (top bar,
        // editor panels, help, settings, crash, tutorial), so a press
        // or release landing on any of them resolves as UI — never a
        // scene pick, subtree delete, lift, or drop on the craft
        // behind the window (C14-F2).
        if mode == csp_ui::ModeSelect::Vab {
            csp_ui::vab::vab_pointer_input(ctx, &mut vab.0, &ui_masks, palette_rect);
        }
    }));
    // The top bar's Tutorial button re-opens the checklist — session
    // state, folded back here rather than riding the app outbox.
    if vab_out.tutorial {
        tutorial.0.start();
    }
    // The top bar's "? Help" button toggles the controls card — the
    // same fold-back pattern as the tutorial (session state, never the
    // app outbox).
    if vab_out.help {
        prog_ui.panels.help = !prog_ui.panels.help;
    }
    // The top bar's "Program" button toggles the progression window —
    // same fold-back convention as help/tutorial.
    if vab_out.program {
        prog_ui.panels.program = !prog_ui.panels.program;
    }
    // Warp steps may come from the debug panel's buttons or the top
    // bar's −/+ now that the scaffold is dev-gated.
    outbox.warp_step = panel_output.warp_step + vab_out.warp_step;
    outbox.mode = vab_out.mode;
    outbox.launch = vab_out.launch;
    // Save carries the serialized bytes — the app owns the file store.
    // The `if let` keeps `to_bytes` (a bincode pass over the session)
    // behind the actual save request — `Option::zip` would serialize on
    // every VAB frame. A serialization failure lands on the notice
    // channel like every other op refusal (C17-F8 — `.ok()` used to
    // swallow it, the one silent op path in the module).
    outbox.craft_save = if let Some(name) = vab_out.save_name {
        match vab.0.to_bytes() {
            Ok(bytes) => Some((name, bytes)),
            Err(e) => {
                vab.0.set_notice(Some(format!("craft save failed: {e}")));
                None
            }
        }
    } else {
        None
    };
    outbox.craft_load = vab_out.load_name;
}

/// The map mode's UiBuild panels — the M1 overlay + the M5 node panel
/// (its `SetManeuver`/`DeleteManeuver` output rides the outbox).
/// Extracted from [`build_ui`] under the line budget.
#[allow(clippy::too_many_arguments)] // the W2.4 panel adds the model/table/planning mirrors
#[allow(clippy::too_many_lines)] // one panel roster = one pass
fn map_panels(
    ctx: &egui::Context,
    map_ui: &map::MapUiData,
    map_snap: &map::MapSnapshot,
    map_input: &map::MapInput,
    map_model: &csp_sim::orbit::PatchedConics,
    map_table: &map::BodyTable,
    tick: u64,
    session: &SessionMirror,
    maneuver_state: &mut csp_ui::ManeuverPanelState,
    planning_state: &mut csp_ui::PlanningPanelState,
    planning: &mut map::MapPlanning,
    outbox: &mut UiOutbox,
) {
    csp_ui::map_overlay(
        ctx,
        &map_ui.focus_name,
        map_ui.camera_distance_m,
        &map_ui.labels,
        map_ui.orbit_info.as_ref(),
        &session.keys,
        &session.key_labels,
    );

    let mut mcmd = None;
    csp_ui::maneuver_panel(
        ctx,
        tick,
        csp_core::TICK_HZ as f64,
        &map_snap
            .vessels
            .iter()
            // Only ON-RAILS vessels are offered (P09-F2): a
            // `SetManeuver` on an InBubble vessel rejects "not on
            // rails" at drain — the picker used to list them anyway,
            // making every such Set press a silent refusal.
            .filter(|v| v.3)
            .map(|v| csp_ui::ManeuverVessel {
                id: v.0.0,
                node: v.2.as_ref().map(|n| (n.tick, n.dv_magnitude())),
            })
            .collect::<Vec<_>>(),
        maneuver_state,
        &mut mcmd,
    );
    outbox.maneuver = mcmd.map(|c| match c {
        csp_ui::ManeuverCommand::Set { vessel, tick, dv } => SimCommand::SetManeuver {
            vessel: csp_sim::VesselId(vessel),
            tick,
            dv_prograde_mps: dv[0],
            dv_normal_mps: dv[1],
            dv_radial_mps: dv[2],
        },
        csp_ui::ManeuverCommand::Delete { vessel } => SimCommand::DeleteManeuver {
            vessel: csp_sim::VesselId(vessel),
        },
    });

    // W2.4: the planning panel — alarms + the transfer-window planner
    // (PLAN §4 v1/M5). Endpoint pickers get orbiting bodies only; the
    // vessel picker stages eligibility honestly (on rails + orbiting
    // the picked origin) so an unservable vessel is never a dead
    // click. Emitted commands resolve against the same mirrors here —
    // SetAlarm/WatchTransferWindow/SetManeuver ride the program
    // outbox (the progression FIFO); the pure planner calls run on
    // the spot and stage into `planning`.
    let focus_vid = map_input.focus_vessel;
    let focus_has_node = focus_vid
        .and_then(|id| map_snap.vessels.iter().find(|v| v.0 == id))
        .is_some_and(|v| v.2.is_some());
    let bodies: Vec<String> = map_table
        .bodies
        .iter()
        .filter(|b| b.parent.is_some())
        .map(|b| b.id.to_string())
        .collect();
    let vessels: Vec<csp_ui::TransferVessel> = map_snap
        .vessels
        .iter()
        .map(|v| {
            let (eligible, note) = if !v.3 {
                (
                    false,
                    "in physics bubble — coast to rails first".to_string(),
                )
            } else if v.1.parent.as_str() != planning_state.origin {
                (
                    false,
                    format!("orbits {}, not {}", v.1.parent, planning_state.origin),
                )
            } else {
                (true, String::new())
            };
            csp_ui::TransferVessel {
                id: v.0.0,
                parent: v.1.parent.to_string(),
                eligible,
                note,
            }
        })
        .collect();
    let plan_row = planning.plan_row(tick);
    let alarm_rows = planning.alarm_rows(tick);
    let input = csp_ui::PlanningPanelInput {
        tick,
        tick_hz: csp_core::TICK_HZ as f64,
        bodies: &bodies,
        vessels: &vessels,
        alarms: &alarm_rows,
        plan: plan_row.as_ref(),
        plan_error: planning.plan_error.as_deref(),
        focus_vessel: focus_vid.map(|v| v.0),
        focus_has_node,
    };
    let picked = vessels
        .iter()
        .find(|v| v.id == planning_state.vessel && v.eligible)
        .map(|v| csp_sim::VesselId(v.id));
    let mut pcmds = Vec::new();
    csp_ui::planning_panel(ctx, &input, planning_state, &mut pcmds);
    for cmd in pcmds {
        map::resolve_planning_command(
            cmd,
            map_model,
            map_snap,
            focus_vid,
            picked,
            planning,
            &mut outbox.program,
        );
    }
}

/// Resolve one [`csp_ui::program::ProgramCommand`] into the
/// `SimCommand` it names — the context fields the command needs
/// (vessel, biome, situation, `DataKey`) come back out of the
/// [`ProgramMirror`] the panel was drawn from. `None` when the
/// mirrored context vanished between staging and the click (a
/// one-frame-stale panel command is dropped, not misfired) — the
/// drop is reported through `dropped` so `build_ui` can post a
/// flight notice; a stale mirror must never eat a click silently.
#[allow(clippy::too_many_lines)] // one verb = one match arm
fn program_command(
    mirror: &ProgramMirror,
    cmd: csp_ui::program::ProgramCommand,
    dropped: &mut Vec<&'static str>,
) -> Option<SimCommand> {
    use csp_ui::program::ProgramCommand as C;
    match cmd {
        C::ResearchTech { node } => Some(SimCommand::ResearchTech {
            node: csp_data::TechNodeId::from(node),
        }),
        C::ResearchTrack { track } => Some(SimCommand::ResearchUpgradeLevel {
            track: csp_data::UpgradeTrackId::from(track),
        }),
        C::AcceptContract { offer } => Some(SimCommand::AcceptContract { offer }),
        C::DeclineContract { offer } => Some(SimCommand::DeclineContract { offer }),
        C::RunExperiment { node, experiment } => {
            let Some((biome, situation)) = mirror.science_ctx.clone() else {
                dropped.push("EXPERIMENT NOT RUN — NO SCIENCE CONTEXT");
                return None;
            };
            let Some(vessel) = mirror.vessel else {
                dropped.push("EXPERIMENT NOT RUN — NO FOCUSED VESSEL");
                return None;
            };
            Some(SimCommand::RunExperiment {
                vessel,
                node: csp_phys::PartNodeId(node),
                experiment: csp_data::ExperimentId::from(experiment),
                biome,
                situation,
            })
        }
        C::Transmit { row } | C::Recover { row } => {
            let is_transmit = matches!(cmd, C::Transmit { .. });
            let Some(key) = mirror.keys.get(row).map(|k| k.key.clone()) else {
                dropped.push("DATA ROW DROPPED — STALE PANEL INDEX");
                return None;
            };
            let Some(vessel) = mirror.vessel else {
                dropped.push("DATA COMMAND DROPPED — NO FOCUSED VESSEL");
                return None;
            };
            Some(if is_transmit {
                SimCommand::TransmitData { vessel, key }
            } else {
                SimCommand::RecoverData { vessel, key }
            })
        }
        // §3.5's recovery verbs — the site was staged with the mirror
        // (launch site on the latched body, else its first site, else
        // the `None` remote-salvage leg).
        C::RecoverVessel => {
            let Some(vessel) = mirror.vessel else {
                dropped.push("RECOVER DROPPED — NO FOCUSED VESSEL");
                return None;
            };
            Some(SimCommand::RecoverVessel {
                vessel,
                site: mirror.recovery_site.clone(),
            })
        }
        C::TerminateVessel => {
            let Some(vessel) = mirror.vessel else {
                dropped.push("TERMINATE DROPPED — NO FOCUSED VESSEL");
                return None;
            };
            Some(SimCommand::TerminateVessel { vessel })
        }
        // W2.0/W2.3 — the progression-surface verbs. Focused-vessel
        // commands drop with a notice on a stale mirror (the §3.7
        // rule); id-addressed commands go through as issued — a stale
        // id rejects `Recoverable` at the drain, which the rejected-
        // command channel surfaces.
        C::HireCrew { kind, skill } => {
            let kind = match kind.as_str() {
                "pilot" => csp_sim::CrewKind::Pilot,
                "engineer" => csp_sim::CrewKind::Engineer,
                "scientist" => csp_sim::CrewKind::Scientist,
                _ => csp_sim::CrewKind::Tourist,
            };
            let name = format!("{kind:?} {}", mirror.input.roster.len() + 1).to_lowercase();
            Some(SimCommand::HireCrew {
                name,
                kind,
                skill,
                salary_per_s: mirror.input.hire_salary_per_s,
            })
        }
        C::BoardCrew => match mirror.vessel {
            Some(vessel) => Some(SimCommand::BoardCrew { vessel }),
            None => {
                dropped.push("BOARD DROPPED — NO FOCUSED VESSEL");
                None
            }
        },
        C::TransferCrew { crew, to_vessel } => match mirror.vessel {
            Some(vessel) => Some(SimCommand::TransferCrew {
                from_vessel: vessel,
                to_vessel: csp_sim::VesselId(to_vessel),
                crew: csp_sim::CrewId(crew),
            }),
            None => {
                dropped.push("CREW TRANSFER DROPPED — NO FOCUSED VESSEL");
                None
            }
        },
        C::TransferResource {
            to_vessel,
            resource,
            amount,
        } => match mirror.vessel {
            Some(vessel) => Some(SimCommand::TransferResource {
                from_vessel: vessel,
                to_vessel: csp_sim::VesselId(to_vessel),
                resource: csp_data::ResourceId::from(resource),
                amount,
            }),
            None => {
                dropped.push("TRANSFER DROPPED — NO FOCUSED VESSEL");
                None
            }
        },
        C::AssignCrewToColony { crew, colony } => Some(SimCommand::AssignCrewToColony {
            colony: csp_sim::ColonyId(colony),
            crew: csp_sim::CrewId(crew),
        }),
        C::EvacuateColony { colony } => Some(SimCommand::EvacuateColony {
            colony: csp_sim::ColonyId(colony),
        }),
        C::AbandonColony { colony } => Some(SimCommand::AbandonColony {
            colony: csp_sim::ColonyId(colony),
        }),
        C::RepairColonyModule { colony, module } => Some(SimCommand::RepairColonyModule {
            colony: csp_sim::ColonyId(colony),
            module: module as usize,
        }),
        C::ScheduleColonyInspection { colony } => Some(SimCommand::ScheduleColonyInspection {
            colony: csp_sim::ColonyId(colony),
            tick: mirror.now_ticks + csp_core::TICK_HZ * 86_400,
        }),
        C::FoundColony { site } => Some(SimCommand::FoundColony {
            def: starter_colony_def(&site),
        }),
        C::JoinColony { colony } => match mirror.vessel {
            Some(vessel) => Some(SimCommand::JoinColonyInventory {
                vessel,
                colony: csp_sim::ColonyId(colony),
            }),
            None => {
                dropped.push("JOIN DROPPED — NO FOCUSED VESSEL");
                None
            }
        },
        C::LeaveColony => match mirror.vessel {
            Some(vessel) => Some(SimCommand::LeaveColonyInventory { vessel }),
            None => {
                dropped.push("LEAVE DROPPED — NO FOCUSED VESSEL");
                None
            }
        },
        C::TransferColonyResource {
            colony,
            resource,
            amount,
            to_colony,
        } => match mirror.vessel {
            Some(vessel) => Some(SimCommand::TransferColonyResource {
                vessel,
                colony: csp_sim::ColonyId(colony),
                resource: csp_data::ResourceId::from(resource),
                amount,
                to_colony,
            }),
            None => {
                dropped.push("COLONY TRANSFER DROPPED — NO FOCUSED VESSEL");
                None
            }
        },
        C::TransferColonyCrew {
            colony,
            crew,
            to_colony,
        } => match mirror.vessel {
            Some(vessel) => Some(SimCommand::TransferColonyCrew {
                vessel,
                colony: csp_sim::ColonyId(colony),
                crew: csp_sim::CrewId(crew),
                to_colony,
            }),
            None => {
                dropped.push("COLONY CREW DROPPED — NO FOCUSED VESSEL");
                None
            }
        },
        C::DeployPart { node } => match mirror.vessel {
            Some(vessel) => Some(SimCommand::DeployPart {
                vessel,
                node: csp_phys::PartNodeId(node),
            }),
            None => {
                dropped.push("DEPLOY DROPPED — NO FOCUSED VESSEL");
                None
            }
        },
        C::StartResearch { lab_node, row } => {
            let Some(vessel) = mirror.vessel else {
                dropped.push("RESEARCH DROPPED — NO FOCUSED VESSEL");
                return None;
            };
            let Some(key) = mirror.keys.get(row).map(|k| k.key.clone()) else {
                dropped.push("RESEARCH DROPPED — STALE PANEL INDEX");
                return None;
            };
            // C21-F7: a lab node absent from the staged table is not
            // the empty-roster case — the lab isn't aboard the focus
            // (a stale row between staging and the click). The two
            // drops used to share "NO CREW ABOARD"; §3.7's "says why
            // not" rule wants the real one.
            let Some(slots) = mirror.lab_slots.get(&lab_node).copied() else {
                dropped.push("RESEARCH DROPPED — LAB NOT ON FOCUSED VESSEL");
                return None;
            };
            let researchers: Vec<csp_sim::ResearcherAssign> = mirror
                .researchers
                .iter()
                .take(slots as usize)
                .map(|(crew, skill)| csp_sim::ResearcherAssign {
                    crew: *crew,
                    skill: *skill,
                })
                .collect();
            if researchers.is_empty() {
                dropped.push("RESEARCH DROPPED — NO CREW ABOARD");
                return None;
            }
            Some(SimCommand::StartResearch {
                vessel,
                lab_node: csp_phys::PartNodeId(lab_node),
                key,
                researchers,
            })
        }
        C::StopResearch { project } => Some(SimCommand::StopResearch {
            project: csp_sim::ResearchId(project),
        }),
        C::SetAlarm { in_seconds, label } => Some(SimCommand::SetAlarm {
            tick: mirror.now_ticks + (in_seconds.max(1.0) * csp_core::TICK_HZ as f64) as u64,
            label,
        }),
        C::LoadScript { source } => match mirror.vessel {
            Some(vessel) => Some(SimCommand::LoadScript { vessel, source }),
            None => {
                dropped.push("SCRIPT LOAD DROPPED — NO FOCUSED VESSEL");
                None
            }
        },
        C::UnloadScript { script } => Some(SimCommand::UnloadScript {
            script: csp_sim::ScriptId(script),
        }),
        C::SetScriptEnabled { script, enabled } => Some(SimCommand::SetScriptEnabled {
            script: csp_sim::ScriptId(script),
            enabled,
        }),
        C::EngageAutopilot { apo_m, pe_m } => match mirror.vessel {
            Some(vessel) => Some(SimCommand::EngageAutopilot {
                vessel,
                program: csp_sim::AutopilotProgram::Ascent {
                    target_apoapsis_m: apo_m,
                    target_periapsis_m: pe_m,
                },
            }),
            None => {
                dropped.push("AUTOPILOT DROPPED — NO FOCUSED VESSEL");
                None
            }
        },
        C::DisengageAutopilot => match mirror.vessel {
            Some(vessel) => Some(SimCommand::DisengageAutopilot { vessel }),
            None => {
                dropped.push("AUTOPILOT DROPPED — NO FOCUSED VESSEL");
                None
            }
        },
        C::RunBenchTest {
            site,
            family,
            units,
        } => Some(SimCommand::RunBenchTest {
            site: csp_data::SiteId::from(site),
            family: csp_data::FamilyId::from(family),
            units,
        }),
        // The FACILITIES build row's cancel — id-addressed like the
        // colony verbs: a stale `BuildId` rejects `Recoverable` at
        // the drain rather than needing mirror context.
        C::CancelBuild { build } => Some(SimCommand::CancelBuild {
            build: csp_sim::manufacture::BuildId(build),
        }),
        C::WatchTransferWindow {
            origin,
            destination,
            tick,
        } => Some(SimCommand::WatchTransferWindow {
            origin: csp_data::BodyId::from(origin),
            destination: csp_data::BodyId::from(destination),
            tick,
        }),
    }
}

/// The player-facing `FoundColony` payload — a starter outpost:
/// a small solar array, a battery, a `LifeSupport` module with its
/// founding consumables, and the founding EC charge. The apply
/// resolves the site's body/pose, runs the founding solve, and
/// charges the authored `colony_founding_cost_funds` (C21-F8 — the
/// "prototype, no cost" era ended; the panel prints the price).
/// C09-F4: the LS module is what makes `AssignCrewToColony` legal
/// here — its `consumes_per_crew` mirrors the authored
/// `life_support_*_per_crew_s` balance rates, and the founding
/// oxygen/food stocks cover ≈12 ks of one crew's need before
/// resupply must arrive.
fn starter_colony_def(site: &str) -> csp_data::ColonyDef {
    csp_data::ColonyDef {
        site: csp_data::SiteId::from(site),
        display_name: csp_data::LocKey::from(""),
        modules: vec![
            csp_data::ColonyModuleDef::Solar {
                area_m2: 10.0,
                efficiency: 0.2,
            },
            csp_data::ColonyModuleDef::Battery { capacity: 500.0 },
            csp_data::ColonyModuleDef::LifeSupport {
                consumes_per_crew: vec![
                    (csp_data::ResourceId::from("oxygen"), 0.02),
                    (csp_data::ResourceId::from("food"), 0.005),
                ],
                power_ec_per_s: 0.05,
            },
            csp_data::ColonyModuleDef::Store {
                resource: csp_data::ResourceId::from("oxygen"),
                capacity: 500.0,
            },
            csp_data::ColonyModuleDef::Store {
                resource: csp_data::ResourceId::from("food"),
                capacity: 500.0,
            },
        ],
        stores: vec![
            csp_data::ColonyStoreInit {
                resource: csp_data::ResourceId::from("electric_charge"),
                level: 250.0,
            },
            csp_data::ColonyStoreInit {
                resource: csp_data::ResourceId::from("oxygen"),
                level: 250.0,
            },
            csp_data::ColonyStoreInit {
                resource: csp_data::ResourceId::from("food"),
                level: 250.0,
            },
        ],
    }
}

/// The map's pointer-side camera intake — a second `UiBuild` system
/// ordered after [`build_ui`] so egui's pointer state (this frame's
/// `EguiPass.input`, processed inside the `run_ui` pass) is complete.
/// A middle/right-drag becomes free `Pan`, a primary click inside
/// [`map::PICK_RADIUS_PX`] of a projected marker becomes
/// click-to-focus, and the live cursor lands on
/// [`map::MapInput::cursor_px`] so scroll-zoom aims at the point under
/// it — all queued for the next `Input` pass (§2.5's one-frame
/// UiBuild → Input hand-off, same as the outbox).
///
/// `egui_wants_pointer_input` gates every action: clicks and drags on
/// panels or the top bar are egui's business, never camera moves. It
/// deliberately stays `false` for a drag that began on the open map
/// and crosses a panel — matching the winit orbit-drag's
/// never-cancelled-by-panels rule (a swallowed release must not leave
/// the camera moving).
///
/// `egui_wants_pointer_input` alone can't gate a pan drag, though: its
/// over-egui clause is dead while any button is held, and no egui
/// widget claims a secondary/middle drag, so it reads `false` for the
/// whole drag even when the press landed on a panel. `drag_over_ui`
/// latches the press frame's own verdict — `is_pointer_over_egui` for
/// where the press fell, `egui_is_using_pointer` for a press landing
/// mid-widget-interaction — and gates the drag until every pan button
/// is released.
fn map_pointer_input(
    egui: Res<EguiPass>,
    map_ui: Res<map::MapUiData>,
    mut map_cam: ResMut<map::MapInput>,
    mut map_events: ResMut<map::MapInputEvents>,
    mut drag_over_ui: Local<bool>,
) {
    if map_ui.mode != map::MapMode::Map {
        map_cam.cursor_px = None;
        *drag_over_ui = false;
        return;
    }
    let ctx = &egui.ctx;
    let ppp = f64::from(ctx.pixels_per_point());
    let over_ui = ctx.egui_wants_pointer_input();
    let (cursor, pan_delta, clicked, pan_pressed, pan_held) = ctx.input(|i| {
        (
            i.pointer.latest_pos(),
            (i.pointer.button_down(egui::PointerButton::Middle)
                || i.pointer.button_down(egui::PointerButton::Secondary))
            .then(|| i.pointer.delta()),
            i.pointer
                .primary_clicked()
                .then(|| i.pointer.interact_pos())
                .flatten(),
            i.pointer.button_pressed(egui::PointerButton::Middle)
                || i.pointer.button_pressed(egui::PointerButton::Secondary),
            i.pointer.button_down(egui::PointerButton::Middle)
                || i.pointer.button_down(egui::PointerButton::Secondary),
        )
    });
    // `|=` keeps a still-held earlier press's verdict when a second
    // pan button joins; the latch clears once no pan button is down.
    if pan_pressed {
        *drag_over_ui |= ctx.is_pointer_over_egui() || ctx.egui_is_using_pointer();
    }
    if !pan_held {
        *drag_over_ui = false;
    }
    // Physical px — the space `viewport_px`/`cursor_px` speak (egui
    // positions are in points; `ppp` converts).
    map_cam.cursor_px = cursor.map(|p| (f64::from(p.x) * ppp, f64::from(p.y) * ppp));
    if over_ui {
        return;
    }
    if let Some(d) = pan_delta
        && (d.x != 0.0 || d.y != 0.0)
        && !*drag_over_ui
    {
        // Grab-the-map feel: the content follows the cursor, so the
        // look-at point moves opposite the drag delta.
        map_events.pending.push(map::MapKeyEvent::Pan {
            dx_px: -f64::from(d.x) * ppp,
            dy_px: -f64::from(d.y) * ppp,
        });
    }
    if let Some(pos) = clicked {
        let (w, h) = map_cam.viewport_px;
        let px = (f64::from(pos.x) * ppp, f64::from(pos.y) * ppp);
        // Nearest projected marker inside the pick radius wins —
        // picks were recorded in `map_present` alongside the labels,
        // so a hit targets exactly what the player saw drawn.
        let hit = map_ui
            .picks
            .iter()
            .filter_map(|pk| {
                let sx = (f64::from(pk.ndc[0]) * 0.5 + 0.5) * w;
                let sy = (0.5 - f64::from(pk.ndc[1]) * 0.5) * h;
                let d = (sx - px.0).hypot(sy - px.1);
                (d <= map::PICK_RADIUS_PX).then_some((d, pk.target))
            })
            .min_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        if let Some((_, target)) = hit {
            map_events.pending.push(match target {
                map::MapFocusTarget::Body(i) => map::MapKeyEvent::FocusBody(i),
                map::MapFocusTarget::Vessel(id) => map::MapKeyEvent::FocusVessel(id),
            });
        }
    }
}

/// M10's always-on session panels — the settings window (when open;
/// its Apply/Reset requests ride the outbox), the §2.13 Fatal crash
/// dialog (while the halt mirror is set), and the first-flight
/// tutorial checklist (while its session flag is active). Extracted
/// from [`build_ui`] to keep that system under the line budget.
/// Returns the rect of every window it drew — the caller masks the
/// VAB's pointer input against them (C14-F2).
fn m10_panels(
    ctx: &egui::Context,
    settings_state: &mut settings::SettingsPanelState,
    session: &SessionMirror,
    outbox: &mut UiOutbox,
    tutorial: &mut csp_ui::tutorial::TutorialState,
    game: &mut GameMenuRes,
    in_vab: bool,
) -> Vec<egui::Rect> {
    let mut rects = Vec::with_capacity(3);
    let mut settings_out = csp_ui::settings::SettingsPanelOutput::default();
    if settings_state.open {
        rects.push(csp_ui::settings::settings_panel(
            ctx,
            settings_state,
            &mut settings_out,
            &session.key_labels,
        ));
    }
    if settings_out.reset_defaults {
        settings_state.settings = csp_ui::settings::Settings::default();
        settings_state.notice = Some("defaults restored — Apply to keep".into());
        // The restage drops the keymap the armed capture expected —
        // a press landing now would bind into the fresh defaults, not
        // the rows the user armed against.
        settings_state.listening = None;
    }
    outbox.settings_apply = settings_out.apply;
    if let Some(fatal) = &session.fatal {
        rects.push(csp_ui::crash_dialog(
            ctx,
            fatal.reason(),
            session.fatal_bundle.as_deref(),
        ));
    }
    // The Game menu — save slots, load, revert-to-launch (with the
    // PLAN:1180 "not a reroll" disclosure), new game. The app owns the
    // file IO; the verbs ride the outbox like every other request.
    {
        let mut gm_out = csp_ui::game::GameMenuOutput::default();
        let input = csp_ui::game::GameMenuInput {
            saves: &game.saves,
            revert_tick: game.revert_tick,
        };
        rects.extend(csp_ui::game::game_menu(
            ctx,
            &mut game.ui,
            &input,
            &mut gm_out,
        ));
        outbox.game_save = gm_out.save;
        outbox.game_load = gm_out.load;
        outbox.revert = gm_out.revert;
        outbox.new_game = gm_out.new_game;
    }
    // The first-flight checklist — drawn in every mode while active
    // (it coaches the VAB steps from inside the editor, where its
    // anchor keeps it clear of the Craft inspector).
    if tutorial.active {
        rects.push(csp_ui::tutorial::tutorial_overlay(
            ctx,
            tutorial,
            &session.keys,
            &session.key_labels,
            in_vab,
        ));
    }
    rects
}

/// The six schedules from PLAN §2.5, run explicitly by `csp-app`.
pub struct Schedules {
    /// winit events -> SimCommands; camera input; UI event intake.
    pub input: Schedule,
    /// The sim's own step — single-threaded always (PLAN §2.5).
    pub sim_step: Schedule,
    /// Sim snapshot -> presentation components; writes every mirror.
    pub sync: Schedule,
    /// Interpolation, camera solve, LOD, VFX, audio cues.
    pub presentation: Schedule,
    /// The entire egui pass; emits requests consumed next frame's Input.
    pub ui_build: Schedule,
    /// Build the immutable `csp_render::Frame`.
    pub render_extract: Schedule,
}

/// The presentation world + its schedules + the authoritative sim.
pub struct View {
    /// Authoritative sim — lives OUTSIDE the ECS (PLAN §2.2). `csp-app`
    /// reads `sim.clock()` for the §2.6.3 frame target and enqueues through
    /// [`View::enqueue_command`]; everything else is mirror plumbing.
    pub sim: SimWorld,
    /// The bevy_ecs presentation world — mirrors and session resources
    /// only, never authoritative sim state (§2.2). Read/mutation
    /// discipline for callers (`csp-app` included): **never read the ECS
    /// to build UI** — input enters through the `set_*`/`update_*`
    /// plumbing methods and output leaves through `take_*`; and mutate
    /// the world only through those same plumbing methods, never by
    /// poking resources directly (tests may, to stage fixtures).
    pub world: World,
    /// The six §2.5 schedules. They run only through
    /// [`View::begin_frame`]/[`View::sim_step`]/[`View::end_frame`] in the
    /// prescribed order — `pub` so systems can be *registered* into them
    /// as milestones land, never for ad-hoc `run` calls or reads from
    /// `csp-app`.
    pub schedules: Schedules,
    /// Ticks the last `sim_step` advanced — staged into `SyncInput` by
    /// `end_frame` so `ticks_last_frame` reaches `FrameStats` through the
    /// Sync system, not the sim path (Appendix A item 1).
    ticks_last_frame: u64,
    /// `true` for the §6.3 web-demo build — set by [`View::new_demo`].
    /// `csp-app` reads it to refuse craft-file import/export (the
    /// session-level part cap and file-IO refusal are installed on the
    /// `VabSession` itself at construction).
    demo: bool,
    /// W2.6: cursor into the sim's serialized notification ring —
    /// `(tick of the last surfaced entry, entries surfaced at that
    /// tick)`. `sim_step` diffs the ring against it so each
    /// `PartFailed`/`IgnitionMisfire`/`OnRails` record lands exactly
    /// one `flight_notice` toast (report-08 L3: the LOG was the only
    /// surface — a dormant failure mid-flight read as nothing but
    /// lost thrust). Ring eviction past `notification_cap` is safe:
    /// evicted entries carry older ticks and can never resurface.
    /// Session state only — never sim state.
    notice_cursor: Option<(u64, u32)>,
    /// The launch snapshot for revert-to-launch (PLAN:1180 —
    /// "revert is not a reroll": the same pre-drawn θ records ride the
    /// restore, so the same parts still fail at the same Λ). Captured
    /// in `launch_craft` before the order's commands enqueue, then
    /// recaptured at the delivery boundary (see [`RevertCapture`]) —
    /// `launch_craft` itself can only ever see the pre-ORDER world,
    /// its own commands still being pending. `Err` text records why
    /// no revert point exists (a §7.4 snapshot refusal).
    pre_launch: Option<Result<WorldSnapshot, String>>,
    /// A launch awaiting its delivery-boundary recapture — see
    /// [`RevertCapture`]. Session state tied to this session's launch
    /// press: `rebuild_from_snapshot` clears it alongside `pre_launch`
    /// so a loaded world's next `ConstructionComplete` cannot write
    /// itself in as a revert point the loaded session never ordered.
    revert_capture: Option<RevertCapture>,
    /// The resolved focus the view last issued `SetVesselTracked` +
    /// `SetActiveVessel` commands for (C02-F1/C02-F2). The sim can't
    /// see a queued-but-undrained command, so `end_frame` diffs the
    /// resolved focus against THIS mirror — not the sim's
    /// `active_vessel` — or a paused sim would flood the player FIFO
    /// with re-issues. `None` after a world-replacing op so the next
    /// frame restamps the focus fresh.
    focused_issued: Option<csp_sim::VesselId>,
}

/// The launch revert point's delivery-boundary recapture (PLAN:1177-1180
/// — the sim contract at `manufacture.rs` expects the post-delivery
/// snapshot: a retry of the flying, never a re-pay of the build wait).
/// `launch_craft` can only snapshot the pre-ORDER world — its own
/// commands are still pending at that point — so `sim_step` re-takes
/// the point once the armed order's own delivery boundary lands the
/// craft surface-latched on the pad (see
/// [`View::advance_revert_capture`] for the match).
#[derive(Clone, Debug)]
enum RevertCapture {
    /// Order enqueued; armed for THIS order's delivery, not any
    /// `ConstructionComplete` — a concurrent `ManufactureCraft` (a
    /// second launch's superseded arm aside, the server/MCP ingress
    /// carries `SourceClass::External` orders) delivers on its own
    /// event and must not spend the arm.
    ///
    /// `site` + `craft` name the order's product and `min_build_id` is
    /// the `StagePlan`-convention bound (`builds().next_back() + 1` at
    /// arm — `next_build_id` itself is not observable; ids are
    /// monotonic, never reused): the `BuildId` is minted inside the
    /// drain, so it cannot be named at enqueue, and an already-queued
    /// foreign order takes the lower ids — the watermark bounds the
    /// `builds()` scan to records minted at-or-after this order.
    /// `armed_tick` is the clock at enqueue: the player FIFO
    /// drains the order at the first boundary the next step crosses,
    /// so a `reached_tick` past it has consumed the order one way or
    /// the other. The site-less `SpawnPhysicalVessel` fallback never
    /// ENTERS this state — its branch arms nothing, and the
    /// launch-time capture stays the point.
    AwaitingDelivery {
        /// The launch site the order delivers to.
        site: csp_data::SiteId,
        /// The ordered craft — the record/log match key (the same
        /// instance the command carries verbatim).
        craft: csp_sim::CraftDef,
        /// `builds().next_back() + 1` at arm: records minted before
        /// this order are pre-existing builds, never its delivery.
        min_build_id: u64,
        /// Clock at enqueue — the boundary stamp the order drains at.
        armed_tick: u64,
    },
    /// The armed order's own boundary passed; retry the §7.4 snapshot
    /// every `sim_step` until it admits one — a transient refusal (live
    /// contacts on another vessel, a joint island) must not strand the
    /// point at the pre-order capture, which is the "revert un-pays
    /// the build" hole this recapture exists to close.
    Delivered,
}

/// §2.5's interpolation resources — installed with the flight view's
/// (`View::with_sim`): the frame's fractional tick remainder, the
/// flight view's previous-tick poses, and what the frame draws.
fn insert_interp_resources(world: &mut World) {
    world.insert_resource(TickFraction::default());
    world.insert_resource(flight::PoseHistory::default());
    world.insert_resource(flight::DrawnScene::default());
}

/// Replace the session's sim with a world rebuilt from `snapshot`,
/// re-installing the originating world's own config channels
/// ([`SimWorld::install_config`]) and the control engines the
/// snapshot cannot carry (`RhaiHost`/`AscentAutopilot` — the same
/// pair `from_source` installs). Resets every sim-keyed session
/// mirror (`StagePlans`, `MapPlanning`, the focus pin, the latched
/// weld cache, the script log, the notice cursor) and lands the
/// session in Flight — the restored world is a different vessel set,
/// so no stale binding may survive.
fn rebuild_from_snapshot(
    view: &mut View,
    snapshot: WorldSnapshot,
) -> Result<Vec<String>, SimError> {
    // C02-F2: the focus record rides the snapshot — read it before
    // `restore` consumes the struct, then re-anchor the session on it
    // below instead of falling to `vessels.last()`'s newest-spawn rule.
    let restored_focus = snapshot.active_vessel;
    let config = view.sim.config();
    let mut sim = SimWorld::restore(
        csp_sim::orbit::PatchedConics::new(config.orbits.clone()),
        config.physics.clone(),
        snapshot,
    )?;
    sim.install_config(&config);
    sim.set_script_engine(Box::new(csp_script::RhaiHost::new()));
    sim.set_autopilot_driver(Box::new(csp_ai::AscentAutopilot::new()));
    let warnings: Vec<String> = sim
        .missing_config_channels()
        .into_iter()
        .map(|c| format!("restored world is missing config channel `{c}`"))
        .collect();
    view.sim = sim;
    view.notice_cursor = None;
    // The revert point belongs to the world being replaced — a load
    // must never let F10/Game-menu revert resurrect the previous
    // session's snapshot over the loaded world (C21-F5/C22-F1), and a
    // stale delivery arm must not turn the loaded world's next
    // construction into a revert point it never launched.
    // `revert_to_launch` re-arms its own point after this call — that
    // restore IS the armed session's point.
    view.pre_launch = None;
    view.revert_capture = None;
    view.world
        .insert_resource(flight::LatchedWeldCache::default());
    view.world.insert_resource(flight::StagePlans::default());
    view.world.insert_resource(map::MapPlanning::default());
    view.world.insert_resource(ScriptLogMirror::default());
    // The restored vessels' previous-tick poses belong to the replaced
    // world — a drawn pose interpolated from them would slide the
    // restored craft in from where the old one flew.
    view.world.insert_resource(flight::PoseHistory::default());
    // The chase camera's anchor belongs to it as well: left in place,
    // the restored focus reads as a focus CHANGE and the camera travels
    // to the loaded craft from wherever the replaced session orbited.
    view.world
        .resource_mut::<flight::FlightInput>()
        .forget_anchor();
    // The restored `active_vessel` becomes the session's focus: the
    // quiet flight pin (no lease re-arm — the restored record carries
    // its own leases) re-anchors the camera/HUD — a rails record
    // stages through the C19-F2 hint — and the map's vessel focus
    // re-anchors itself in `sync_map` (or drops to the body default
    // when the vessel carries no conic, e.g. surface-latched).
    match restored_focus {
        Some(id) => {
            view.world
                .resource_mut::<flight::FlightFocusOverride>()
                .pin(id);
            view.world.resource_mut::<map::MapInput>().focus_vessel = Some(id);
        }
        None => {
            view.clear_flight_focus_override();
            view.world.resource_mut::<map::MapInput>().focus_vessel = None;
        }
    }
    // `FlightUiData.focus` is last frame's pin-aware resolution and
    // the value `resolved_focus`'s flight half reads — the pin only
    // reaches it through the next `flight_present` run. Leaving the
    // pre-load value would let the first post-load
    // `sync_focus_commands` resolve a stale VesselId that collides
    // with a live vessel in the restored arena, enqueueing
    // track/active commands for a vessel the player never focused.
    // Seed it from the restored record (`None` included — a stale id
    // must not outlive a focus-less restore) so the boundary resolves
    // the pin's own target until the present pass catches up.
    view.world.resource_mut::<flight::FlightUiData>().focus = restored_focus.map(|id| id.0);
    // The issued mirror restarts empty: the restored world's tracked
    // set may predate the boundary's commands, so the next frame
    // restamps the focus (track + active) fresh.
    view.focused_issued = None;
    view.set_mode(ViewMode::Flight);
    Ok(warnings)
}

/// Startup construction failure — [`View::new`] loads `data/` (§8.3's
/// "ALL tuning lives in data/"): a missing or invalid
/// `balance.ron`/`systems/home/` file is a startup error, surfaced
/// honestly, never a silent default (§2.13).
#[derive(Debug)]
pub enum ViewError {
    /// A `data/` file failed to load.
    Load(csp_data::LoadError),
    /// The loaded body defs did not build a patched-conics model.
    Sim(SimError),
    /// The loaded `data/` tree failed the §8.2 semantic validator —
    /// refused loudly at startup rather than shipped silently. Carries
    /// every failure the sweep found, not just the first.
    Validation(Vec<csp_data::ValidationError>),
    /// The §6.3 demo's seeded scenario failed to build — the embedded
    /// data set is internally inconsistent (a startup failure like
    /// the others).
    Demo(String),
    /// The shipping build's first-run starter craft failed to build
    /// ([`View::seed_starter_craft`]) — the same class of broken-data
    /// startup failure as [`ViewError::Demo`].
    Seed(String),
}

/// `from_source`'s loaded halves — every `data/` file kind the game
/// consumes, loaded once and §8.2-validated before a byte reaches the
/// sim. The `ViewError::Validation` refusal lives here so a bad tree
/// can never be installed silently.
struct LoadedDataset {
    system: csp_data::LoadedSystem,
    balance: csp_data::BalanceDef,
    experiments: csp_data::LoadedExperiments,
    techtree: csp_data::LoadedTechTree,
    sites: csp_data::SiteFile,
    contracts: csp_data::LoadedContracts,
    recipes: csp_data::LoadedRecipes,
    colonies: csp_data::ColonyFile,
    parts: csp_data::LoadedParts,
    resources: csp_data::ResourceFile,
    loc: csp_data::LocFile,
    /// `data/id-baseline.ron` — the §7.7 ID-stability baseline, when
    /// the data set authors one (the demo's reduced tree ships none).
    id_baseline: Option<csp_data::IdBaseline>,
}

/// §8.2: load the whole `data/` tree and run the semantic validator —
/// [`ViewError::Validation`] on any failure, carrying every error the
/// sweep found rather than the first.
fn load_validated_dataset(
    src: &dyn csp_data::DataSource,
    root: &std::path::Path,
) -> Result<LoadedDataset, ViewError> {
    let d = LoadedDataset {
        system: csp_data::load_system_from(src, &root.join("systems").join("home"))?,
        balance: csp_data::load_balance_from(src, &root.join("balance.ron"))?,
        experiments: csp_data::load_experiments_from(src, &root.join("experiments"))?,
        techtree: csp_data::load_techtree_from(src, &root.join("techtree"))?,
        sites: csp_data::load_sites_from(
            src,
            &root.join("systems").join("home").join("sites.ron"),
        )?,
        contracts: csp_data::load_contracts_from(src, &root.join("contracts"))?,
        recipes: csp_data::load_recipes_from(src, &root.join("recipes"))?,
        colonies: csp_data::load_colonies_from(
            src,
            &root.join("systems").join("home").join("colonies.ron"),
        )?,
        parts: csp_data::load_parts_from(src, &root.join("parts"))?,
        resources: csp_data::load_resources_from(src, &root.join("resources.ron"))?,
        loc: csp_data::load_loc_from(src, &root.join("loc").join("en.ron"))?,
        id_baseline: csp_data::load_id_baseline_from(src, &root.join("id-baseline.ron"))?,
    };
    let errors = csp_data::validate_dataset(&csp_data::Dataset {
        system: &d.system,
        balance: &d.balance,
        resources: &d.resources,
        sites: &d.sites,
        colonies: &d.colonies,
        recipes: &d.recipes,
        experiments: &d.experiments,
        techtree: &d.techtree,
        contracts: &d.contracts,
        parts: &d.parts,
        loc: &d.loc,
        id_baseline: d.id_baseline.as_ref(),
    });
    if !errors.is_empty() {
        return Err(ViewError::Validation(errors));
    }
    Ok(d)
}

/// The sim half of [`View::from_source`]: wire a `SimWorld` from an
/// already-validated dataset — orbit model, physics bubble, every
/// `set_*` construction-config channel, the free tech roots, and the
/// authored starting colonies. Kept as the ONE wiring pass so
/// non-presentation hosts (csp-bench's `WorldSpec::Data` eval worlds)
/// build the same world the game starts with instead of hand-rolling
/// a subset of these calls (the divergence class
/// [`SimWorld::install_config`]'s doc names).
fn build_sim_from_dataset(d: &LoadedDataset, save_seed: u64) -> Result<SimWorld, ViewError> {
    let balance = &d.balance;
    let defs: Vec<csp_data::BodyDef> = d.system.bodies.iter().map(|b| b.def.clone()).collect();
    let orbits = csp_sim::orbit::PatchedConics::from_body_defs(&defs).map_err(ViewError::Sim)?;
    // M4c presentation floor: the game world runs the physics
    // bubble over the home planet (same `Environment` convention
    // `csp-bench`'s flight scenarios use).
    let terrin = defs
        .iter()
        .find(|b| b.id.as_str() == "terrin")
        .ok_or(ViewError::Sim(SimError::Fatal(
            "home system has no 'terrin' body",
        )))?;
    let mut environment =
        csp_phys::Environment::for_body(terrin, csp_core::DVec3::ZERO, terrin.rotation_period_s);
    // C14-F3: the convective-heating η is authored — `for_body`
    // stamps the no-data default; the balance file overrides it.
    environment.convective_heat_fraction = balance.convective_heat_fraction;
    // Water physics: the ocean's density and drag are authored the same
    // way. One ocean, one water — a body's `ocean` says where the
    // surface is; `balance.ron` says what the water is.
    if let Some(ocean) = environment.ocean.as_mut() {
        ocean.density_kg_m3 = balance.water_density_kg_m3;
        ocean.drag_cd = balance.water_drag_cd;
    }
    let mut sim = SimWorld::with_physics(
        orbits,
        csp_sim::PhysicsConfig {
            environment,
            dominant: terrin.id.clone(),
            bubble_radius_m: balance.bubble_radius_m,
        },
        balance.advance_to_max_events_per_frame,
    );
    sim.set_vessel_caps(csp_sim::VesselCaps {
        max_vessels: balance.max_tracked_vessels,
        max_debris: balance.max_debris,
    });
    // §2.7's save seed — selects the contract-offer and
    // reliability draws. Never left at the `World::new` default:
    // without this every fresh game drew the identical offer
    // board and identical failure rolls (W1.5-a). Native gets
    // wall-clock entropy ([`native_save_seed`]); the §6.3 demo
    // passes [`demo::DEMO_SAVE_SEED`] so the fixed scenario stays
    // reproducible.
    sim.set_save_seed(save_seed);
    // M9a §2.10: the script host + autopilot driver (construction
    // config — re-installed identically by csp-bench's replay
    // harness) and the per-save op budget.
    sim.set_script_op_budget(balance.script_op_budget);
    sim.set_script_engine(Box::new(csp_script::RhaiHost::new()));
    sim.set_autopilot_driver(Box::new(csp_ai::AscentAutopilot::new()));
    sim.set_docking_config(csp_sim::DockingConfig::from_balance(balance))
        .map_err(ViewError::Sim)?;
    sim.set_transfer_config(csp_sim::TransferConfig::from_balance(balance));
    // M9b §3.4: the reliability tunables — dormant scale,
    // ignition window, effect magnitudes, test-stand economics.
    sim.set_reliability_config(csp_sim::ReliabilityConfig::from_balance(balance));
    // M7-A §3.2: the science tunables and the experiment-def table —
    // same data-derived wiring; without them science commands
    // would run on the no-data defaults and reject every run.
    sim.set_science_config(csp_sim::ScienceConfig::from_balance(balance));
    sim.set_experiments(
        d.experiments
            .by_id()
            .into_iter()
            .map(|(id, def)| (id, def.clone()))
            .collect(),
    );
    // M7-B §3.3: the merged tech tree + the part catalog the
    // `CraftDef::part_ids` gate resolves requirements against —
    // same data-derived wiring; without them research commands
    // reject with "no tech tree" and gated parts cannot resolve.
    sim.set_tech_tree(d.techtree.merged());
    sim.set_parts(
        d.parts
            .parts
            .iter()
            .map(|p| (p.def.id.clone(), p.def.clone()))
            .collect(),
    );
    // The authored starting capability: every prerequisite-free
    // zero-cost root (`tech.start` — the launch stack) is
    // researched in the baseline world, or the gate would lock
    // out the equipment the program starts with.
    sim.grant_free_tech_roots();
    // M7-C §3.5: the economy tunables (starting funds, the salary
    // cadence, hire cost, milestone payout, recovery shares) and
    // the system's site table — the same data-derived wiring;
    // without them the funds paths run on the no-data defaults
    // and `RecoverVessel` has no sites to name.
    sim.set_economy_config(csp_sim::economy::EconomyConfig::from_balance(balance))
        .map_err(ViewError::Sim)?;
    // The site's table collects first-wins on a duplicate id —
    // the `by_id()` convention (`or_insert`), not the `collect`
    // last-wins it replaced (the validator flags the duplicate
    // loudly either way; the tie-break just has to be one rule).
    sim.set_sites(
        d.sites
            .sites
            .iter()
            .fold(std::collections::BTreeMap::new(), |mut m, s| {
                m.entry(s.id.clone()).or_insert_with(|| s.clone());
                m
            }),
    );
    // M7-D §3.6: the contract template table + the board-size
    // tunable — the same data-derived wiring.
    sim.set_contracts(
        d.contracts
            .by_id()
            .into_iter()
            .map(|(id, def)| (id, def.clone()))
            .collect(),
    );
    sim.set_contract_config(csp_sim::ContractConfig::from_balance(balance));
    // M8-A §3.5.1: the per-body colony environments — rotation
    // and authored insolation from the loaded body defs (the
    // `set_sites` precedent).
    sim.set_colony_env(csp_sim::colony_env_from_body_defs(&defs));
    // M8-C §3.5: the life-support tunables — same data-derived
    // wiring (the `set_economy_config` precedent); without it the
    // check cadence/rates run on the no-data `DEFAULT_*` mirrors.
    // The per-save flag itself is sim state, not config — it
    // stays opt-in (`World::new` starts disabled).
    sim.set_life_support_config(csp_sim::LifeSupportConfig::from_balance(balance));
    // M8-B §3.5.1: the recipe table — the same data-derived
    // channel; `FoundColony`'s Converter modules resolve
    // `RecipeId`s against it.
    sim.set_recipes(
        d.recipes
            .by_id()
            .into_iter()
            .map(|(id, def)| (id, def.clone()))
            .collect(),
    );
    // M8-B §3.5.1: the home system's authored starting colonies
    // (`colonies.ron`) — founded at world build through the same
    // apply the `FoundColony` command drains to, after the site,
    // colony-env, and recipe tables are installed (the apply
    // resolves all three). A missing file reads as an empty set —
    // a system without authored colonies simply starts with none.
    sim.found_authored_colonies(&d.colonies.colonies)
        .map_err(ViewError::Sim)?;
    Ok(sim)
}

/// A `SimWorld` built from an authored `data/` tree — the same
/// world `View::new` starts with (orbit model, physics bubble,
/// parts/tech/contracts/colonies wired, `tech.start` granted),
/// minus the presentation resources and the first-run starter
/// craft. Native-only like `View::new`: reads the filesystem
/// through [`csp_data::FsDataSource`]. `csp-bench`'s eval tier
/// drives its `WorldSpec::Data` scenarios from this so the §8.5
/// proofs run on shipped content, not a fixture's subset (C30-F1).
#[cfg(not(target_arch = "wasm32"))]
pub fn load_sim_world(root: &std::path::Path, save_seed: u64) -> Result<SimWorld, ViewError> {
    let d = load_validated_dataset(&csp_data::FsDataSource, root)?;
    build_sim_from_dataset(&d, save_seed)
}

impl std::fmt::Display for ViewError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Load(e) => write!(f, "data/ startup load failed: {e}"),
            Self::Validation(errs) => {
                write!(f, "data/ failed semantic validation ({}):", errs.len())?;
                for e in errs.iter().take(10) {
                    write!(f, " {e}")?;
                }
                Ok(())
            }
            Self::Sim(e) => write!(f, "data/ bodies rejected by the orbit model: {e}"),
            Self::Demo(e) => write!(f, "demo scenario seed failed: {e}"),
            Self::Seed(e) => write!(f, "starter craft seed failed: {e}"),
        }
    }
}

impl std::error::Error for ViewError {}

impl From<csp_data::LoadError> for ViewError {
    fn from(e: csp_data::LoadError) -> Self {
        Self::Load(e)
    }
}

impl View {
    /// The startup path: §8.3's "ALL tuning lives in `data/`" made real —
    /// the §3.0 home system comes from `data/systems/home/` and the sim
    /// world's §2.6.5 event budget from `data/balance.ron`'s
    /// `advance_to_max_events_per_frame`. Missing/invalid data is a
    /// [`ViewError`], never a silent default. `data/` is resolved relative
    /// to the CWD, falling back to the repo layout relative to this
    /// crate's manifest (so tests/tools work from any directory).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn new() -> Result<Self, ViewError> {
        Self::from_source(&csp_data::FsDataSource, &data_root(), native_save_seed())
    }

    /// The M10-C shared half: load a `data/` tree from any
    /// [`csp_data::DataSource`] and wire the sim world + presentation
    /// resources from it. `View::new` (native) reads the filesystem;
    /// [`View::new_demo`] reads the §6.3 embedded set — identical wiring,
    /// identical file-shape checks, different source.
    #[allow(clippy::too_many_lines)] // one load = one wiring pass
    fn from_source(
        src: &dyn csp_data::DataSource,
        root: &std::path::Path,
        save_seed: u64,
    ) -> Result<Self, ViewError> {
        // §8.2: load the whole `data/` tree up front and run the
        // semantic validator BEFORE anything reaches the sim — the
        // same sweep the data tests run, so an invalid tree refuses
        // loudly at startup instead of shipping silently (the
        // loaders' "the validator flags it loudly" docs, made true).
        let d = load_validated_dataset(src, root)?;
        let sim = build_sim_from_dataset(&d, save_seed)?;
        let mut view = Self::with_sim(sim);
        // M3 VAB: the part catalog + resource registry are data-derived
        // constants for the session model (like BodyTable — §7.3's
        // re-derived-on-load class). The §3.1 aggregate rides along so
        // `BuiltPart::data_set_hash` gets the real stamp (C15-F2);
        // `data_set_hash` walks the filesystem, so a non-fs source
        // (the embedded demo set) installs `None` and the parts keep
        // the zero sentinel — nothing consumes the field yet.
        view.install_vab_catalog(d.parts, d.resources, data_set_hash(root).ok());
        // The catalog's display-name half: `PartDef::display_name` is a
        // `LocKey`, so the English table is what lets the palette print
        // "Sparc Launch Engine" instead of the raw part id — same
        // data-derived-constant class as the catalog itself.
        view.with_vab_session(|v| v.set_loc(d.loc.clone()));
        // R2: the authored terrain table (noise seeds, ocean datums,
        // decoded biome palettes) — installed over the empty default.
        let art = art::load();
        let mut tc = terrain::config_from_bodies(&d.system.bodies);
        tc.art = art.0.terrain.clone();
        view.world.insert_resource(tc);
        view.world.insert_resource(art);
        // The progression surface's data-derived constants: loc strings
        // (display names), decoded biome maps (the `RunExperiment`
        // biome context), and body radii (rails-altitude situations).
        let defs: Vec<csp_data::BodyDef> = d.system.bodies.iter().map(|b| b.def.clone()).collect();
        install_program_resources(&mut view, &d.system.bodies, &defs, &d.loc);
        Ok(view)
    }

    /// The §6.3 web demo — the fixed scenario over the embedded reduced
    /// data set ([`demo`]): Terrin only (the system manifest carries
    /// solen + terrin — no interplanetary body exists to reach), the
    /// editor part count capped at [`demo::DEMO_PART_LIMIT`], craft-file
    /// import/export refused. A flight-capable starter craft is seeded
    /// on the pad and pre-loaded in the VAB session.
    ///
    /// Compiled on every target: wasm's `View::new` IS this, and native
    /// tests exercise the same construction.
    pub fn new_demo() -> Result<Self, ViewError> {
        let mut view = Self::from_source(
            &demo::source(),
            std::path::Path::new(demo::DEMO_ROOT),
            demo::DEMO_SAVE_SEED,
        )?;
        view.demo = true;
        view.with_vab_session(|v| {
            v.set_part_limit(Some(demo::DEMO_PART_LIMIT));
            v.set_file_io(false);
        });
        demo::seed_pad_craft(&mut view, demo::DEMO_CRAFT_NAME).map_err(ViewError::Demo)?;
        Ok(view)
    }

    /// The shipping build's first-run seed — `csp-app` calls it once
    /// after [`View::new`]. Spawns the stock starter craft ("Stock
    /// Hopper") on the pad through the same
    /// `launch_craft` command path the §6.3 demo seeds with, and leaves
    /// the same craft loaded in the VAB session, so the app opens on a
    /// real flight scene — terrain, sky, and a flyable vessel — instead
    /// of the empty void a Flight mode with nothing InBubble produces
    /// (and the editor opens on an editable craft, not a blank
    /// session). A failure is a broken data set — [`ViewError::Seed`],
    /// a startup error like the loaders'.
    ///
    /// wasm never calls this: its [`View::new`] IS [`View::new_demo`],
    /// which self-seeds inside the constructor.
    pub fn seed_starter_craft(&mut self) -> Result<(), ViewError> {
        demo::seed_pad_craft(self, demo::STARTER_CRAFT_NAME).map_err(ViewError::Seed)
    }

    /// wasm: the §6.3 demo IS the web tier — `View::new` builds the
    /// embedded fixed scenario; the full `data/` path is native-only.
    #[cfg(target_arch = "wasm32")]
    pub fn new() -> Result<Self, ViewError> {
        Self::new_demo()
    }

    /// The shell around an explicit sim world — the headless/test
    /// constructor (`new` is the data-loading startup path).
    #[allow(clippy::too_many_lines)] // a constructor: one resource insert per line, no branches
    pub fn with_sim(sim: SimWorld) -> Self {
        let mut world = World::new();
        world.insert_resource(Clock::default());
        world.insert_resource(FrameStats::default());
        world.insert_resource(WarpMirror::default());
        world.insert_resource(SessionMirror::default());
        world.insert_resource(EguiPass::default());
        world.insert_resource(UiOutbox::default());
        world.insert_resource(SyncInput::default());
        // The §2.6.6-item-4 scan mirror — `sync_mirrors`'s fifth
        // write target (Clock/WarpMirror/FrameStats are the others).
        world.insert_resource(SoiScanMirror::default());

        // M1-D map resources (see map.rs): the body table/orbit model are
        // data-derived constants; MapInput/MapSnapshot/… are session state.
        let (body_table, map_input, orbit_model) = map::build_resources(&sim);
        world.insert_resource(body_table);
        world.insert_resource(map_input);
        world.insert_resource(orbit_model);
        world.insert_resource(map::MapInputEvents::default());
        world.insert_resource(map::MapSyncInput::default());
        world.insert_resource(map::MapSnapshot::default());
        world.insert_resource(map::MapScene::default());
        world.insert_resource(map::MapCameraF64::default());
        world.insert_resource(map::MapUiData::default());
        world.insert_resource(flight::FlightFocusOverride::default());
        world.insert_resource(map::MapFrame::default());
        world.insert_resource(ManeuverPanelRes::default());
        // W2.4: the planning surface — the panel's session state and
        // the alarm book + staged transfer plan it works against.
        world.insert_resource(PlanningPanelRes::default());
        world.insert_resource(map::MapPlanning::default());
        // M10: the settings window's staged state (§2.2-C session data).
        world.insert_resource(SettingsPanelRes::default());
        // The Game menu (save/load/revert/new-game) — the staged slot
        // list + armed confirmations (§2.2-C session state).
        world.insert_resource(GameMenuRes::default());
        // The optional-panel toggles (debug scaffold, controls help) —
        // session state flipped by the ToggleDebug/ToggleHelp keys.
        world.insert_resource(PanelFlags::default());
        // M10: the first-flight tutorial checklist (§2.2-C session
        // data) — driven by `tutorial_drive` in Presentation, drawn by
        // `build_ui`.
        world.insert_resource(tutorial::TutorialRes::default());

        // M3 VAB resources (see vab.rs): the working-craft session +
        // camera/pick state + the craft-file name list.
        world.insert_resource(vab::VabSessionRes::default());
        world.insert_resource(vab::VabInputEvents::default());
        world.insert_resource(vab::VabInput::default());
        world.insert_resource(vab::VabScene::default());
        world.insert_resource(vab::VabCameraF64::default());
        world.insert_resource(vab::VabFrame::default());
        world.insert_resource(vab::CraftFileList::default());

        // M4c flight resources (see flight.rs): chase camera + the
        // staged InBubble craft mirrors.
        world.insert_resource(flight::FlightInput::default());
        // Separate from `map::MapCameraF64` — the two `*_present`
        // systems share one unordered tuple, so a shared resource
        // would be a last-writer-wins race.
        world.insert_resource(flight::FlightCameraF64::default());
        world.insert_resource(flight::FlightInputEvents::default());
        world.insert_resource(flight::FlightSyncInput::default());
        world.insert_resource(flight::FlightSnapshot::default());
        insert_interp_resources(&mut world);
        // The non-debris vessel count across all residences — the
        // tutorial's alive-or-lost mirror (P12-F2).
        world.insert_resource(flight::LiveVesselCount::default());
        // The rails-staged id set — the C19-F2 focus handoff's mirror
        // (same standalone-resource pattern as `LiveVesselCount`).
        world.insert_resource(flight::RailsStaged::default());
        // The staged bounding radii — the chase camera's framing input
        // (same standalone-resource pattern).
        world.insert_resource(flight::CraftRadii::default());
        // The propulsion walk mirror — the HUD's live TWR/Δv/
        // burn-time figures (same standalone-resource pattern as
        // `LiveVesselCount`: `FlightSnapshot` cannot grow).
        world.insert_resource(flight::VesselPropulsionStaged::default());
        // The §3.4 flight display's sustained-g mirror — staged each
        // `end_frame` beside the flight vessels it measures.
        world.insert_resource(flight::RegimeLoads::default());
        world.insert_resource(flight::FlightUiData::default());
        world.insert_resource(flight::StagePlans::default());
        world.insert_resource(flight::FlightFrame::default());
        // C1: the extract's geometry cache — baked launch-complex and
        // part-silhouette/model triangles keyed by layout/shape
        // identity. Pure presentation data; position/orientation stay
        // per-frame and never enter the keys.
        world.insert_resource(flight::FlightGeomCache::default());
        // §2.6.4: the latched-vessel weld cache — the staging path's
        // session state, so a parked craft isn't re-welded per frame.
        world.insert_resource(flight::LatchedWeldCache::default());
        // R2 surface resources (see terrain.rs): empty config ⇒
        // per-body params synthesised from the body id; `View::new`
        // installs the authored table over it.
        world.insert_resource(terrain::TerrainConfig::default());
        world.insert_resource(terrain::TerrainCache::default());
        // The authored site table — a data-derived constant like
        // `BodyTable` above (`sim.config()`'s clone runs once, here at
        // construction — never per frame).
        world.insert_resource(structures::SiteTable::from_sim(&sim));
        world.insert_resource(art::ArtAssets::default());
        // Progression surface resources (ProgramMirror is staged by
        // `stage_program` each `end_frame`; LocStrings/BiomeMaps/
        // BodyRadii are data-derived constants `from_source` fills in).
        world.insert_resource(ProgramMirror::default());
        // The right-click part menu's session state.
        world.insert_resource(PartMenuRes::default());
        world.insert_resource(ScriptLogMirror::default());
        world.insert_resource(LocStrings::default());
        world.insert_resource(BiomeMaps::default());
        world.insert_resource(BodyRadii::default());
        // §3.7 visual-SFX channel (flight.rs carries the audio deferral).
        world.insert_resource(flight::FlightSfx::default());

        // §2.5: "SimStep and Sync use the single-threaded executor always."
        let mut input = Schedule::default();
        input.add_systems((
            map::apply_map_input,
            vab::apply_vab_input,
            flight::apply_flight_input,
        ));
        let mut sim_step = Schedule::default();
        sim_step.set_executor(SingleThreadedExecutor::new());
        let mut sync = Schedule::default();
        sync.set_executor(SingleThreadedExecutor::new());
        sync.add_systems((sync_mirrors, map::sync_map, flight::sync_flight));

        let mut presentation = Schedule::default();
        presentation.add_systems((
            map::map_present,
            vab::vab_present,
            // §2.5: the drawn poses exist before anything solves
            // against them (the chase camera, the extract).
            flight::flight_interp.before(flight::flight_camera),
            flight::flight_camera,
            flight::flight_present,
            // M10: the tutorial's progress observer — reads the
            // Sync-written snapshot directly, so it stays independent
            // of sibling ordering inside this schedule.
            tutorial::tutorial_drive,
        ));
        let mut render_extract = Schedule::default();
        render_extract.add_systems((map::extract_map, vab::extract_vab, flight::extract_flight));

        let mut ui_build = Schedule::default();
        ui_build.add_systems((build_ui, map_pointer_input.after(build_ui)));

        Self {
            sim,
            world,
            schedules: Schedules {
                input,
                sim_step,
                sync,
                presentation,
                ui_build,
                render_extract,
            },
            ticks_last_frame: 0,
            demo: false,
            notice_cursor: None,
            pre_launch: None,
            revert_capture: None,
            focused_issued: None,
        }
    }

    /// `true` when this is the §6.3 web-demo world ([`View::new_demo`]) —
    /// `csp-app` reads it to disable craft-file import/export and gate
    /// demo-off features.
    pub fn is_demo(&self) -> bool {
        self.demo
    }

    /// Ticks the last [`View::sim_step`] call actually advanced (0 on a
    /// halted or zero-advance frame). `csp-app` divides its per-frame
    /// `step_frame` wall measurement by this to get the per-tick cost
    /// §2.6.3's inequality is written in. Reset to 0 by `end_frame` after
    /// staging into `SyncInput` — read it between `sim_step` and
    /// `end_frame`, or read the mirrored copy via `FrameStats` inside
    /// systems.
    pub fn ticks_last_frame(&self) -> u64 {
        self.ticks_last_frame
    }

    /// Queue a `SimCommand` for the next tick boundary (§2.2.1). The tick and
    /// per-source seq stamp at dequeue inside `step_frame` — enqueueing here
    /// touches only the in-flight queue, never sim state.
    pub fn enqueue_command(
        &mut self,
        source_class: SourceClass,
        source_id: u32,
        command: SimCommand,
    ) {
        self.sim.enqueue(source_class, source_id, command);
    }

    /// Once per frame: Input (§2.5 #1 — before SimStep, always).
    pub fn begin_frame(&mut self) {
        self.schedules.input.run(&mut self.world);
    }

    /// The SimStep stage (§2.5 #2, §2.6.2): a single
    /// `csp_sim::World::step_frame(target_tick)` call — the whole frame's
    /// advance is exactly one step, what §2.5's "a single system" prescribes.
    /// The sim lives outside the ECS `World` (§2.2), so the call itself sits
    /// in this method — a bevy system could not reach `self.sim` — and the
    /// `sim_step` schedule (single-threaded always) then runs any
    /// SimStep-adjacent systems once per frame.
    ///
    /// The caller (csp-app's warp controller) computes `target_tick` from
    /// wall time and warp level; this layer never derives targets —
    /// §2.6.3 makes the sim's advance driven, never measured.
    ///
    /// Returns the call's [`AdvanceReport`] — `csp-app` reads
    /// `fired_events` for the §2.6.5 Clamping demotion.
    ///
    /// Propagates `SimError` (§2.13): on `Fatal` the frame is abandoned and
    /// the caller decides how to surface it — the presentation layer does
    /// not swallow sim failures.
    pub fn sim_step(&mut self, target_tick: u64) -> Result<AdvanceReport, SimError> {
        let start = self.sim.clock().ticks();
        // The non-empty → empty arena edge, captured BEFORE the drain:
        // a post-removal escape below needs the edge, not the level
        // (a world that never held a vessel — boot before the starter
        // seed's build delivers — must keep its Flight default).
        let had_vessels = !self.sim.vessels().is_empty();
        // The builds-edge half of the same signal: a `CancelBuild`
        // (or a delivery that dropped its record without
        // materializing) empties the build arena with NO vessel
        // edge at all — the wait below needs this captured
        // pre-drain or the cancelled-order player strands in the
        // empty Flight scene the bounce exists to leave.
        let had_builds = !self.sim.builds().is_empty();
        let report = self.sim.step_frame(target_tick)?;
        self.ticks_last_frame = report.reached_tick - start;
        // C02-F1/C02-F2's focus boundary rides THIS seam — AFTER the
        // drain, so the arena reflects every removal this step's
        // earlier commands committed (a RecoverVessel batch can drop
        // the focused id mid-step): a stale `ui.focus` naming it
        // defers rather than firing a Recoverable rejection for a
        // dead vessel. The enqueued track/active commands drain ahead
        // of anything enqueued later — a same-batch removal can never
        // beat them.
        self.sync_focus_commands();
        // The post-removal escape: this step's drain took the LAST
        // vessel out of the arena while the player was in Flight —
        // `resolved_focus` now has nothing to resolve, `extract_flight`
        // emits only the fallback sky, and no input path leads out
        // (the "purple void" C24-F1 documented for boot — the same
        // dead scene, mid-session, on the last `RecoverVessel` or a
        // terminal impact). Land the player in the VAB — the working
        // session outlives its vessel and holds the very craft they
        // just flew — rather than a frame nothing reacts in. Gated on
        // the EDGE (`had_vessels`): a vessel-free world from boot
        // keeps Flight until the seed delivers, and Map/VAB keep
        // their own surfaces — only an empty Flight scene is dead.
        // The pending-order carve-out: an empty arena with an
        // `ActiveBuild` still on the books is a delivery in flight,
        // not a dead scene — `launch_craft`'s own pad-clearing
        // `RecoverVessel` drains the occupant in the SAME step that
        // armed the order, and bouncing Flight→Vab there would hide
        // the launch behind a "nothing happened" (the build lands
        // ~build_ticks later with nobody watching). If the order
        // rejected at drain, `builds()` is empty and the bounce still
        // catches the truly-dead scene.
        // The rejected-order edge the `had_vessels` gate can't see:
        // a `ManufactureCraft`/`LaunchCraft` that refuses AT ITS DRAIN
        // on an ALREADY-empty arena (the player recovered the pad's
        // last vessel before pressing LAUNCH, or a hand-queued order)
        // has no vessel-removal edge at all — `launch_craft` switched
        // to Flight at enqueue, so a refuse leaves `mode=Flight` with
        // `vessels()`/`builds()` both empty and no exit path. The
        // typed `RejectKind` carries the refusal (the `ToBubble`
        // precedent — `reason()` text is never branch-determining);
        // the sim's own reason lands on the VAB notice row rather
        // than a transient "COMMAND REFUSED" cue the scene hides.
        let order_reject = report.rejected.iter().find_map(|r| {
            (r.key.source_class == SourceClass::Ui
                && r.key.source_id == PLAYER_SOURCE_ID
                && matches!(
                    r.kind,
                    csp_core::RejectKind::ManufactureCraft | csp_core::RejectKind::LaunchCraft
                ))
            .then(|| (r.kind, r.error.reason()))
        });
        // The builds edge: `builds()` was non-empty before the drain
        // and is empty now with no vessel delivered — the order
        // left the books without a product, which is a `CancelBuild`
        // (or a dropped delivery, e.g. a vanished site). The vessel
        // edge can't see it — nothing was removed, the wait simply
        // ended — so without it a cancel in Flight lands the player
        // on the same dead empty scene the vessel edge covers.
        let builds_edge = had_builds && self.sim.builds().is_empty();
        if (had_vessels || order_reject.is_some() || builds_edge)
            && self.sim.vessels().is_empty()
            && self.sim.builds().is_empty()
            && self.mode() == ViewMode::Flight
        {
            self.set_mode(ViewMode::Vab);
            let msg = if let Some((kind, reason)) = order_reject {
                match kind {
                    csp_core::RejectKind::LaunchCraft => format!("launch refused — {reason}"),
                    _ => format!("build order refused — {reason}"),
                }
            } else if !had_vessels {
                "build order ended — the arena is empty; back in the VAB".to_string()
            } else {
                "vessel recovered — the arena is empty; back in the VAB".to_string()
            };
            self.vab_event(vab::VabEvent::Notice(msg));
        }
        // The launch revert point's delivery boundary (PLAN:1177-1180 —
        // the sim contract at `manufacture.rs` expects the post-delivery
        // snapshot): `launch_craft` captured the pre-ORDER world — its
        // own commands were necessarily still pending there — so the
        // point is re-taken once the ARMED order's delivery lands the
        // craft surface-latched (see `advance_revert_capture` for the
        // match; a foreign `ConstructionComplete` must not spend the
        // arm). `Delivered` retries every step until the §7.4 boundary
        // admits a snapshot: a transient refusal (live contacts, a
        // joint island, a partial drain) must not strand the point at
        // the pre-order capture, which is the "revert un-pays the
        // build" hole this recapture exists to close.
        self.advance_revert_capture(&report);
        if matches!(self.revert_capture, Some(RevertCapture::Delivered))
            && let Ok(snap) = self.sim.snapshot()
        {
            self.pre_launch = Some(Ok(snap));
            self.revert_capture = None;
        }
        // The focus pin's drain-reject release (P04-F2): `end_frame`
        // covers the pinned vessel gone from the arena and
        // `flight_present` the staged-then-left case, but a `ToBubble`
        // that drained and REJECTED (the SOI-TOCTOU class the
        // eligibility check cannot see — an event re-parenting the
        // conic inside the same drain) leaves the vessel on rails and
        // the pin armed forever, hijacking focus on any later
        // staging. A `ToBubble` rejection on the player channel is
        // the recover's own command failing — release the pending pin
        // and say so on the global cue channel. The match is the
        // typed `RejectKind`, never `reason()` text: a `ToBubble`
        // that fails at its weld/`state_at`/aero step reports a
        // reason with no "to-bubble" prefix at all, and the string
        // match would leave the pin armed. Gated on `pending()`:
        // a `seen` pin's vessel already converted, so a later
        // to-bubble rejection cannot be ours.
        let to_bubble_reject = report.rejected.iter().any(|r| {
            r.key.source_class == SourceClass::Ui
                && r.key.source_id == PLAYER_SOURCE_ID
                && r.kind == csp_core::RejectKind::ToBubble
        });
        if to_bubble_reject
            && self
                .world
                .resource::<flight::FlightFocusOverride>()
                .pending()
        {
            self.clear_flight_focus_override();
            self.flight_notice("RECOVER FAILED — CONVERSION REFUSED".to_string());
        }
        // W2.6/report-08 L3: the notification ring was window-only —
        // a dormant failure or misfire on the flying vessel reached
        // the player only through the Program LOG (and only as lost
        // thrust in flight). New entries naming the FOCUSED vessel
        // post one HUD toast each; the ring keeps them regardless —
        // the toast is surfacing, not the record.
        self.drain_part_notices();
        // W2.4: fold fired alarms/watches into the planning mirror —
        // armed book entries mark fired, and a fire the panel never
        // armed (a script's or MCP alarm) still records a row, so the
        // surfacing contract is view-side visible, not app-only.
        self.world
            .resource_mut::<map::MapPlanning>()
            .note_fired(&report.fired_events);
        self.schedules.sim_step.run(&mut self.world);
        Ok(report)
    }

    /// The `AwaitingDelivery` arm's per-step matcher — see
    /// [`RevertCapture`]. Run after `step_frame` so the step's drains,
    /// log writes, and event firings are all visible.
    ///
    /// The arm names the ORDER, not the event: `EventKind::
    /// ConstructionComplete { build }` carries the `BuildId`, but the id
    /// is minted inside the drain — the enqueue site cannot know it,
    /// and a foreign order ahead in the same drain takes the lower
    /// ids — so the match runs on the order's identity: `(site, craft)`
    /// for the product plus `min_build_id` (`builds().next_back() + 1`
    /// at arm time — `next_build_id` is not observable) bounding the
    /// `builds()` scan to records minted at-or-after this order.
    ///
    /// Phase one proves the order APPLIED: `command_log` is the applied
    /// channel — a drain-rejected `ManufactureCraft`/`LaunchCraft`
    /// writes no entry
    /// (its `source_seq` and effects roll back; the rejection reports
    /// only on `report.rejected`, which carries no command identity),
    /// and the player FIFO drains at the first boundary the frame
    /// crosses, stamped `armed_tick`. A step whose `reached_tick` has
    /// passed `armed_tick` therefore consumed the order; no matching
    /// log entry means it REJECTED. The arm then disarms rather than
    /// waiting on a delivery that can never come and latching onto an
    /// unrelated later `ConstructionComplete` — `pre_launch` keeps the
    /// pre-order capture, the honest point for a launch whose order
    /// never took.
    ///
    /// Phase two waits out the build — or skips the wait entirely for
    /// a `LaunchCraft`, whose applied log entry means the craft already
    /// berths (no `ActiveBuild` is ever minted). An `ActiveBuild`
    /// leaves the arena only at its terminal `ConstructionComplete`
    /// firing — a berth-blocked delivery re-queues the record with a
    /// fresh deadline — so "applied and no matching record in flight"
    /// IS the delivery boundary. Two pending builds of an identical craft to
    /// the same site cannot be told apart by any key the sim exposes
    /// (the `vessel_for` ambiguity class): the match then waits out
    /// every matching record, so a wrong pick can only recapture LATE
    /// (the point moves to a world where an extra identical order may
    /// still be building) — never EARLY, which would re-pay the build
    /// wait this recapture exists to erase.
    fn advance_revert_capture(&mut self, report: &AdvanceReport) {
        let Some(RevertCapture::AwaitingDelivery {
            site,
            craft,
            min_build_id,
            armed_tick,
        }) = &self.revert_capture
        else {
            return;
        };
        let armed_tick = *armed_tick;
        let min_build_id = *min_build_id;
        let (mut applied, mut berthed) = (false, false);
        for (key, command) in self
            .sim
            .command_log()
            .iter()
            .rev()
            .take_while(|(key, _)| key.tick >= armed_tick)
        {
            if key.source_class != SourceClass::Ui || key.source_id != PLAYER_SOURCE_ID {
                continue;
            }
            match command {
                SimCommand::ManufactureCraft {
                    site: s, craft: c, ..
                } if *s == *site && *c == *craft => {
                    applied = true;
                }
                SimCommand::LaunchCraft {
                    site: s, craft: c, ..
                } if *s == *site && *c == *craft => {
                    // `LaunchCraft` IS its delivery — an applied entry
                    // means the craft already berths in this world's
                    // state, so the arm goes straight to `Delivered`
                    // without a `builds()` record to wait out.
                    applied = true;
                    berthed = true;
                }
                _ => {}
            }
        }
        if !applied {
            if report.reached_tick > armed_tick {
                self.revert_capture = None;
                self.flight_notice(
                    "BUILD ORDER REFUSED AT APPLY — revert point holds the pre-order world",
                );
            }
            return;
        }
        let building = !berthed
            && self
                .sim
                .builds()
                .values()
                .any(|b| b.id.0 >= min_build_id && b.site == *site && b.craft == *craft);
        if !building {
            self.revert_capture = Some(RevertCapture::Delivered);
        }
    }

    /// Once per frame, after SimStep: Sync → Presentation → UiBuild →
    /// RenderExtract (PLAN §2.5 ordering, Appendix A item 2). Stages the
    /// post-step sim values into `SyncInput` first so the mirror writes are
    /// the `sync` schedule's [`sync_mirrors`] system, not bare statements.
    pub fn end_frame(&mut self) {
        // §2.6.6 item 4: the debug overlay's `h` report rides the
        // same SyncInput→mirror staging as the clock — the values are
        // the scan's own (computed at `Self::soi_scan_report`), never
        // re-derived presentation-side.
        let soi_scan = self.soi_scan_report();
        self.world.insert_resource(SyncInput {
            clock: self.sim.clock(),
            warp_level: self.sim.warp_level(),
            ticks_last_frame: self.ticks_last_frame,
            soi_scan,
        });
        // The map's sim reads are staged here too (§2.5: Sync/Presentation
        // never read csp-sim) — vessel orbit states + the clock; positions
        // and polylines derive from OrbitModelRes inside the schedules.
        self.world
            .insert_resource(map::capture_sync_input(&self.sim));
        // M4c: the flight view's InBubble craft mirrors — same
        // staging convention (the schedules never read `sim`). The
        // §2.6.4 latched vessels reuse `LatchedWeldCache` instead of
        // re-welding per frame.
        let staged_flight = {
            // C19-F2's rails-staging hint — the vessels the camera may
            // need to follow through a bubble→rails conversion: the
            // armed focus pin's target and last frame's resolved focus
            // (`FlightUiData.focus`, still un-run this frame). Only
            // these can stage off-rails; see `capture_sync_input`.
            // Read BEFORE the `cache` mutable borrow below.
            let mut rails_hint: Vec<csp_sim::VesselId> = Vec::with_capacity(2);
            if let Some(t) = self.world.resource::<flight::FlightFocusOverride>().target {
                rails_hint.push(t);
            }
            if let Some(f) = self.world.resource::<flight::FlightUiData>().focus {
                let f = csp_sim::VesselId(f);
                if !rails_hint.contains(&f) {
                    rails_hint.push(f);
                }
            }
            let art = self.world.resource::<art::ArtAssets>().clone();
            let mut cache = self.world.resource_mut::<flight::LatchedWeldCache>();
            flight::capture_sync_input(&self.sim, &mut cache, &art, &rails_hint)
        };
        self.world.insert_resource(staged_flight);
        // The focus pin's sim-side release rule: a pinned vessel gone
        // from the arena (impact removal, a despawn) can never stage,
        // so the pin is dead — clear it here where `sim` is readable.
        // `flight_present` covers the staged-then-left case.
        let pin = self.world.resource::<flight::FlightFocusOverride>().target;
        if pin.is_some_and(|id| !self.sim.vessels().contains_key(&id)) {
            self.clear_flight_focus_override();
        }
        self.ticks_last_frame = 0;
        // M7-B §3.3: restage the VAB's progression gate — research
        // drained this frame flips palette availability next frame.
        self.stage_vab_tech();
        // M7/M9b: the Program window's mirror — economy, contracts,
        // tech, and the focused vessel's science/reliability rows.
        self.stage_program();
        // The Game menu's revert mirror — same staging rule.
        self.world.resource_mut::<GameMenuRes>().revert_tick = self.revert_tick();
        self.schedules.sync.run(&mut self.world);
        self.schedules.presentation.run(&mut self.world);
        // P04-F3: the recover-control Player-lease transfer issues on
        // the pin's `seen` edge — `flight_present` just proved the
        // `ToBubble` materialized the vessel — never optimistically at
        // press time (a rejected conversion used to still preempt the
        // vessel's live `Script`/`Agent` lease). The command rides the
        // player FIFO and drains at the next tick boundary.
        if let Some(target) = self
            .world
            .resource::<flight::FlightFocusOverride>()
            .lease_target()
        {
            self.enqueue_command(
                SourceClass::Ui,
                PLAYER_SOURCE_ID,
                SimCommand::TakeControl {
                    vessel: target,
                    lease: csp_sim::Lease::Player,
                },
            );
            self.world
                .resource_mut::<flight::FlightFocusOverride>()
                .lease_pending = false;
        }
        self.schedules.ui_build.run(&mut self.world);
        self.schedules.render_extract.run(&mut self.world);
    }

    /// The vessel the session's focus names — the map's explicit
    /// vessel focus while the map is up (a click-tracked vessel IS
    /// the player's "watch this" mark — the ×10⁵-through-an-SOI case
    /// §2.6.5's Clamping row exists for), else the pin-aware flight
    /// resolution. Session state, never sim state.
    fn resolved_focus(&self) -> Option<csp_sim::VesselId> {
        if self.mode() == ViewMode::Map
            && let Some(v) = self.world.resource::<map::MapInput>().focus_vessel
        {
            return Some(v);
        }
        self.flight_focus()
    }

    /// §2.6.6 item 4's report for the resolved-focus vessel, flattened
    /// to the panel's plain rows for `SyncInput`→[`SoiScanMirror`].
    /// The strides come from [`SimWorld::soi_scan_strides`] — the
    /// scan's own helpers, so the overlay can never disagree with the
    /// scan it describes. A non-rails focus carries no stamped conic
    /// (the `schedule_next_soi` invariant: SOI events are only ever
    /// scheduled while on rails), and a model refusal is reported the
    /// same way — `unavailable` always names the reason rather than
    /// showing an empty list that reads as "no SOIs exist".
    fn soi_scan_report(&self) -> csp_ui::SoiScanReport {
        let Some(vessel) = self.resolved_focus() else {
            return csp_ui::SoiScanReport::default();
        };
        match self.sim.soi_scan_strides(vessel) {
            Ok(Some(scans)) => csp_ui::SoiScanReport {
                vessel: Some(vessel.0),
                rows: scans
                    .iter()
                    .map(|s| csp_ui::SoiScanRow {
                        body: s.body.as_str().to_string(),
                        stride_ticks: s.stride_ticks,
                        v_rel_max_mps: s.v_rel_max_m_per_s,
                    })
                    .collect(),
                // Same honesty as the None/Err arms — an empty scan
                // names its reason rather than rendering zero rows.
                unavailable: scans
                    .is_empty()
                    .then(|| "no SOI candidates — no encounters in the scan horizon".into()),
            },
            Ok(None) => csp_ui::SoiScanReport {
                vessel: Some(vessel.0),
                rows: Vec::new(),
                unavailable: Some("focused vessel not on rails — its conic isn't scanned".into()),
            },
            Err(e) => csp_ui::SoiScanReport {
                vessel: Some(vessel.0),
                rows: Vec::new(),
                unavailable: Some(e.to_string()),
            },
        }
    }

    /// C02-F1/C02-F2: the view's focus boundary pushes the resolved
    /// focus into sim state when it changes — untrack the previous
    /// focus's `SetVesselTracked` (skipped when it already left the
    /// arena — the removal paths scrub it), track the new one
    /// (skipped when already tracked — a restored or agent-tracked
    /// vessel needs no re-issue), and record §2.2's `active_vessel`
    /// (skipped when the record already matches). Runs from
    /// [`Self::sim_step`] right after `step_frame` — the drain just
    /// applied this step's batches, so a resolved focus naming a
    /// vessel the arena already dropped defers (the corrected focus
    /// resolves next frame) instead of landing a `Recoverable`
    /// rejection for a dead id. `focused_issued` is the issue mirror
    /// — not the sim's fields — because the sim can't see a
    /// queued-but-undrained command.
    fn sync_focus_commands(&mut self) {
        let resolved = self.resolved_focus();
        if resolved == self.focused_issued {
            return;
        }
        if resolved.is_some_and(|v| !self.sim.vessels().contains_key(&v)) {
            return;
        }
        if let Some(old) = self.focused_issued
            && self.sim.vessels().contains_key(&old)
        {
            self.enqueue_command(
                SourceClass::Ui,
                PLAYER_SOURCE_ID,
                SimCommand::SetVesselTracked {
                    vessel: old,
                    tracked: false,
                },
            );
        }
        if let Some(new) = resolved
            && !self.sim.tracked_vessels().contains(&new)
        {
            self.enqueue_command(
                SourceClass::Ui,
                PLAYER_SOURCE_ID,
                SimCommand::SetVesselTracked {
                    vessel: new,
                    tracked: true,
                },
            );
        }
        if self.sim.active_vessel() != resolved {
            self.enqueue_command(
                SourceClass::Ui,
                PLAYER_SOURCE_ID,
                SimCommand::SetActiveVessel { vessel: resolved },
            );
        }
        self.focused_issued = resolved;
    }

    /// Stage this frame's `egui::RawInput` for the `UiBuild` system —
    /// input plumbing, not a UI read of the world (§2.5).
    pub fn set_egui_input(&mut self, input: egui::RawInput) {
        self.world.resource_mut::<EguiPass>().input = input;
    }

    /// One-shot GPU identity for the debug HUD — session info (§2.2 C).
    pub fn set_gpu_info(&mut self, adapter: String, backend: String) {
        let mut m = self.world.resource_mut::<SessionMirror>();
        m.adapter = adapter;
        m.backend = backend;
    }

    /// Per-frame session plumbing for the UI: wall dt, the warp
    /// controller's requested/effective multipliers, and the §2.13 halt
    /// state. Written between frames, never read by the sim.
    pub fn update_session(
        &mut self,
        dt_wall_ms: f64,
        warp_requested: u64,
        warp_effective: u64,
        fatal: Option<SimError>,
    ) {
        let mut m = self.world.resource_mut::<SessionMirror>();
        m.dt_wall_ms = dt_wall_ms;
        m.warp_requested = warp_requested;
        m.warp_effective = warp_effective;
        m.fatal = fatal;
    }

    /// P1's FPS readout plumbing: the app's smoothed wall-clock frame
    /// timing (measured at §2.5's one legal Instant site, averaged
    /// over its ~10-frame window) written into the flight HUD's mirror
    /// for `UiBuild` to stage. Presentation data only — never read by
    /// the sim, never part of any hash.
    pub fn set_frame_timing(&mut self, fps: f64, frame_ms: f64) {
        let mut r = self.world.resource_mut::<flight::FlightUiData>();
        r.fps = fps;
        r.frame_ms = frame_ms;
    }

    /// The frame-phase split's plumbing (`set_frame_timing`'s sibling,
    /// the 20-fps-report diagnostic): the app's smoothed cpu-residue vs
    /// acquire/present wait, written into the flight mirror for the F3
    /// debug panel to stage. Same presentation-only contract.
    pub fn set_frame_phase(&mut self, cpu_ms: f64, present_ms: f64) {
        let mut r = self.world.resource_mut::<flight::FlightUiData>();
        r.cpu_ms = cpu_ms;
        r.present_ms = present_ms;
    }

    /// Hand the frame its fractional tick remainder (§2.5 interpolation)
    /// — `csp-app` calls this every frame, between the step and
    /// [`Self::end_frame`], with its warp controller's figure for the
    /// tick the step reached. The next `end_frame` draws every staged
    /// pose between its previous and current tick at that fraction.
    pub fn set_tick_fraction(&mut self, frac: f64) {
        self.world.resource_mut::<TickFraction>().0 = frac;
    }

    /// Update only the §2.13 halt mirror — for the frame a `Fatal` is
    /// produced inside `sim_step`, so `UiBuild` sees it immediately rather
    /// than a frame later.
    pub fn set_fatal(&mut self, fatal: Option<SimError>) {
        self.world.resource_mut::<SessionMirror>().fatal = fatal;
    }

    /// Update only the bug-report-bundle mirror — set by `csp-app`'s
    /// Fatal handler once the bundle write resolves (path or failure).
    pub fn set_fatal_bundle(&mut self, bundle: Option<String>) {
        self.world.resource_mut::<SessionMirror>().fatal_bundle = bundle;
    }

    /// Stage the app's live settings into the panel's edit copy —
    /// called at startup, every time the window opens (the restage
    /// keeps discarded edits from haunting the next session), and
    /// whenever the file is (re)loaded, so the window always opens on
    /// the applied state.
    pub fn stage_settings(&mut self, settings: &settings::Settings) {
        self.world.resource_mut::<SettingsPanelRes>().0.settings = settings.clone();
    }

    /// Mirror the app's live (applied) keymap into [`SessionMirror`]
    /// — the source every hint bar and the controls card prints.
    /// Called at startup and on each settings Apply; the staged copy
    /// in [`SettingsPanelRes`] stays the edit buffer.
    pub fn set_live_keymap(&mut self, keys: &settings::Keymap) {
        self.world.resource_mut::<SessionMirror>().keys = keys.clone();
    }

    /// Record the glyph a physical `key-*` token produced on the
    /// host's keyboard layout — the hint bars' observed-label map
    /// ([`csp_ui::settings::KeyBinding::display_label`]). The input
    /// layer calls this on every key event; repeats are idempotent.
    pub fn set_key_label(&mut self, token: String, label: String) {
        self.world
            .resource_mut::<SessionMirror>()
            .key_labels
            .insert(token, label);
    }

    /// The panel's staged settings — read by `csp-app` when "Apply"
    /// rides the outbox; it becomes the live set and the file write.
    pub fn staged_settings(&self) -> settings::Settings {
        self.world.resource::<SettingsPanelRes>().0.settings.clone()
    }

    /// Toggle the M0 debug scaffold panel (the `ToggleDebug` binding)
    /// — dev internals (adapter name, tick/frame counters), off by
    /// default so the shipped view is a game screen, not a diagnostics
    /// dump.
    pub fn toggle_debug_panel(&mut self) {
        let mut r = self.world.resource_mut::<PanelFlags>();
        r.debug = !r.debug;
    }

    /// Toggle the controls-help card (the `ToggleHelp` binding, the
    /// top-bar "?" button).
    pub fn toggle_help_panel(&mut self) {
        let mut r = self.world.resource_mut::<PanelFlags>();
        r.help = !r.help;
    }

    /// Toggle the Program window (the `OpenProgram` binding, the
    /// top-bar "Program" button).
    pub fn toggle_program_panel(&mut self) {
        let mut r = self.world.resource_mut::<PanelFlags>();
        r.program = !r.program;
    }

    /// Whether the Program window is on screen — for tests and the
    /// tutorial's progress read.
    pub fn program_panel_open(&self) -> bool {
        self.world.resource::<PanelFlags>().program
    }

    /// Whether the controls-help card is on screen — for tests and
    /// the app's own wiring checks.
    pub fn help_panel_open(&self) -> bool {
        self.world.resource::<PanelFlags>().help
    }

    /// Toggle the settings window (the `OpenSettings` binding).
    pub fn toggle_settings_panel(&mut self) {
        let mut r = self.world.resource_mut::<SettingsPanelRes>();
        r.0.open = !r.0.open;
        // A closed window drops an armed rebind so the next key press
        // is not swallowed by a panel the user can no longer see.
        if !r.0.open {
            r.0.listening = None;
        }
    }

    /// Whether the settings window is open — for the app's
    /// restage-on-open and for tests.
    pub fn settings_panel_open(&self) -> bool {
        self.world.resource::<SettingsPanelRes>().0.open
    }

    /// The action awaiting a captured key, if the panel's "rebind" is
    /// armed — `csp-app` reads this *before* normal dispatch each key
    /// press. `None` while the window is closed even if `listening`
    /// was left armed (e.g. an X-close raced a frame) — a capture
    /// must never swallow keys for a panel the user cannot see.
    pub fn rebind_target(&self) -> Option<settings::KeyAction> {
        let s = &self.world.resource::<SettingsPanelRes>().0;
        if s.open { s.listening } else { None }
    }

    /// Disarm the rebind capture (Escape, or after a key lands).
    pub fn set_rebind_target(&mut self, action: Option<settings::KeyAction>) {
        self.world.resource_mut::<SettingsPanelRes>().0.listening = action;
    }

    /// Land a captured key into the staged keymap ([`settings::Keymap::bind`]
    /// handles eviction) and disarm the capture. `token` is the
    /// host-normalized binding text — `KeyBinding::parse` has already
    /// accepted it on the app side.
    pub fn bind_key(&mut self, action: settings::KeyAction, key: settings::KeyBinding) {
        let mut r = self.world.resource_mut::<SettingsPanelRes>();
        let evicted = r.0.settings.keys.bind(action, key.clone());
        r.0.listening = None;
        r.0.notice = Some(match evicted {
            Some(other) => format!("{key} bound to {} (was {})", action.label(), other.label()),
            None => format!("{key} bound to {}", action.label()),
        });
    }

    /// A one-line notice for the panel ("settings saved", a write
    /// failure) — the host's feedback channel.
    pub fn set_settings_notice(&mut self, notice: Option<String>) {
        self.world.resource_mut::<SettingsPanelRes>().0.notice = notice;
    }

    /// M10 tutorial: whether the first-flight checklist is on screen —
    /// the overlay's own Dismiss and the top bar's Tutorial button are
    /// the in-game path; this is the host's read for tests and a
    /// future "don't show again" persist.
    pub fn tutorial_active(&self) -> bool {
        self.world.resource::<tutorial::TutorialRes>().0.active
    }

    /// M10 tutorial: show/hide the checklist — the host-side plumbing
    /// (progress is preserved across a hide/show cycle).
    pub fn set_tutorial_active(&mut self, active: bool) {
        self.world.resource_mut::<tutorial::TutorialRes>().0.active = active;
    }

    /// M10 tutorial: the checklist's progress snapshot — read access
    /// for tests and status displays (mutations go through the overlay
    /// or `observe` inside the schedules).
    pub fn tutorial_state(&self) -> &csp_ui::tutorial::TutorialState {
        &self.world.resource::<tutorial::TutorialRes>().0
    }

    /// The `egui::Context` — `csp-app` borrows this clone to tessellate the
    /// stored `FullOutput` into paint triangles. The pass itself already ran
    /// inside `UiBuild`.
    pub fn egui_context(&self) -> egui::Context {
        self.world.resource::<EguiPass>().ctx.clone()
    }

    /// Take the completed egui pass for this frame — consumed once by the
    /// painter (platform output, shapes → tessellation, textures delta).
    pub fn take_ui_frame(&mut self) -> Option<egui::FullOutput> {
        self.world.resource_mut::<EguiPass>().output.take()
    }

    /// Drain UI-emitted requests for next frame's Input (§2.5's one-frame
    /// UiBuild → Input hand-off).
    pub fn take_ui_outbox(&mut self) -> UiOutbox {
        std::mem::take(&mut *self.world.resource_mut::<UiOutbox>())
    }

    /// Queue a map-view event (key/mouse translated by `csp-app`) —
    /// drained by the `Input` schedule's `apply_map_input` system next
    /// frame (§2.5 #1: camera input is Input-stage work).
    pub fn map_event(&mut self, event: map::MapKeyEvent) {
        self.world
            .resource_mut::<map::MapInputEvents>()
            .pending
            .push(event);
    }

    /// Per-frame viewport plumbing for the map camera/projection —
    /// physical pixels (the packet's NDC offsets are px-exact).
    pub fn set_map_viewport(&mut self, width_px: f64, height_px: f64) {
        self.world.resource_mut::<map::MapInput>().viewport_px = (width_px, height_px);
    }

    /// Flight vs map — read by `csp-app` to gate map mouse input.
    pub fn map_mode(&self) -> map::MapMode {
        self.world.resource::<map::MapInput>().mode
    }

    /// Take this frame's map render packet — `None` in flight mode or
    /// before the first solved camera. Consumed once by the painter,
    /// like [`Self::take_ui_frame`].
    pub fn take_map_packet(&mut self) -> Option<csp_render::map::MapPacket> {
        self.world.resource_mut::<map::MapFrame>().0.take()
    }

    /// The effective view mode — `Vab` while the editor is open, else
    /// the map's Flight/Map. Session state (§2.2-C).
    pub fn mode(&self) -> ViewMode {
        if self.world.resource::<vab::VabInput>().open {
            ViewMode::Vab
        } else if self.world.resource::<map::MapInput>().mode == map::MapMode::Map {
            ViewMode::Map
        } else {
            ViewMode::Flight
        }
    }

    /// Switch the view mode — the choreography lives here: entering
    /// the VAB closes the map display (its overlay would draw under
    /// the editor), and re-entering Map refocuses. Direct resource
    /// writes — same plumbing-mutate precedent as `set_map_viewport`.
    /// The refocus runs `MapInput::refocus` itself (P09-F1) — the
    /// hand-duplicated body-only version snapped a vessel focus to
    /// the 6e10 m system distance because `bodies.get(focus)` misses
    /// every vessel-half index.
    pub fn set_mode(&mut self, mode: ViewMode) {
        {
            let mut v = self.world.resource_mut::<vab::VabInput>();
            v.open = mode == ViewMode::Vab;
        }
        let want_map = mode == ViewMode::Map;
        let mut refocus = false;
        {
            let mut m = self.world.resource_mut::<map::MapInput>();
            if want_map != (m.mode == map::MapMode::Map) {
                m.mode = if want_map {
                    map::MapMode::Map
                } else {
                    map::MapMode::Flight
                };
                refocus = want_map;
            }
        }
        if refocus {
            // `refocus` needs the body table + the vessel snapshot
            // alongside the mutable input — `resource_scope` lifts
            // `MapInput` out so the two reads borrow cleanly.
            self.world
                .resource_scope(|world, mut m: Mut<map::MapInput>| {
                    m.refocus(
                        world.resource::<map::BodyTable>(),
                        world.resource::<map::MapSnapshot>(),
                    );
                });
        }
    }

    /// Queue a VAB event (camera/mouse translated by `csp-app`, plus
    /// pick readbacks and craft-file loads) — drained by the `Input`
    /// schedule's `apply_vab_input` next frame.
    pub fn vab_event(&mut self, event: vab::VabEvent) {
        self.world
            .resource_mut::<vab::VabInputEvents>()
            .pending
            .push(event);
    }

    /// Per-frame viewport plumbing for the VAB camera/projection —
    /// physical pixels.
    pub fn set_vab_viewport(&mut self, width_px: f64, height_px: f64) {
        self.world.resource_mut::<vab::VabInput>().viewport_px = (width_px, height_px);
    }

    /// Take this frame's VAB render packet — `None` outside the VAB.
    /// Consumed once by the painter, like [`Self::take_ui_frame`].
    pub fn take_vab_packet(&mut self) -> Option<csp_render::vab::VabPacket> {
        self.world.resource_mut::<vab::VabFrame>().0.take()
    }

    /// Queue a flight camera event — drained by the `Input`
    /// schedule's `apply_flight_input`.
    pub fn flight_event(&mut self, event: flight::FlightEvent) {
        self.world
            .resource_mut::<flight::FlightInputEvents>()
            .pending
            .push(event);
    }

    /// Per-frame viewport plumbing for the flight projection —
    /// physical pixels.
    pub fn set_flight_viewport(&mut self, width_px: f64, height_px: f64) {
        self.world.resource_mut::<flight::FlightInput>().viewport_px = (width_px, height_px);
    }

    /// Take this frame's flight render packet — `None` before the
    /// first extract. Consumed once by the painter.
    pub fn take_flight_packet(&mut self) -> Option<csp_render::flight::FlightPacket> {
        self.world.resource_mut::<flight::FlightFrame>().0.take()
    }

    /// Pass-1 dead-stick recovery: bring an on-rails vessel back into
    /// the physics bubble so the player regains throttle/attitude
    /// control. The target is the map's focused vessel when the map
    /// has one, else the newest on-rails vessel — the logged
    /// `ToBubble` converts it on the next drain; the flight-focus
    /// override pins the chase camera + HUD to it once it stages.
    /// Returns the vessel the commands were enqueued for.
    ///
    /// The pin and the `ToBubble` are only issued when the command can
    /// plausibly apply (`to_bubble_eligible` — the drain's own
    /// reject classes checked up front): a focused vessel that cannot
    /// convert (in the bubble already, off the dominant parent's
    /// conic, craft-less) raises the "not recoverable" flight notice
    /// instead of a pin-and-silent-reject, and no eligible target at
    /// all raises "no vessel to recover". The takeover also moves the
    /// vessel's §2.2.1 control lease to `Player` in the same drain — a
    /// bound `Script`/`Agent` holder keeps issuing control commands
    /// after `ToBubble` converts the residence (only `Ui` and the
    /// named holder pass `check_control_lease` under a non-`Player`
    /// lease), so the recovered vessel would answer two masters.
    pub fn recover_control(&mut self) -> Option<csp_sim::VesselId> {
        let focused = if self.mode() == ViewMode::Map {
            self.world.resource::<map::MapInput>().focus_vessel
        } else {
            None
        };
        let target = match focused {
            // An explicit map target that cannot convert is a refusal
            // to surface, not a reason to redirect at another vessel.
            Some(id) if self.to_bubble_eligible(id) => Some(id),
            Some(_) => {
                self.flight_notice("VESSEL NOT RECOVERABLE — NOT ON RAILS HERE".to_string());
                return None;
            }
            None => self
                .sim
                .vessels()
                .iter()
                .filter(|(id, _)| self.to_bubble_eligible(**id))
                .map(|(id, _)| *id)
                .max(),
        };
        let Some(target) = target else {
            self.flight_notice("NO VESSEL TO RECOVER".to_string());
            return None;
        };
        self.enqueue_command(
            SourceClass::Ui,
            PLAYER_SOURCE_ID,
            csp_sim::SimCommand::ToBubble { vessel: target },
        );
        // M9c §2.2.1: vacate any `Script`/`Agent` control lease — but
        // ONLY once the `ToBubble` actually converts. Issuing the
        // `TakeControl` optimistically here would preempt the live
        // lease even when the conversion rejects at drain (P04-F3):
        // the pin's `lease_pending` flag arms it, and `end_frame`
        // issues it on the `seen` edge the materialization proves.
        self.world
            .resource_mut::<flight::FlightFocusOverride>()
            .set(target);
        Some(target)
    }

    /// Would a `ToBubble` plausibly apply to `vessel`? The drain's own
    /// reject classes, checked pre-pin: the vessel exists, rides an
    /// on-rails conic around the dominant body, carries a craft, a
    /// physics environment exists to receive it — AND the two fallible
    /// conversions the apply performs actually succeed: the conic must
    /// evaluate at the current tick (`state_at`) and the stored craft
    /// must re-weld (P04-F2 — an unchecked reject here used to leave
    /// the pin armed forever). What this still cannot see is the
    /// TOCTOU half: an SOI event re-parenting the orbit inside the
    /// same drain. `View::sim_step`'s drain-reject release covers that.
    fn to_bubble_eligible(&self, vessel: csp_sim::VesselId) -> bool {
        let Some(v) = self.sim.vessels().get(&vessel) else {
            return false;
        };
        let Some(orbit) = v.orbit() else {
            return false;
        };
        let Some(craft) = v.craft.as_ref() else {
            return false;
        };
        if self.sim.environment().is_none() {
            return false;
        }
        if !self.sim.dominant_body().is_some_and(|d| orbit.parent == *d) {
            return false;
        }
        // The apply's fallible halves, mirrored exactly:
        // `self.orbits.state_at(orbit_state, self.clock)` and
        // `csp_phys::weld(&craft.graph, &craft.parts)` — both reject
        // Recoverable at drain, so both are checked up front. The weld
        // runs once per keypress, not per frame — cheap enough to be
        // honest instead of predictive.
        if csp_sim::orbit::OrbitModel::state_at(self.sim.orbits(), orbit, self.sim.clock()).is_err()
        {
            return false;
        }
        csp_phys::weld(&craft.graph, &craft.parts).is_ok()
    }

    /// Clear the recover-control focus pin (session navigation aid) —
    /// called from `end_frame` when the pinned vessel leaves the sim's
    /// arena and from `launch_craft` so a fresh launch reclaims the
    /// focus under the newest-spawn rule.
    pub fn clear_flight_focus_override(&mut self) {
        self.world
            .resource_mut::<flight::FlightFocusOverride>()
            .clear();
    }

    /// The focused vessel's `VesselId` — the throttle/attitude
    /// command target (`None` when the flight scene has no vessel).
    /// The focus is the most recently spawned staged vessel — a new
    /// launch takes the camera, HUD, and this command route with it.
    /// A surface-latched pad craft still counts: `SetThrottle`
    /// unlatches it.
    pub fn flight_focus(&self) -> Option<csp_sim::VesselId> {
        self.world
            .resource::<flight::FlightUiData>()
            .focus
            .map(csp_sim::VesselId)
    }

    /// M4c launch: turn the VAB working craft into a vessel berthed
    /// on the pad and switch to the flight view. The craft comes
    /// from the session's `to_part_graph` +
    /// `CraftAnalysis.effective` weld inputs (the same resolution the
    /// editor displays); an empty/missing session falls back to a
    /// stock booster so the gate is playable out of the box. The
    /// command rides the normal §7.6 log like every mutation.
    /// A craft that cannot resolve is refused *before* the mode
    /// switch — the player stays in the VAB with the reason on the
    /// session's notice row (a `VabEvent::Notice`) instead of landing
    /// in a flight view with no vessel. Every refusal also returns
    /// `Err` with the same text: callers with no VAB audience (the
    /// startup seed, a headless launch) must not mistake a queued
    /// notice nobody reads for a spawn. `Ok(())` means the launch
    /// command is enqueued and the mode is Flight — `LaunchCraft`
    /// for a catalog-resolved craft at an authored site (the atomic
    /// order-and-berth: the craft is `SurfaceLatched` the same drain,
    /// no build wait), `SpawnPhysicalVessel` for the site-less
    /// sandbox fallback.
    ///
    /// The spawn pose is the §2.6.4 surface record the pad implies —
    /// the dominant body's authored launch site at the *current* spin
    /// phase, upright, belly resting on the datum, with the surface
    /// co-rotation velocity `ω × r`: the craft lands as a parked
    /// vehicle, not an inertially suspended one (a spawn without the
    /// `ω × r` term reads as ~175 m/s of surface-relative slide on
    /// Terrin and sheds its stack on first contact).
    #[allow(clippy::too_many_lines)] // the pre-flight gate pass grew it past the limit
    pub fn launch_craft(&mut self) -> Result<(), String> {
        // The analysis cache is dirty-gated — the VAB's own
        // Presentation frames refresh it, but a launch can arrive
        // before the editor ever ran one (the demo seed, a headless
        // spawn) or between an edit and the next frame. `effective`
        // is bound positionally to `session.iter()`/`to_part_graph`
        // order, so a stale cache pairs the fresh graph with another
        // revision's resolved parts — an `EffectiveIndexOutOfRange`
        // reject at best, silently misassigned weld inputs at worst.
        // Recompute first: cheap when clean (`demo::seed_pad_craft`
        // documents the same requirement).
        let craft = self.with_vab_session(|v| {
            v.recompute();
            (
                v.session().clone(),
                v.catalog().cloned(),
                v.analysis().cloned(),
                v.analysis_error().map(str::to_string),
                stage_plan_of(v),
            )
        });
        let (session, catalog, analysis, analysis_error, stage_plan) = craft;
        // W2.6: launch is a §3.5 manufacture verb — the craft def
        // rides `LaunchCraft` with its aggregate BOM
        // (`EffectivePart::build_resources` summed; the apply
        // re-checks and debits the site's inventory, then berths the
        // craft in the same drain). A craft with
        // no catalog resolution (the stock fallback) carries no
        // authored BOM — it still pays funds.
        let craft_def: Result<(csp_sim::CraftDef, Vec<(csp_data::ResourceId, f64)>), String> =
            (|| {
                if session.is_empty() {
                    return Ok((stock_flight_craft(), Vec::new()));
                }
                // A non-empty session that failed to resolve cannot
                // launch — the cached analysis error names why; without
                // a catalog the session can hold parts (a loaded craft
                // file) yet resolve nothing.
                let analysis = analysis.ok_or_else(|| {
                    analysis_error.unwrap_or_else(|| {
                        "craft could not be resolved — is the part catalog loaded?".to_string()
                    })
                })?;
                // `to_part_graph` needs the catalog to stamp each
                // edge's `joint` record (the snapped attach point,
                // C14-F4); a resolved analysis implies it is loaded.
                let (graph, built) = session
                    .to_part_graph(
                        catalog
                            .as_ref()
                            .ok_or_else(|| "part catalog is not loaded".to_string())?,
                    )
                    .map_err(|e| e.to_string())?;
                let parts: Vec<csp_phys::WeldPart> = analysis
                    .effective
                    .iter()
                    .map(|e| e.weld_input.clone())
                    .collect();
                let part_costs: Vec<f64> =
                    analysis.effective.iter().map(|e| e.cost_funds).collect();
                // The order's bill of materials — the command log is
                // self-contained (the `part_costs` convention), so the
                // per-part resolved BOMs ride it verbatim; the apply
                // aggregates duplicates.
                let mut bom: std::collections::BTreeMap<csp_data::ResourceId, f64> =
                    std::collections::BTreeMap::new();
                for e in &analysis.effective {
                    for (r, u) in &e.build_resources {
                        *bom.entry(r.clone()).or_insert(0.0) += u;
                    }
                }
                let build_resources: Vec<(csp_data::ResourceId, f64)> = bom.into_iter().collect();
                // M9b: the production spawn path populates the
                // reliability table from the same resolved parts — the
                // §3.4 model rides every launched craft.
                let reliability: Vec<csp_phys::ReliabilitySpec> = analysis
                    .effective
                    .iter()
                    .map(csp_phys::ReliabilitySpec::from_effective)
                    .collect();
                // M7-B §3.3: part identity rides the craft — `built[i]` is
                // positionally bound to `effective_index` like `parts`, so
                // the spawn gate can resolve each def's `tech_requirement`.
                let part_ids: Vec<csp_data::PartId> =
                    built.iter().map(|b| b.part_id.clone()).collect();
                // The embodied BOM rides the craft too — recovery
                // credits it back into the site's inventory, so the
                // per-part rows travel parallel to `parts`/`part_costs`
                // (the apply re-checks they sum to the declared bill).
                let part_build_resources: Vec<Vec<(csp_data::ResourceId, f64)>> = analysis
                    .effective
                    .iter()
                    .map(|e| e.build_resources.clone())
                    .collect();
                Ok((
                    csp_sim::CraftDef {
                        hash_memo: Default::default(),
                        graph,
                        parts,
                        part_costs,
                        reliability,
                        part_ids,
                        part_build_resources,
                    },
                    build_resources,
                ))
            })();
        let (craft, build_resources) = match craft_def {
            Ok(c) => c,
            Err(reason) => return Err(self.launch_refused(reason)),
        };
        // M7-B pre-flight: refuse an un-researched part BEFORE the mode
        // switch — a craft file can carry parts the palette would never
        // offer, and the sim apply re-checks identically at drain.
        if self.sim.tech_tree().is_some() {
            for id in &craft.part_ids {
                if let Some(def) = self.sim.parts().get(id)
                    && !self.sim.tech().is_part_unlocked(def)
                {
                    return Err(self.launch_refused(format!(
                        "{id} is tech-locked — requires {}",
                        def.tech_requirement
                    )));
                }
            }
        }
        let Some(env) = self.sim.environment().cloned() else {
            return Err(self.launch_refused("no system is loaded"));
        };
        let Some(dominant) = self.sim.dominant_body().cloned() else {
            return Err(self.launch_refused("no system is loaded"));
        };
        // The pad: the dominant body's authored launch site
        // (`sites.ron`, `launch: true`; the data validator enforces at
        // most one per body) — falling back to the fixed-frame equator
        // origin (lat = lon = alt = 0) for a body with no launch site.
        // A non-launch site is a recovery site, never a pad.
        // `config()` is a derived read (§7.3), identical for every
        // consumer.
        let site = self
            .sim
            .config()
            .sites
            .into_values()
            .find(|s| s.body == dominant && s.launch);
        let (lat, lon, site_alt) = site.as_ref().map_or((0.0, 0.0, 0.0), |s| {
            (s.latitude_rad, s.longitude_rad, s.altitude_m)
        });
        // Rest the craft ON the pad: `pos` is the body origin, so the
        // spawn altitude is the site's plus the belly depth — the lowest
        // body-frame collider vertex along the stack's up axis (+Z, the
        // VAB's build direction). Welding here mirrors exactly what
        // `SpawnPhysicalVessel`'s apply does — and a failed weld is a
        // refusal like every check above: enqueueing the spawn anyway
        // would land the player in Flight with no vessel and only a
        // logged command reject to show for it. `min(0.0)` keeps an
        // origin-below-parts craft from driving the origin itself
        // under the datum. The same weld hands `com_local` to the
        // restore — `vel` is the CoM slot (§2.9), so the spawn's
        // co-rotation stamp is the velocity of `pos + rot·com_local`,
        // not of the origin. The welded body survives below as the
        // pre-flight occupancy check's candidate.
        let mut candidate = match csp_phys::weld(&craft.graph, &craft.parts) {
            Ok(b) => b,
            Err(e) => return Err(self.launch_refused(e)),
        };
        let z_min = csp_sim::manufacture::collider_belly(&candidate);
        let com_local = candidate.com_local;
        // The spawn kinematics are the §2.6.4 latch record's exact
        // restore: upright ENU pose at the site, position resolved at
        // the CURRENT tick's spin phase (the pad co-rotates — spawning
        // at the stale t=0 longitude would put the craft downrange of
        // the site), velocity the surface co-rotation `ω × r` of the
        // CoM — a pad-resting craft, not an inertially-dropped one.
        // `berth_pose` is the ONE upright-on-belly convention the
        // delivery probe and the berth share (C12-F1) — the candidate
        // below is posed exactly as the order's delivery will berth
        // it. `z_min − PAD_CLEARANCE_M` keeps the (currently zero)
        // authored hover folded into the shared arithmetic.
        let pose = site.as_ref().map_or_else(
            || csp_sim::SurfacePose {
                body: dominant.clone(),
                latitude_rad: lat,
                longitude_rad: lon,
                altitude_m: site_alt + PAD_CLEARANCE_M - z_min,
                orientation: csp_sim::bubble::surface_upright(lat, lon),
                afloat: false,
            },
            |s| csp_sim::manufacture::berth_pose(s, z_min - PAD_CLEARANCE_M),
        );
        let (pos, rot, vel, ang_vel) =
            csp_sim::bubble::surface_pose_restore(&env, &pose, com_local, self.sim.clock());
        // `launch_preflight` runs the order's refusals BEFORE the
        // enqueue + mode switch, so a doomed launch keeps the player
        // in the VAB with the reason on the notice row. The candidate
        // is posed exactly as the delivery would berth it — the pad
        // is a single berth for build orders the same way it was for
        // instant spawns.
        candidate.pos = pos;
        candidate.rot = rot.normalize();
        candidate.vel = vel;
        candidate.ang_vel = ang_vel;
        // The pad is a single berth — but a *parked* vessel occupying
        // it is the same build the player is iterating on: the VAB
        // session IS that craft's working copy. When a surface-latched
        // vessel claims the pad, recover it (funds/parts/science come
        // back) as part of this launch instead of dead-refusing the
        // very build the player is looking at. Only a vessel that
        // cannot be recovered (in flight, on rails) still refuses.
        let occupant = self.sim.spawn_site_occupant(&candidate).and_then(|id| {
            self.sim
                .vessels()
                .get(&id)
                .filter(|v| matches!(v.state, csp_sim::VesselState::SurfaceLatched(_)))
                .map(|_| id)
        });
        if let Some(reason) = self.launch_preflight(
            &craft,
            &candidate,
            site.as_ref(),
            &build_resources,
            occupant,
        ) {
            return Err(reason);
        }
        // PLAN:1180's revert point — the world as it stood before this
        // launch press. It MUST be taken here, before the launch's
        // commands enqueue: a snapshot after the `RecoverVessel` /
        // `LaunchCraft` enqueues refuses at the §7.4 boundary
        // ("commands pending at serialization boundary") and leaves a
        // parked-build launch with no revert point at all. The
        // delivery-boundary recapture in `sim_step` (`RevertCapture`)
        // replaces this point when the launch berths — a revert retries
        // the flying, never un-pays the launch. A §7.4 refusal here is
        // still recorded verbatim: the revert verb then names its own
        // refusal instead of silently missing.
        self.pre_launch = Some(self.sim.snapshot().map_err(|e| e.to_string()));
        // The parked build clears the pad before the new craft berths
        // — the recovery is a normal logged command that
        // credits its value back, then the launch proceeds.
        if let Some(id) = occupant {
            let recovery_site = site.as_ref().map(|s| s.id.clone());
            self.enqueue_command(
                SourceClass::Ui,
                PLAYER_SOURCE_ID,
                SimCommand::RecoverVessel {
                    vessel: id,
                    site: recovery_site,
                },
            );
            self.flight_notice(
                "PARKED BUILD RECOVERED — value credited, pad cleared for the new craft",
            );
        }
        // The staged craft's firing plan rides session state (the
        // spawned graph drops stage metadata); `fire_stage` drains it
        // one set per press. The plan binds to THIS launch's vessel —
        // captured before the enqueue so `min_id` bounds the binding
        // to ids minted after this point (the `LaunchCraft` apply
        // mints the `VesselId` inside the drain — `StagePlan`'s
        // `(min_id, fingerprint)` binding tolerates exactly this late
        // materialization).
        let plan = flight::StagePlan::for_launch(stage_plan, &self.sim, &craft);
        if let Some(site) = site.filter(|_| !craft.part_ids.is_empty()) {
            // §3.5's honest path: the LAUNCH click is `LaunchCraft` —
            // the build order's gates and its atomic BOM + funds
            // spend, but the craft berths `SurfaceLatched` in the
            // same drain (throttle unlatches it). No build wait:
            // `ManufactureCraft`'s reservation shape is the
            // colony/foreign-order path now, never a rocket launch.
            // A catalog-resolved craft is required: the apply's bill
            // check (C11-F4) refuses anonymous parts in a catalog
            // world, so the stock/debug fallback drops through to the
            // sandbox spawn like a site-less world.
            self.enqueue_command(
                SourceClass::Ui,
                PLAYER_SOURCE_ID,
                SimCommand::LaunchCraft {
                    site: site.id.clone(),
                    craft: craft.clone(),
                    build_resources,
                },
            );
            // Arm the delivery-boundary recapture: `sim_step` moves the
            // revert point from the pre-order world to the boundary
            // where THIS launch's berth lands — the paid world with
            // the craft parked — so a revert retries the flying instead
            // of refunding the launch (the site-less fallback below keeps
            // its pre-spawn point: it has no berthing apply). The arm
            // keys on the order's `(site, craft)` identity plus the
            // `next_build_id` watermark — the `BuildId` itself is minted
            // inside the drain and cannot be named here — and expires
            // if the command rejects at its drain.
            self.revert_capture = Some(RevertCapture::AwaitingDelivery {
                site: site.id.clone(),
                craft,
                // `next_build_id` is not observable — the `StagePlan`
                // `min_id` convention instead: ids are monotonic and
                // never reused, so `last pending id + 1` admits only
                // records this order or a later-minted one created.
                min_build_id: self
                    .sim
                    .builds()
                    .keys()
                    .next_back()
                    .map_or(0, |id| id.0 + 1),
                armed_tick: self.sim.clock().ticks(),
            });
            self.flight_notice("CRAFT ON THE PAD — THROTTLE TO LIFTOFF".to_string());
        } else {
            // No authored launch site on the dominant body — a
            // build order needs a `launch` site's inventory to pay
            // out of, so a site-less world keeps the instant spawn
            // (funds still debit at apply). Sandbox/test worlds only;
            // the shipped data set always authors `terrin-launch`.
            // The embodied-BOM table must NOT ride: this path never
            // debited a bill, and the apply refuses a non-empty table
            // precisely because crediting it at recovery would mint
            // stock. Strip it here — the sandbox spawn is free craft,
            // free of the recovery BOM too.
            let mut sandbox_craft = craft;
            sandbox_craft.part_build_resources.clear();
            self.enqueue_command(
                SourceClass::Ui,
                PLAYER_SOURCE_ID,
                SimCommand::SpawnPhysicalVessel {
                    craft: sandbox_craft,
                    pos,
                    rot,
                    vel,
                    ang_vel,
                },
            );
        }
        // Only a successfully enqueued launch leaves the VAB — every
        // refusal above returned before this point. A recover-control
        // pin from an earlier T press must not survive it: the
        // documented rule is newest-spawn-takes-focus, so the fresh
        // launch gets the camera, HUD, and command route.
        self.clear_flight_focus_override();
        self.world.resource_mut::<flight::StagePlans>().push(plan);
        self.set_mode(ViewMode::Flight);
        Ok(())
    }

    /// The §7.2 save payload for the live world — snapshot at the
    /// §7.4 boundary plus the recorded state hash; the caller adds
    /// `game_version`/`data_set_hash`/`label` and encodes. The
    /// `SaveBlocking` reason (pending queue, live contacts, live
    /// joints, session taint) propagates for the UI to name.
    pub fn save_game(&self) -> Result<save::Save, SimError> {
        let snapshot = self.sim.snapshot()?;
        Ok(save::Save {
            game_version: String::new(),
            label: None,
            data_set_hash: None,
            data_kind_hashes: None,
            state_hash: self.sim.state_hash().ok(),
            snapshot,
        })
    }

    /// Restore the world from a decoded save — full §7.3 config
    /// re-install, then reset every sim-keyed session mirror. Returns
    /// the post-restore warnings (missing config channels, which the
    /// caller renders as notices).
    pub fn load_game(&mut self, snapshot: WorldSnapshot) -> Result<Vec<String>, SimError> {
        rebuild_from_snapshot(self, snapshot)
    }

    /// Whether a revert point exists — the Game menu gates its
    /// Revert button on this.
    pub fn revert_available(&self) -> bool {
        matches!(&self.pre_launch, Some(Ok(_)))
    }

    /// The revert point's sim tick for the prompt; `None` when no
    /// captured snapshot exists (the `Err` arm's reason reads via
    /// [`Self::revert_to_launch`]).
    pub fn revert_tick(&self) -> Option<u64> {
        match &self.pre_launch {
            Some(Ok(snap)) => Some(snap.clock_ticks),
            _ => None,
        }
    }

    /// Why no revert point exists — the §7.4 capture refusal text,
    /// for the menu's disabled-button hover.
    pub fn revert_reason(&self) -> Option<&str> {
        match &self.pre_launch {
            Some(Err(r)) => Some(r.as_str()),
            _ => None,
        }
    }

    /// PLAN:1180 — revert-to-launch. Restores the pre-launch snapshot
    /// captured at `launch_craft` — the same pre-drawn θ records ride
    /// the restore (it is a retry of the flying, never a reroll; the
    /// UI says so at the prompt). `Err` when no revert point exists.
    pub fn revert_to_launch(&mut self) -> Result<Vec<String>, SimError> {
        let snapshot = match &self.pre_launch {
            Some(Ok(snap)) => snap.clone(),
            // The capture's refusal text is surfaced through
            // `revert_reason`, not smuggled in the static SimError.
            Some(Err(_)) => {
                return Err(SimError::Recoverable(
                    "revert point could not be captured at launch",
                ));
            }
            None => {
                return Err(SimError::Recoverable(
                    "no revert point — nothing launched yet",
                ));
            }
        };
        // `rebuild_from_snapshot` clears `pre_launch` (a load must
        // never resurrect another session's point) — this restore IS
        // that point's own world, so re-arm it: revert stays
        // repeatable, a second press returning to the same pad-bound
        // state rather than reporting "nothing launched yet" while the
        // delivered craft sits on the pad.
        let warnings = rebuild_from_snapshot(self, snapshot.clone())?;
        self.pre_launch = Some(Ok(snapshot));
        Ok(warnings)
    }

    /// A fresh world — new game. Native loads `data/` again; the §6.3
    /// demo rebuilds its fixed scenario. The whole `View` is replaced
    /// so no presentation state can leak across the reset.
    pub fn new_game(&mut self) -> Result<(), ViewError> {
        #[cfg(target_arch = "wasm32")]
        {
            *self = Self::new_demo()?;
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            *self = if self.demo {
                Self::new_demo()?
            } else {
                Self::new()?
            };
        }
        Ok(())
    }

    /// Stage the save-slot listing the Game menu renders — `csp-app`
    /// rescans its save dir and pushes the rows here.
    pub fn set_save_list(&mut self, saves: Vec<csp_ui::game::SaveSlotRow>) {
        self.world.resource_mut::<GameMenuRes>().saves = saves;
    }

    /// Arm the revert confirmation — the RevertToLaunch key path,
    /// which must show the same "not a reroll" disclosure the menu's
    /// Revert button does.
    pub fn arm_revert_confirm(&mut self) {
        self.world.resource_mut::<GameMenuRes>().ui.confirm_revert = true;
    }

    /// Fire the launched craft's next stage: pop the front action set
    /// off [`flight::StagePlan`] and issue its logged commands — a
    /// `DeployPart` per staged deployable (the KSP staged-chute rule:
    /// a parachute fires its stage to deploy) then one `JettisonPart`
    /// per drop-set node, the session's decoupler semantics ("severs
    /// all its child edges") mapped onto the sim's v1 command
    /// surface. Returns the nodes acted on (`0` = no stage left, or
    /// the press was refused before popping).
    ///
    /// The plan belongs to the vessel its launch spawned — NOT the
    /// flight focus, which a newer spawn or a breakup fragment can
    /// take over without retargeting this craft's stages (the
    /// jettisons must reach the launched graph's node ids, not
    /// whatever holds focus). [`flight::StagePlan::vessel_for`] binds
    /// that vessel once the spawn lands.
    ///
    /// `JettisonPart`/`DeployPart` are both `InBubble`-only in v1, so
    /// the pop happens only after the bound vessel checks
    /// bubble-resident: on a surface-latched pad craft or a rails
    /// vessel the commands would all reject while the stage was still
    /// eaten — a silent no-op that permanently spent the firing.
    /// Every outcome now lands a [`flight::FlightSfx`] cue (the HUD's
    /// visible feedback channel): the press either visibly does
    /// something or says why not. Honest limitation: a jettisoned
    /// part leaves the sim outright — no KSP-style debris fragment,
    /// and engines are globally armed so a firing separates but never
    /// ignites.
    #[allow(clippy::too_many_lines)] // the per-vessel plan table adds arms
    pub fn fire_stage(&mut self) -> usize {
        // Per-vessel plans (C13-F4): every launch pushed its own
        // `StagePlan` into `StagePlans`; a second launch no longer
        // destroys the first craft's unfired stages. The press picks
        // the plan bound to the focused vessel, else the newest plan
        // that still holds stages — identical to the single-slot
        // behaviour when only one launch is live.
        let focus = self.flight_focus();
        let Some((idx, vessel, ambiguous)) = self
            .world
            .resource_mut::<flight::StagePlans>()
            .select_for_fire(&self.sim, focus)
        else {
            self.flight_notice("SINGLE-STAGE CRAFT — NOTHING TO STAGE".to_string());
            return 0;
        };
        {
            let mut plans = self.world.resource_mut::<flight::StagePlans>();
            let plan = &mut plans.plans[idx];
            if plan.stages.is_empty() {
                // `total` keeps the cases honest — an expended
                // multi-stage plan and a single-stage craft are not
                // the same "nothing".
                let msg = if plan.total > 0 {
                    "ALL STAGES FIRED — NOTHING LEFT"
                } else {
                    "SINGLE-STAGE CRAFT — NOTHING TO STAGE"
                };
                self.flight_notice(msg.to_string());
                return 0;
            }
        }
        if ambiguous {
            // P05-F1: two same-craft mints ≥ min_id — the plan can
            // never name its vessel honestly, so a firing now would
            // hit a random one. Retire it like the gone-vessel branch
            // rather than leaving a loaded trap for the next press.
            {
                let mut plans = self.world.resource_mut::<flight::StagePlans>();
                let plan = &mut plans.plans[idx];
                plan.stages.clear();
                plan.vessel = None;
            }
            self.flight_notice(
                "STAGE PLAN LOST — DUPLICATE LAUNCHED CRAFT, CANNOT IDENTIFY VESSEL".to_string(),
            );
            return 0;
        }
        let Some(vessel) = vessel else {
            // The launch's spawn has not landed yet — or never did
            // (a spawn rejection leaves no vessel to bind).
            self.flight_notice("STAGING NOT READY — LAUNCHED CRAFT NOT SPAWNED".to_string());
            return 0;
        };
        match self.sim.vessels().get(&vessel).map(|v| &v.state) {
            Some(csp_sim::VesselState::InBubble(_)) => {}
            Some(csp_sim::VesselState::SurfaceLatched(_)) => {
                self.flight_notice(
                    "NOT STAGED — PARKED ON THE SURFACE (THROTTLE TO LIFTOFF)".to_string(),
                );
                return 0;
            }
            Some(csp_sim::VesselState::OnRails(_)) => {
                self.flight_notice("NOT STAGED — VESSEL IS ON RAILS".to_string());
                return 0;
            }
            None => {
                // The bound vessel left the arena — retire the plan
                // so the readout stops promising firings it cannot
                // deliver.
                {
                    let mut plans = self.world.resource_mut::<flight::StagePlans>();
                    let plan = &mut plans.plans[idx];
                    plan.stages.clear();
                    plan.vessel = None;
                }
                self.flight_notice("NOTHING TO STAGE — LAUNCHED CRAFT GONE".to_string());
                return 0;
            }
        }
        // `stages` is stored front-first firing order; `remove(0)`
        // takes the next firing (pop() would take the last). Deploys
        // issue BEFORE jettisons — a chute staged alongside the
        // decoupler that drops its own subtree must get its deploy
        // record while the node still exists (a `DeployPart` naming
        // a jettisoned node just rejects).
        let actions = self.world.resource_mut::<flight::StagePlans>().plans[idx]
            .stages
            .remove(0);
        let (n_deploys, n_jettisons) = (actions.deploys.len(), actions.jettisons.len());
        for node in actions.deploys {
            self.enqueue_command(
                SourceClass::Ui,
                PLAYER_SOURCE_ID,
                SimCommand::DeployPart { vessel, node },
            );
        }
        for node in actions.jettisons {
            self.enqueue_command(
                SourceClass::Ui,
                PLAYER_SOURCE_ID,
                SimCommand::JettisonPart { vessel, node },
            );
        }
        // The cue names what the firing actually did — a chute-only
        // stage reads "DEPLOYED", not "0 PART(S) JETTISONED". C19-F6:
        // when the fired plan belongs to a vessel that ISN'T the
        // focus, name it — jettisoning another craft's parts must
        // never read as if the focused vessel staged.
        let notice = match (n_jettisons, n_deploys) {
            (j, 0) => format!("STAGED — {j} PART(S) JETTISONED"),
            (0, d) => format!("STAGED — {d} PART(S) DEPLOYED"),
            (j, d) => format!("STAGED — {j} JETTISONED + {d} DEPLOYED"),
        };
        let notice = if focus != Some(vessel) {
            format!("{notice} · VESSEL {}", vessel.0)
        } else {
            notice
        };
        self.flight_notice(notice);
        n_jettisons + n_deploys
    }

    /// Push a player-visible flight notice — a cue on the HUD's §3.7
    /// [`flight::FlightSfx`] channel (the same stack that flashes sim
    /// transitions). The in-flight feedback path for player commands
    /// the view must refuse or confirm — staging a parked craft, an
    /// empty plan, a landed staging fire — so a key press either does
    /// something visible or says why it cannot. Session state only;
    /// nothing here reaches the sim.
    pub fn flight_notice(&mut self, text: impl Into<String>) {
        self.world
            .resource_mut::<flight::FlightSfx>()
            .cues
            .push_back(flight::SfxCue {
                text: text.into(),
                ttl: flight::SFX_TTL_FRAMES,
            });
    }

    /// W2.6/report-08 L3: fold newly appended notification-ring
    /// entries into the HUD cue stack — `PartFailed` and
    /// `IgnitionMisfire` records naming the FOCUSED vessel post one
    /// toast each, so a dormant failure or a failed start is visible
    /// in flight without opening the Program window. The cursor is
    /// `(last tick surfaced, entries surfaced at that tick)`: ring
    /// entries append in non-decreasing tick order, so the diff is
    /// exactly the tail past the cursor — eviction past
    /// `notification_cap` evicts older ticks, which can never
    /// resurface. Program-level entries (`PROGRAM_VESSEL`) and
    /// notices about OTHER vessels stay ring-only — the LOG carries
    /// them; a toast about a vessel the player isn't flying would
    /// be noise (the per-vessel ReliabilityRow mirror still shows
    /// wear/failures on focus change).
    fn drain_part_notices(&mut self) {
        let focus = self.flight_focus();
        let Some(vid) = focus else {
            // No focused vessel: still advance the cursor — the
            // ring's per-vessel entries would all be stale toasts on
            // the next focus anyway (the LOG is their surface).
            if let Some(n) = self.sim.notifications().back() {
                self.notice_cursor = Some((n.tick, u32::MAX));
            }
            return;
        };
        let (last_tick, mut seen_at_tick) = self.notice_cursor.unwrap_or((0, 0));
        let mut new_tail = Vec::new();
        for n in self.sim.notifications() {
            if n.tick < last_tick {
                continue;
            }
            if n.tick == last_tick && seen_at_tick > 0 {
                // `u32::MAX` (the no-focus sentinel) collapses to
                // "skip this tick's entries entirely".
                seen_at_tick = seen_at_tick.saturating_sub(1);
                continue;
            }
            new_tail.push(n);
        }
        let mut toasts = Vec::new();
        for n in &new_tail {
            if n.vessel != vid {
                continue;
            }
            let part = n
                .node
                .and_then(|nid| {
                    self.sim.vessels().get(&vid).and_then(|v| {
                        v.craft.as_ref().and_then(|c| {
                            c.graph.nodes.get(&nid).and_then(|node| {
                                c.part_ids
                                    .get(node.effective_index as usize)
                                    .and_then(|pid| self.sim.parts().get(pid))
                                    .map(|d| d.id.as_str().to_string())
                            })
                        })
                    })
                })
                .unwrap_or_else(|| format!("part {}", n.node.map_or(0, |n| n.0)));
            let text = match part_failure_text(&n.kind) {
                Some(tail) => format!("vessel {} — {part} {tail}", vid.0),
                None => continue,
            };
            toasts.push(text);
        }
        let cursor = new_tail.last().map(|last| {
            let at = self
                .sim
                .notifications()
                .iter()
                .filter(|n| n.tick == last.tick)
                .count() as u32;
            (last.tick, at)
        });
        for text in toasts {
            self.flight_notice(text);
        }
        if let Some(c) = cursor {
            self.notice_cursor = Some(c);
        }
    }

    /// Fold one step's `AdvanceReport::script_log` into the rolling
    /// mirror the AUTOMATION section renders (W2.5). `tick` is the
    /// drain-side clock — display-only. Session state; the sim's own
    /// `script_log` was already `take`n into the report.
    pub fn push_script_log(&mut self, tick: u64, lines: &[(csp_sim::ScriptId, String)]) {
        let mut mirror = self.world.resource_mut::<ScriptLogMirror>();
        for (id, line) in lines {
            mirror
                .lines
                .push_back((tick, format!("script {}: {line}", id.0)));
        }
        while mirror.lines.len() > 64 {
            mirror.lines.pop_front();
        }
    }

    /// The pre-flight half of the launch path's refusals —
    /// `apply_launch_craft`'s gates for a site'd launch (site row,
    /// BOM affordability, funds, cap — the same
    /// `check_manufacture_craft` set), `apply_spawn_physical`'s for
    /// the site-less fallback — checked BEFORE [`View::launch_craft`]
    /// commits so a doomed launch keeps the
    /// player in the VAB with the reason on the notice row instead
    /// of landing in an empty flight view with only a logged command
    /// reject to show for it. `candidate` is the welded body already
    /// posed at the pad. Returns the refusal text (already posted
    /// through [`Self::launch_refused`]) or `None` when the launch
    /// is clear. The drain re-runs every gate regardless — a second
    /// source can order in between — so this check only ever
    /// *refuses* early; it can never let a bad order through.
    fn launch_preflight(
        &mut self,
        craft: &csp_sim::CraftDef,
        candidate: &csp_phys::WeldedBody,
        site: Option<&csp_data::SiteDef>,
        build_resources: &[(csp_data::ResourceId, f64)],
        occupant: Option<csp_sim::VesselId>,
    ) -> Option<String> {
        // One order per pad at a time. `LaunchCraft` berths
        // instantly, but a colony/agent `ManufactureCraft` in flight
        // still reserves this berth — its delivery waits on the
        // `ConstructionRetry` chain for the pad to clear, and the
        // player should not stack a launch in front of it (a launch
        // draining first would berth and defer the paid build
        // indefinitely while the new craft sits). Scoped to the same
        // condition the order path takes below
        // (`site` + a catalog-resolved craft): a site-less/anonymous
        // spawn never mints a build, and a foreign order at ANOTHER
        // site doesn't claim this berth.
        if let Some(site) = site.filter(|_| !craft.part_ids.is_empty())
            && let Some(b) = self.sim.builds().values().find(|b| b.site == site.id)
        {
            let eta_s = b.complete_tick.saturating_sub(self.sim.clock().ticks()) as f64
                / csp_core::TICK_HZ as f64;
            return Some(self.launch_refused(format!(
                "a build is already in flight — delivery #{} lands in {}; wait for it or cancel it in FACILITIES",
                b.id.0,
                csp_ui::fmt_period(eta_s)
            )));
        }
        // §3.5's cap — vessels + in-flight builds (each build already
        // reserved its delivery slot).
        if self.sim.vessel_slots_in_use() >= self.sim.vessel_caps().max_vessels {
            return Some(self.launch_refused(format!(
                "vessel cap reached — {} slots in use; recover a vessel first",
                self.sim.vessel_slots_in_use()
            )));
        }
        // §3.5's manufacture debit is charged at the pad — an
        // unaffordable craft must not launch, never clamp (the
        // economy module's action rule).
        let cost_micros = csp_sim::economy::craft_cost_micros(craft);
        let funds_micros = self.sim.funds_micros();
        if cost_micros > funds_micros {
            return Some(self.launch_refused(format!(
                "insufficient funds — craft costs {:.0}, {:.0} on hand",
                cost_micros as f64 / csp_sim::economy::MICROS_PER_FUND,
                funds_micros as f64 / csp_sim::economy::MICROS_PER_FUND,
            )));
        }
        // W2.6: the BOM half of the build order — `apply_manufacture_
        // craft` debits every `build_resources` row out of the site's
        // inventory, so a short stockpile refuses up front with the
        // shortfall named, the funds gate's precedent.
        if let Some(site) = site {
            if !self.sim.economy().sites.contains_key(&site.id) {
                return Some(self.launch_refused(format!(
                    "site {} has no inventory row — nothing to build from",
                    site.id
                )));
            }
            for (r, need) in build_resources {
                let have = self
                    .sim
                    .economy()
                    .sites
                    .get(&site.id)
                    .and_then(|st| st.inventory.get(r))
                    .copied()
                    .unwrap_or(0.0);
                if have < *need {
                    return Some(self.launch_refused(format!(
                        "insufficient {r} at {site} — craft needs {need:.1}, {have:.1} stockpiled",
                        site = site.id
                    )));
                }
            }
        }
        // The pad is a single berth — a *parked* surface-latched
        // vessel occupying it is the same build under edit (the VAB
        // session is its working copy), so launching recovers it and
        // proceeds rather than dead-refusing the craft the player is
        // looking at. Only a vessel that can't be recovered still
        // refuses here (in flight or on rails — `occupant` is `Some`
        // only for a surface-latched one the caller will recover).
        if occupant.is_none() && self.sim.spawn_site_occupied(candidate) {
            return Some(
                self.launch_refused("launch site occupied — an active vessel blocks the pad"),
            );
        }
        // The drain-side contract, verbatim: a launch that will
        // actually issue a `LaunchCraft` (a catalog-resolved
        // craft to an authored launch site — the same condition
        // `launch_craft` picks the order path on) must clear EVERY
        // `apply_launch_craft` gate, not only the ones with
        // friendlier wording above — the tank/coherence tables, the
        // part and facility unlocks, the weld, and the catalog bill
        // envelope all reject at the drain too, and a drain-reject on
        // an already-empty arena strands the player in a dead flight
        // scene. `check_manufacture_craft` is both applies' own gate
        // list run read-only, so the button refuses with the sim's
        // real reason rather than a cue after the mode switch.
        if let Some(site) = site.filter(|_| !craft.part_ids.is_empty())
            && let Err(e) = self
                .sim
                .check_manufacture_craft(&site.id, craft, build_resources)
        {
            return Some(self.launch_refused(e.reason()));
        }
        None
    }

    /// The shared refusal tail of [`View::launch_craft`]: the reason
    /// lands on the VAB session's notice row for the player AND comes
    /// back as the `Err` text for callers with no notice audience.
    fn launch_refused(&mut self, reason: impl std::fmt::Display) -> String {
        let msg = format!("launch refused — {reason}");
        self.vab_event(vab::VabEvent::Notice(msg.clone()));
        msg
    }

    /// Run a closure against the working-craft session — the plumbing
    /// accessor for tests/tools (app code reaches the session only via
    /// `vab_event` and the UiBuild pass, §2.5).
    pub fn with_vab_session<R>(&mut self, f: impl FnOnce(&mut csp_ui::vab::VabSession) -> R) -> R {
        f(&mut self.world.resource_mut::<vab::VabSessionRes>().0)
    }

    /// Install the part catalog + resource registry (called by
    /// `View::from_source` after the `data/` tree loads — native reads
    /// the filesystem, the §6.3 demo reads the embedded set; an unset
    /// catalog leaves the VAB honest-but-empty). `data_set_hash` is
    /// the §3.1 aggregate of the set the catalog loaded from — the
    /// session stamps it onto every `BuiltPart` it builds (§7.7's
    /// per-part drift key); `None` leaves the zero sentinel.
    pub fn install_vab_catalog(
        &mut self,
        parts: csp_data::LoadedParts,
        resources: csp_data::ResourceFile,
        data_set_hash: Option<[u8; 32]>,
    ) {
        self.world
            .resource_mut::<vab::VabSessionRes>()
            .0
            .set_catalog(parts, resources, data_set_hash);
        self.stage_vab_tech();
    }

    /// Push the sim's live §3.3 progression state into the VAB
    /// session — the palette's unlock gate + the placement-time
    /// upgrade freeze read it. `None` when the world has no tech
    /// tree (an ungated editor, like a catalog-less one).
    fn stage_vab_tech(&mut self) {
        let tech = self
            .sim
            .tech_tree()
            .cloned()
            .map(|tree| csp_ui::vab::VabTech {
                tree,
                researched: self.sim.tech().researched.clone(),
                track_levels: self.sim.tech().track_levels.clone(),
                // §3.4's confidence input — the same table the
                // FACILITIES rows report from (`stage_program`); a
                // bench-bought family must move the editor's number
                // (C10-F3).
                qualifications: self.sim.tech().qualifications.clone(),
            });
        self.world
            .resource_mut::<vab::VabSessionRes>()
            .0
            .set_tech(tech);
    }

    /// The craft-file name list the save/load UI shows — plumbed by
    /// `csp-app` (its file store owns the bytes).
    pub fn set_craft_files(&mut self, files: Vec<String>) {
        self.world.resource_mut::<vab::CraftFileList>().0 = files;
    }

    /// Stage the progression mirror — economy, contracts, the tech
    /// tree, and the focused vessel's science/reliability rows — for
    /// the Program window. Runs in `end_frame` beside
    /// [`Self::stage_vab_tech`]: sim reads happen HERE (the staging
    /// phase), systems only ever see the mirror (§2.5).
    #[allow(clippy::too_many_lines)] // one staged row set = one pass
    fn stage_program(&mut self) {
        use csp_ui::program as up;
        let sim = &self.sim;
        let loc = self.world.resource::<LocStrings>().0.clone();
        let name_of = |key: &csp_data::LocKey, fallback: &str| {
            loc.get(key.as_str())
                .cloned()
                .unwrap_or_else(|| fallback.to_string())
        };
        let mut input = up::ProgramInput {
            funds: sim.funds_micros() as f64 / csp_sim::economy::MICROS_PER_FUND,
            science: sim.science_now(),
            banked: sim.science().banked,
            hire_cost: sim.economy_config().hire_cost_micros as f64
                / csp_sim::economy::MICROS_PER_FUND,
            // C08-F2 §8.3: the hire default is authored in
            // `data/balance.ron` (`crew_salary_per_s`) and reaches the
            // panel through the sim's economy config — the retired
            // view-side `HIRE_SALARY_PER_S` hardcode put one crew at
            // 5.18%/month of starting funds, over the §3.5 band.
            hire_salary_per_s: sim.economy_config().crew_salary_per_s,
            // C21-F8: the authored founding charge the colonies
            // section's button gates and labels on.
            colony_founding_cost: sim.economy_config().colony_founding_cost_micros as f64
                / csp_sim::economy::MICROS_PER_FUND,
            // C09-F6: the authored repair charge the colony card's
            // per-module repair buttons gate and label on.
            colony_repair_cost: sim.economy_config().colony_module_repair_cost_micros as f64
                / csp_sim::economy::MICROS_PER_FUND,
            ..Default::default()
        };
        // Contracts, research rows, and the notification log.
        stage_program_overview(sim, &mut input, &name_of);
        // The vessel the science/reliability rows describe: the
        // flight focus (InBubble/latched), else the map's focused
        // on-rails vessel.
        let staged = &self.world.resource::<flight::FlightSyncInput>().vessels;
        let pin = self.world.resource::<flight::FlightFocusOverride>();
        let mut vid = pin
            .resolve(staged)
            .map(|b| csp_sim::VesselId(b.id))
            .filter(|id| sim.vessels().contains_key(id));
        if vid.is_none() {
            vid = self
                .world
                .resource::<map::MapInput>()
                .focus_vessel
                .filter(|id| {
                    sim.vessels()
                        .get(id)
                        .is_some_and(|v| matches!(v.state, VesselState::OnRails(_)))
                });
        }
        let (mut ctx_biome, mut ctx_situation, mut ctx_label) = (None, None, None);
        let mut keys = Vec::new();
        let mut recovery_site = None;
        let mut researchers = Vec::new();
        let mut lab_slots = std::collections::BTreeMap::new();
        // C10-F2/C22-F5: the §3.4 flight display's sustained-g input.
        // The sim's per-tick `RegimeMetrics.g_load` never persists, so
        // `RegimeLoads` measures the same ‖Δv‖/dt over the staged
        // frame window — non-latched bodies only (a latched pose's
        // co-rotation velocity would read as phantom load, and its
        // records are dormant anyway).
        let vel_rows: Vec<(u64, csp_core::DVec3)> = self
            .world
            .resource::<flight::FlightSyncInput>()
            .vessels
            .iter()
            .filter(|b| !b.latched)
            .map(|b| (b.id, b.vel))
            .collect();
        self.world
            .resource_mut::<flight::RegimeLoads>()
            .stage(&vel_rows, sim.clock().ticks());
        if let Some(vid) = vid
            && let Some(v) = sim.vessels().get(&vid)
        {
            let g_load = self.world.resource::<flight::RegimeLoads>().g_for(vid.0);
            self.stage_program_vessel(
                v,
                &mut input,
                &mut ctx_biome,
                &mut ctx_situation,
                &mut ctx_label,
                &mut keys,
                &mut recovery_site,
                g_load,
                &name_of,
            );
        }
        // W2.5 — the script-console tail, folded back from the
        // report drain (`push_script_log` — the sim's own
        // `script_log` is consumed by `AdvanceReport`).
        input.script_log = self
            .world
            .resource::<ScriptLogMirror>()
            .lines
            .iter()
            .map(|(tick, text)| up::NoticeRow {
                tick: *tick,
                text: text.clone(),
            })
            .collect();
        // W2.0/W2.3 — the program arena: vessels, roster, colonies,
        // sites, labs, agenda, in-flight transfers.
        let now = sim.clock().ticks();
        stage_program_world(
            sim,
            &mut input,
            vid,
            now,
            &name_of,
            &mut researchers,
            &mut lab_slots,
        );
        *self.world.resource_mut::<ProgramMirror>() = ProgramMirror {
            input,
            vessel: vid,
            // W1.2: the context stages on the SITUATION, not the biome —
            // space situations (SpaceLow/SpaceHigh) have no biome, and
            // the sim accepts the empty `BiomeId` for them
            // (`research::apply_run_experiment` + the shipped canister's
            // situation mask). Zipping on the biome made `science_ctx`
            // `None` for every orbital context and dropped every
            // `RunExperiment` the panel correctly showed as runnable.
            science_ctx: ctx_situation
                .map(|s| (ctx_biome.unwrap_or_else(|| csp_data::BiomeId::from("")), s)),
            keys,
            recovery_site,
            now_ticks: now,
            researchers,
            lab_slots,
        };
    }

    /// The per-vessel half of [`Self::stage_program`] — experiments,
    /// pooled data, reliability rows, and the `(biome, situation)`
    /// context a `RunExperiment` command will carry.
    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_lines)] // one vessel = one staging pass
    #[allow(clippy::cognitive_complexity)] // one vessel = one staging pass
    fn stage_program_vessel(
        &self,
        v: &csp_sim::Vessel,
        input: &mut csp_ui::program::ProgramInput,
        ctx_biome: &mut Option<csp_data::BiomeId>,
        ctx_situation: &mut Option<csp_data::Situation>,
        ctx_label: &mut Option<(String, String, String, String)>,
        keys: &mut Vec<PooledKey>,
        recovery_site: &mut Option<csp_data::SiteId>,
        g_load: f64,
        name_of: &dyn Fn(&csp_data::LocKey, &str) -> String,
    ) {
        use csp_ui::program as up;
        let sim = &self.sim;
        input.latched = matches!(v.state, VesselState::SurfaceLatched(_));
        // W2.5 — the automation mirror: the engage gate needs the
        // residence (only an in-bubble vessel can take
        // `EngageAutopilot`), the console rows mirror the script
        // arena, and the autopilot label mirrors its arena entry.
        input.residence = match &v.state {
            VesselState::InBubble(_) => "bubble",
            VesselState::OnRails(_) => "rails",
            VesselState::SurfaceLatched(_) => "latched",
        }
        .to_string();
        input.autopilot = sim.autopilots().get(&v.id).map(|a| {
            let program = match &a.program {
                csp_sim::AutopilotProgram::Ascent {
                    target_apoapsis_m,
                    target_periapsis_m,
                } => format!(
                    "ascent → {:.0} km apo · {:.0} km pe",
                    target_apoapsis_m / 1000.0,
                    target_periapsis_m / 1000.0
                ),
            };
            if a.enabled {
                program
            } else {
                format!("{program} · paused")
            }
        });
        input.scripts = sim
            .scripts()
            .values()
            .filter(|s| s.vessel == v.id)
            .map(|s| up::ScriptRow {
                id: s.id.0,
                label: s
                    .source
                    .lines()
                    .map(str::trim)
                    .find(|l| !l.is_empty())
                    .unwrap_or("(empty source)")
                    .chars()
                    .take(48)
                    .collect(),
                enabled: s.enabled,
                strikes: s.strikes,
                disabled_reason: s.disabled_reason.clone(),
                wake_tick: s.wake_tick,
            })
            .collect();
        // §3.5's flight recovery, player-reachable at last: a latched
        // vessel can be recovered (site credit + part payout + crew +
        // pooled science) or terminated outright — the pad trap's
        // unblock path. The named site defaults to the latched
        // body's launch site (recovery_efficiency 1.0), else its
        // first site; no site at all still permits the
        // `abandon_recovery` remote-salvage leg (`site: None`).
        if let VesselState::SurfaceLatched(p) = &v.state {
            let site = sim
                .site_defs()
                .values()
                .find(|s| s.body == p.body && s.launch)
                .or_else(|| sim.site_defs().values().find(|s| s.body == p.body));
            input.recover_site = site.map(|s| name_of(&s.display_name, s.id.as_str()));
            *recovery_site = site.map(|s| s.id.clone());
        }
        // Situation + biome — the presentation-side context math the
        // research module doc assigns us.
        let (body, alt_m, lat_rad, lon_rad) = self.vessel_context(v);
        let env_atm_top = sim
            .environment()
            .map_or(0.0, |e| e.atmosphere.as_ref().map_or(0.0, |a| a.top_m));
        let situation = match &v.state {
            // Ground or water is the simulation's call (`splashed`):
            // a floater rides with its origin a few centimetres above
            // the datum and a hull on a seabed far below it, so
            // altitude cannot tell `Splashed` from `Landed`.
            VesselState::SurfaceLatched(_) => Some(if sim.splashed(v.id) {
                csp_data::Situation::Splashed
            } else {
                csp_data::Situation::Landed
            }),
            VesselState::InBubble(_) => {
                let contact = sim.ground_contact(v.id);
                if contact && alt_m <= 50.0 {
                    Some(csp_data::Situation::Landed)
                } else if sim.splashed(v.id) {
                    Some(csp_data::Situation::Splashed)
                } else if alt_m < env_atm_top {
                    Some(if alt_m < FLYING_SPLIT_M.min(env_atm_top) {
                        csp_data::Situation::FlyingLow
                    } else {
                        csp_data::Situation::FlyingHigh
                    })
                } else {
                    Some(if alt_m < SPACE_SPLIT_M {
                        csp_data::Situation::SpaceLow
                    } else {
                        csp_data::Situation::SpaceHigh
                    })
                }
            }
            VesselState::OnRails(_) => Some(if alt_m < SPACE_SPLIT_M {
                csp_data::Situation::SpaceLow
            } else {
                csp_data::Situation::SpaceHigh
            }),
        };
        let biome = situation.and_then(|s| {
            s.is_surface_side()
                .then(|| self.biome_at(body.as_deref(), lat_rad, lon_rad))
                .flatten()
        });
        *ctx_situation = situation;
        *ctx_biome = biome.clone();
        if let (Some(b), Some(s)) = (&body, &situation) {
            *ctx_label = Some((
                format!("vessel {}", v.id.0),
                s.name().to_string(),
                biome
                    .as_ref()
                    .map_or(String::new(), |b| b.as_str().to_string()),
                b.clone(),
            ));
            input.context = ctx_label.clone();
        }
        // Experiments — module specs over the live graph, gated the
        // same way `apply_run_experiment` gates them.
        if let Some(craft) = &v.craft {
            for (nid, node) in &craft.graph.nodes {
                let Some(part) = craft.parts.get(node.effective_index as usize) else {
                    continue;
                };
                let part_name = craft
                    .part_ids
                    .get(node.effective_index as usize)
                    .and_then(|pid| {
                        sim.parts()
                            .get(pid)
                            .map(|d| name_of(&d.display_name, pid.as_str()))
                    })
                    .unwrap_or_else(|| format!("part {}", nid.0));
                for spec in &part.experiments {
                    let spent = v.science.spent.contains(&(*nid, spec.kind.clone()));
                    let mut blocked = String::new();
                    if let Some(s) = situation {
                        if !spec.situations.contains(s) {
                            blocked = format!("cannot run while {}", s.name());
                        } else if body.as_deref().is_none_or(|b| {
                            sim.experiments()
                                .get(&spec.kind)
                                .and_then(|d| d.cap_for(&csp_data::BodyId::from(b), s))
                                .is_none()
                        }) {
                            blocked = "no science cap for this context".to_string();
                        }
                    } else {
                        blocked = "no context".to_string();
                    }
                    input.experiments.push(up::ExperimentRow {
                        node: nid.0,
                        part: part_name.clone(),
                        experiment: spec.kind.as_str().to_string(),
                        data_units: spec.data_units,
                        reusable: spec.reusable,
                        spent,
                        runnable: blocked.is_empty() && !(spent && !spec.reusable),
                        blocked: if spent && !spec.reusable {
                            "already collected — not reusable".to_string()
                        } else {
                            blocked
                        },
                    });
                }
            }
        }
        // Pooled data — one row per (node, key), merged by key like
        // `VesselScience::units_at`.
        // W1.5-(e): the transmit EC gate was undisclosed — stage the
        // vessel's ElectricCharge and each row's cost so the button
        // can gate and say why (same arithmetic `apply_transmit_data`
        // enforces: `units × transmit_ec_per_data_unit`).
        input.vessel_ec = Some(csp_sim::transfers::vessel_resource(
            v,
            &csp_data::ResourceId::from(csp_sim::research::ELECTRIC_CHARGE),
        ));
        let ec_per_unit = sim.science_config().transmit_ec_per_data_unit;
        let mut by_key: std::collections::BTreeMap<csp_sim::research::DataKey, f64> =
            std::collections::BTreeMap::new();
        for m in v.science.data.values() {
            for (k, u) in m {
                *by_key.entry(k.clone()).or_insert(0.0) += u;
            }
        }
        for (k, units) in by_key {
            if units <= 0.0 {
                continue;
            }
            let (ratio, prior) = (
                sim.experiments()
                    .get(&k.experiment)
                    .map_or(0.4, |d| d.transmit_ratio),
                sim.science().keys.get(&k).copied().unwrap_or_default(),
            );
            keys.push(PooledKey { key: k.clone() });
            input.pooled.push(up::DataRow {
                index: keys.len() - 1,
                label: format!(
                    "{}@{}/{}{}",
                    k.experiment.as_str(),
                    k.biome.as_str(),
                    k.body.as_str(),
                    k.situation.name()
                ),
                units,
                transmit_yield: units * ratio,
                transmit_ec_cost: units * ec_per_unit,
                recover_yield: units
                    * if prior.transmitted {
                        (1.0 - ratio).max(0.0)
                    } else {
                        1.0
                    },
                transmitted: prior.transmitted,
            });
        }
        // Reliability rows — one per node with a record.
        let now = sim.clock().ticks();
        // C10-F2/C22-F5 (§3.4 "displayed to the player in editor and
        // flight"): the live stress table needs the tick's
        // `RegimeMetrics`, which the sim never persists — so the row
        // rebuilds it from the same sources `bubble_part_tick` reads:
        // the body's throttle, the record's burn clock / ullage latch
        // / ignition count, the vessel's per-part temps, and the
        // `RegimeLoads` windowed g above. `specs_parallel`'s rule
        // applies verbatim: a non-covering spec table resolves no
        // spec, so positional reads are gated the same way.
        let throttle = v.body().map_or(0.0, |b| b.throttle);
        let specs_ok = v
            .craft
            .as_ref()
            .is_some_and(|c| c.reliability.len() == c.parts.len());
        for (nid, rec) in &v.reliability {
            let part_name = v
                .craft
                .as_ref()
                .and_then(|c| {
                    c.graph
                        .nodes
                        .get(nid)
                        .and_then(|n| c.part_ids.get(n.effective_index as usize))
                })
                .and_then(|pid| sim.parts().get(pid))
                .map(|d| name_of(&d.display_name, d.id.as_str()))
                .unwrap_or_else(|| format!("part {}", nid.0));
            let (stress, stress_product) = v
                .craft
                .as_ref()
                .filter(|_| specs_ok)
                .and_then(|c| {
                    let eff = c.graph.nodes.get(nid)?.effective_index as usize;
                    let spec = c.reliability.get(eff)?;
                    let temp_k = v.thermal.part_temps_k.get(nid).copied().unwrap_or(0.0);
                    let max_k = c
                        .parts
                        .get(eff)
                        .map_or(1.0, |p| p.thermal.max_temp_k.max(1.0));
                    let burn_s = if rec.burning {
                        now.saturating_sub(rec.burn_start_tick) as f64 / csp_core::TICK_HZ as f64
                    } else {
                        0.0
                    };
                    let m = csp_phys::RegimeMetrics {
                        throttle,
                        burn_s,
                        temp_fraction: temp_k / max_k,
                        g_load,
                        ullage_negative: rec.ullage_stressed,
                        restarts: rec.usage.ignitions.saturating_sub(1) as f64,
                    };
                    Some((
                        csp_phys::stress_breakdown(spec, &m)
                            .into_iter()
                            .map(|(r, a)| (r.clone(), a))
                            .collect::<Vec<_>>(),
                        csp_phys::stress_product(spec, &m),
                    ))
                })
                .unwrap_or_default();
            input.reliability.push(up::ReliabilityRow {
                node: nid.0,
                part: part_name,
                failures: rec.failures.iter().map(|m| format!("{m:?}")).collect(),
                misfire: rec.misfire,
                thrust_factor: rec.thrust_factor,
                wear: rec
                    .theta
                    .is_finite()
                    .then(|| (rec.hazard_at(now) / rec.theta).clamp(0.0, 1.0)),
                dormant: rec.dormant_class,
                stress,
                stress_product,
            });
        }
    }

    /// The focused vessel's `(body, altitude_m, lat_rad, lon_rad)` for
    /// situation/biome derivation — presentation-side science context.
    /// InBubble positions unrotate by the spin phase (the
    /// `surface_pose_capture` convention); rails vessels solve the
    /// osculating conic at the clock.
    fn vessel_context(&self, v: &csp_sim::Vessel) -> (Option<String>, f64, f64, f64) {
        let sim = &self.sim;
        match &v.state {
            VesselState::SurfaceLatched(p) => (
                Some(p.body.as_str().to_string()),
                p.altitude_m,
                p.latitude_rad,
                p.longitude_rad,
            ),
            VesselState::InBubble(b) => {
                let Some(env) = sim.environment() else {
                    return (None, 0.0, 0.0, 0.0);
                };
                let omega = env.body_spin.norm();
                let rel = if omega > 0.0 {
                    let phase = csp_core::DQuat::from_axis_angle(
                        env.body_spin / omega,
                        omega * sim.clock().seconds(),
                    );
                    phase.conjugate().rotate(b.pos - env.body_center)
                } else {
                    b.pos - env.body_center
                };
                let r = rel.norm();
                (
                    sim.dominant_body().map(|d| d.as_str().to_string()),
                    r - env.radius_m,
                    if r > 0.0 {
                        (rel.z / r).clamp(-1.0, 1.0).asin()
                    } else {
                        0.0
                    },
                    if rel.x != 0.0 || rel.y != 0.0 {
                        rel.y.atan2(rel.x)
                    } else {
                        0.0
                    },
                )
            }
            VesselState::OnRails(o) => {
                let alt = csp_sim::orbit::OrbitModel::state_at(sim.orbits(), o, sim.clock())
                    .map(|(pos, _)| {
                        let r = self
                            .world
                            .resource::<BodyRadii>()
                            .0
                            .get(o.parent.as_str())
                            .copied()
                            .unwrap_or(0.0);
                        pos.norm() - r
                    })
                    .unwrap_or(0.0);
                (Some(o.parent.as_str().to_string()), alt, 0.0, 0.0)
            }
        }
    }

    /// The `BiomeId` under `(lat, lon)` on `body` — equirect sample of
    /// the decoded §3.0.1 map; `None` when the body has no map.
    fn biome_at(
        &self,
        body: Option<&str>,
        lat_rad: f64,
        lon_rad: f64,
    ) -> Option<csp_data::BiomeId> {
        let maps = &self.world.resource::<BiomeMaps>().0;
        let l = maps.get(body?)?;
        if l.width == 0 || l.height == 0 {
            return None;
        }
        let lat_deg = lat_rad.to_degrees().clamp(-90.0, 90.0);
        let lon_deg = lon_rad.to_degrees().clamp(-180.0, 180.0);
        let row = (((90.0 - lat_deg) / 180.0) * l.height as f64) as usize;
        let col = (((lon_deg + 180.0) / 360.0) * l.width as f64) as usize;
        let idx = *l.indices.get(
            row.min(l.height as usize - 1) * l.width as usize + col.min(l.width as usize - 1),
        )?;
        l.legend.get(&idx).cloned()
    }
}

/// The native run's §2.7 save seed — wall-clock entropy folded with
/// the pid so two games started in the same nanos-quantum still
/// differ. Session entropy, not sim state: the seed itself is what
/// the sim serializes and hashes, so a saved/replayed run reproduces
/// its draws from `save_seed` regardless of how it was chosen.
#[cfg(not(target_arch = "wasm32"))]
fn native_save_seed() -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    nanos ^ (u64::from(std::process::id()) << 32) ^ 0x9E37_79B9_7F4A_7C15
}

/// The data/ directory root: `./data` when it exists (the app runs from
/// the repo root), else the repo layout relative to this crate's manifest
/// so tests and tools resolve the same files from any CWD. `pub` so
/// `csp-app`'s bug-report bundle can hash the same set (§2.14).
#[cfg(not(target_arch = "wasm32"))]
pub fn data_root() -> std::path::PathBuf {
    for candidate in [
        std::path::PathBuf::from("data"),
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data"),
    ] {
        if candidate.join("balance.ron").exists() {
            return candidate;
        }
    }
    std::path::PathBuf::from("data")
}

impl Default for View {
    /// The headless default — no `data/` IO; [`View::new`] is the startup
    /// path that loads it (and fails honestly on bad input).
    fn default() -> Self {
        Self::with_sim(SimWorld::new())
    }
}

/// The launch-time staging capture — the session's `stage_order`
/// firing sequence resolved into per-firing [`flight::StageActions`]
/// for [`flight::StagePlan`]. Each stage contributes:
///
/// - *jettisons* — every node in the subtree below each decoupler
///   assigned to the stage (the session's "severs all its child
///   edges" rule — the decoupler stays parent-side);
/// - *deploys* — every staged part whose def carries a
///   `ModuleDef::Parachute`, the KSP staged-chute rule (a parachute
///   fires its stage to deploy, not a right-click verb).
///
/// A part may land in both halves (a chute under a staged decoupler
/// deploys, then leaves the sim with the drop set — `fire_stage`
/// emits deploys first). Stages holding no decoupler and no
/// deployable, or a decoupler severing nothing, contribute no
/// firing.
fn stage_plan_of(vab: &csp_ui::vab::VabSession) -> Vec<flight::StageActions> {
    let session = vab.session();
    let mut plan = Vec::new();
    for stage in session.stage_order() {
        let mut drop_set = std::collections::BTreeSet::new();
        let mut deploy_set = std::collections::BTreeSet::new();
        for id in session.parts_in_stage(stage) {
            let Some(def) = vab.def_of(id) else { continue };
            if def
                .modules
                .iter()
                .any(|m| matches!(m, csp_data::ModuleDef::Decoupler))
            {
                let mut stack = session.children_ids(id);
                while let Some(n) = stack.pop() {
                    if drop_set.insert(n) {
                        stack.extend(session.children_ids(n));
                    }
                }
            }
            if def
                .modules
                .iter()
                .any(|m| matches!(m, csp_data::ModuleDef::Parachute { .. }))
            {
                deploy_set.insert(id);
            }
        }
        let actions = flight::StageActions {
            jettisons: drop_set.into_iter().collect(),
            deploys: deploy_set.into_iter().collect(),
        };
        if !actions.jettisons.is_empty() || !actions.deploys.is_empty() {
            plan.push(actions);
        }
    }
    plan
}

/// Install the progression surface's data-derived constants —
/// extracted from `from_source` under the line budget: the loc
/// strings (display names), the decoded biome maps the
/// `RunExperiment` biome context samples, and the body radii that
/// turn a rails conic radius into an altitude for situation bands.
fn install_program_resources(
    view: &mut View,
    bodies: &[csp_data::LoadedBody],
    defs: &[csp_data::BodyDef],
    loc: &csp_data::LocFile,
) {
    view.world.insert_resource(LocStrings(loc.strings.clone()));
    view.world.insert_resource(BiomeMaps(
        bodies
            .iter()
            .filter_map(|b| {
                let loaded = b.biomes.as_ref()?;
                let img = csp_data::decode_indexed_png(&loaded.png).ok()?;
                Some((
                    b.def.id.as_str().to_owned(),
                    BiomeLookup {
                        width: img.info.width,
                        height: img.info.height,
                        indices: img.indices,
                        legend: loaded
                            .legend
                            .entries
                            .iter()
                            .map(|e| (e.index, e.id.clone()))
                            .collect(),
                    },
                ))
            })
            .collect(),
    ));
    view.world.insert_resource(BodyRadii(
        defs.iter()
            .map(|d| (d.id.as_str().to_owned(), d.radius_m))
            .collect(),
    ));
}

/// The Program window's non-vessel half — contract board, tech-tree
/// and upgrade-track rows (same purchasability arithmetic the applies
/// enforce), and the tail of the reliability notification log.
/// Extracted from `stage_program` under the line budget.
#[allow(clippy::too_many_lines)] // one staging pass per mirror section
fn stage_program_overview(
    sim: &SimWorld,
    input: &mut csp_ui::program::ProgramInput,
    name_of: &dyn Fn(&csp_data::LocKey, &str) -> String,
) {
    use csp_ui::program as up;
    // W2.1/W2.3: the resupply gate is gone — the colony-logistics
    // verbs ship in this pass, so every generated offer is
    // completable and renders with its description + prereqs.
    let crew_lost = sim
        .economy()
        .crew
        .values()
        .filter(|m| m.status == csp_sim::CrewStatus::Lost)
        .count() as u32;
    for o in sim.contracts().offers.values() {
        let template = sim.contract_defs().get(&o.template);
        let prereqs: Vec<(bool, String)> = template
            .map(|t| {
                t.prerequisites
                    .iter()
                    .map(|p| (prereq_met(sim, p, crew_lost), prereq_text(p)))
                    .collect()
            })
            .unwrap_or_default();
        input.offers.push(up::OfferRow {
            index: o.index,
            label: format!(
                "#{} {}",
                o.index,
                template.map_or_else(
                    || o.template.as_str().to_string(),
                    |t| name_of(&t.display_name, o.template.as_str())
                )
            ),
            description: template.map_or_else(String::new, |t| name_of(&t.description, "")),
            detail: contract_params_summary(&o.params),
            reward: template.map_or_else(String::new, |t| {
                format!(
                    "+₣{:.0} sign · +₣{:.0} done · −₣{:.0} expiry",
                    t.rewards.advance_funds,
                    t.rewards.completion_funds,
                    t.rewards.failure_penalty_funds
                )
            }),
            deadline: template
                .and_then(|t| t.deadline_days)
                .map_or_else(String::new, |d| format!("deadline {d:.1} days")),
            accept_ok: template.is_some() && prereqs.iter().all(|(met, _)| *met),
            prereqs,
        });
    }
    let now = sim.clock().ticks();
    for c in sim.contracts().active.values() {
        let template = sim.contract_defs().get(&c.template);
        input.active.push(up::ActiveRow {
            label: format!(
                "#{} {}",
                c.id.0,
                template.map_or_else(
                    || c.template.as_str().to_string(),
                    |t| name_of(&t.display_name, c.template.as_str())
                )
            ),
            detail: format!(
                "{}{}{}",
                contract_params_summary(&c.params),
                template.map_or_else(String::new, |t| format!(
                    "  · +₣{:.0} on completion",
                    t.rewards.completion_funds
                )),
                c.deadline_tick.map_or_else(String::new, |t| {
                    let days = t.saturating_sub(now) as f64 / csp_core::TICK_HZ as f64 / 86_400.0;
                    format!("  · {days:.1} days left")
                })
            ),
            // W2.1: the completion predicate's live reading — the
            // per-kind progress line (delivery counts, band checks).
            progress: contract_progress(sim, c),
        });
    }
    let tech = sim.tech();
    input.tech = sim.tech_tree().map(|tree| {
        tree.nodes
            .iter()
            .map(|n| up::TechRow {
                id: n.id.as_str().to_string(),
                name: name_of(&n.display_name, n.id.as_str()),
                cost: n.science_cost,
                researched: tech.is_researched(&n.id),
                prereqs_met: n.requires.iter().all(|r| tech.is_researched(r)),
                affordable: input.banked >= n.science_cost,
            })
            .collect()
    });
    input.tracks = sim
        .tech_tree()
        .map(|tree| {
            tree.tracks
                .iter()
                .map(|t| {
                    let level = tech.track_level(&t.id);
                    let cap = tech.level_cap(tree, &t.id);
                    let maxed = usize::from(level) >= t.levels.len();
                    up::TrackRow {
                        id: t.id.as_str().to_string(),
                        name: name_of(&t.display_name, t.id.as_str()),
                        level,
                        cap,
                        levels: t.levels.len() as u8,
                        next_cost: (!maxed).then(|| t.levels[usize::from(level)].science_cost),
                        affordable: !maxed
                            && input.banked >= t.levels[usize::from(level)].science_cost,
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    // The LOG tail — the notification ring (vessel + program events)
    // merged with the funds ledger's tail by tick, so contract
    // lifecycle and money movement interleave correctly. Bounded at
    // the same 8 rows.
    let mut rows: Vec<(u64, String)> = sim
        .notifications()
        .iter()
        .map(|n| (n.tick, notice_text(n)))
        .collect();
    rows.extend(sim.economy().ledger.iter().map(|e| {
        (
            e.tick,
            format!(
                "funds {:+}₣ — {:?}",
                e.delta_micros as f64 / csp_sim::economy::MICROS_PER_FUND,
                e.reason
            ),
        )
    }));
    rows.sort_by_key(|(tick, _)| *tick);
    for (tick, text) in rows.into_iter().rev().take(8).rev() {
        input.notices.push(up::NoticeRow { tick, text });
    }
}

/// The free-seat count both staging passes write (C21-F9 — the
/// vessel-arena loop and the focused-vessel block each hand-kept
/// it): `vessel_seats` minus aboard crew minus inbound crew
/// transfers — an in-flight member holds a seat reservation, the
/// sim-side transfer gate counts it the same way.
fn free_seats(sim: &csp_sim::World, v: &csp_sim::Vessel, id: csp_sim::VesselId) -> u64 {
    let inbound = sim
        .transfers()
        .values()
        .filter(|t| {
            t.to == csp_sim::transfers::TransferEnd::Vessel(id)
                && matches!(t.cargo, csp_sim::transfers::TransferCargo::Crew { .. })
        })
        .count() as u64;
    csp_sim::transfers::vessel_seats(v)
        .saturating_sub(v.crew.len() as u64)
        .saturating_sub(inbound)
}

/// The `PartFailed`/`IgnitionMisfire` mode text (C21-F9 — the
/// focused-vessel toast and the `LOG` row each hand-kept the
/// mode/dormant formatting). Callers add their own vessel/part
/// naming — the toast resolves part display names, the LOG prints
/// node ids.
fn part_failure_text(kind: &csp_sim::reliability::NotificationKind) -> Option<String> {
    use csp_sim::reliability::NotificationKind as K;
    match kind {
        K::PartFailed { mode, dormant } => Some(format!(
            "PART FAILED {:?}{}",
            mode,
            if *dormant { " (dormant)" } else { "" }
        )),
        K::IgnitionMisfire => Some("IGNITION MISFIRE".to_string()),
        _ => None,
    }
}

/// One notification-log line — the `LOG` section's per-kind format.
/// [`csp_sim::reliability::PROGRAM_VESSEL`] entries (contracts,
/// banked science) print no vessel prefix.
fn notice_text(n: &csp_sim::reliability::Notification) -> String {
    use csp_sim::reliability::NotificationKind as K;
    let v = |id: csp_sim::VesselId| format!("vessel {} — ", id.0);
    // W2.6/report-08 L3: the failed part's node id joins the line —
    // "vessel 3 — PART FAILED Leak" couldn't say WHICH part.
    let p = |node: Option<csp_phys::PartNodeId>| {
        node.map_or_else(String::new, |n| format!("part {} ", n.0))
    };
    match n.kind {
        K::PartFailed { .. } | K::IgnitionMisfire => format!(
            "{}{}{}",
            v(n.vessel),
            p(n.node),
            part_failure_text(&n.kind).unwrap_or_default()
        ),
        K::OnRails => format!("{}ON RAILS", v(n.vessel)),
        K::ContractAccepted { contract } => format!("contract #{contract} — ACCEPTED"),
        K::ContractCompleted { contract } => format!("contract #{contract} — COMPLETED"),
        K::ContractFailed { contract } => {
            format!("contract #{contract} — FAILED (deadline expired)")
        }
        K::ScienceBanked { micros } => {
            format!(
                "science banked +{:.1}⚛",
                micros as f64 / csp_sim::economy::MICROS_PER_FUND
            )
        }
        // C08-F3 §3.5's insolvency consequences — both program-level
        // entries (the contract-lifecycle prefix convention: no
        // vessel name).
        K::StaffAttrition { crew } => {
            format!("crew #{crew} — RESIGNED (unpaid salaries)")
        }
        K::FacilityDormant => "FACILITIES DORMANT — program insolvent".to_string(),
        // C21-F2: the berth-blocked episode's two bookends — program-
        // level entries (no vessel prefix, the contract convention).
        K::BuildBerthBlocked { build } => {
            format!("build #{build} — BERTH BLOCKED (delivery deferred)")
        }
        K::BuildDelivered { build } => format!("build #{build} — DELIVERED"),
        // C09-F6: the impact hit's log line — program-level entry
        // (no vessel prefix, the contract convention).
        K::ColonyDamaged { colony, module } => {
            format!("colony {colony} — MODULE {module} DAMAGED (impact)")
        }
        // C09-F6: the repair verb's log line — the paid inverse of
        // the damage entry (same program-level convention).
        K::ColonyModuleRepaired { colony, module } => {
            format!("colony {colony} — module {module} repaired")
        }
    }
}

/// The kind-specific one-line summary of an offer's concrete params —
/// what the accept predicate will actually evaluate (W1.5-d: offers
/// were accept-blind, name-only).
fn contract_params_summary(p: &csp_data::ContractParams) -> String {
    use csp_data::ContractParams as P;
    match p {
        P::SatelliteOrbit {
            body,
            min_periapsis_alt_m,
            max_apoapsis_alt_m,
        } => format!(
            "orbit {body}: Pe ≥ {:.0} km, Ap ≤ {:.0} km",
            min_periapsis_alt_m / 1.0e3,
            max_apoapsis_alt_m / 1.0e3
        ),
        P::SurveyData {
            experiment,
            body,
            min_units,
        } => format!("return {min_units:.0} data of {experiment} from {body}"),
        P::PartTest { module, regime } => {
            let module = match module {
                csp_data::ModuleReq::Lab => "a lab".to_string(),
                csp_data::ModuleReq::DockPort => "a docking port".to_string(),
                csp_data::ModuleReq::CrewSeat => "a crew seat".to_string(),
                csp_data::ModuleReq::Experiment(e) => format!("experiment {e}"),
                csp_data::ModuleReq::Tank => "a tank".to_string(),
                csp_data::ModuleReq::ThrustSite => "an engine".to_string(),
            };
            let regime = match regime {
                csp_data::ContractRegime::Orbit { body, min_alt_m } => {
                    format!("orbit {body} above {:.0} km", min_alt_m / 1.0e3)
                }
                csp_data::ContractRegime::Landed { body } => format!("land on {body}"),
            };
            format!("take {module} to {regime}")
        }
        P::Rescue { body } => format!("recover a vessel latched on {body}"),
        P::ColonyResupply {
            body,
            resource,
            amount,
        } => format!("deliver {amount:.0} {resource} to colonies on {body}"),
        P::CrewTransport { site, count } => {
            format!("deliver {count} crew to the colony at {site}")
        }
        P::TouristFlight { regime } => {
            let regime = match regime {
                csp_data::ContractRegime::Orbit { body, min_alt_m } => {
                    format!("orbit {body} above {:.0} km", min_alt_m / 1.0e3)
                }
                csp_data::ContractRegime::Landed { body } => format!("land on {body}"),
            };
            format!("fly a tourist to {regime} and bring them home")
        }
    }
}

/// The §3.6 fact-predicate, staged — the sim's `prereq_met`
/// (`contracts.rs`) replicated read-side so an offer renders its
/// unmet gates instead of refusing at accept. `crew_lost` is the
/// roster's `Lost` count (the world counts it the same way).
fn prereq_met(sim: &SimWorld, p: &csp_data::ContractPrereq, crew_lost: u32) -> bool {
    use csp_data::ContractPrereq as Pr;
    let facts = &sim.contracts().facts;
    match p {
        Pr::TechResearched(n) => sim.tech().is_researched(n),
        Pr::ContractsCompleted { kind, count } => {
            facts.completed.get(kind).copied().unwrap_or(0) >= *count
        }
        Pr::BodyVisited(b) => facts.bodies_visited.contains(b),
        Pr::CrewLostAtMost(n) => crew_lost <= *n,
    }
}

/// One-line prereq text for the offer row's ✓/✗ list.
fn prereq_text(p: &csp_data::ContractPrereq) -> String {
    use csp_data::ContractPrereq as Pr;
    match p {
        Pr::TechResearched(n) => format!("research {}", n.as_str()),
        Pr::ContractsCompleted { kind, count } => {
            format!("{count} {kind:?} contract(s) completed")
        }
        Pr::BodyVisited(b) => format!("visit {}", b.as_str()),
        Pr::CrewLostAtMost(n) => format!("at most {n} crew lost"),
    }
}

/// A `ColonyModuleDef` → its one-word row label — what the module IS
/// for the colony card's module line.
fn colony_module_label(d: &csp_data::ColonyModuleDef) -> String {
    use csp_data::ColonyModuleDef as M;
    match d {
        M::Solar { area_m2, .. } => format!("solar {area_m2:.0}m²"),
        M::Generator {
            fuel_rate_per_s,
            rate_per_s,
            ..
        } => {
            if *fuel_rate_per_s > 0.0 {
                format!("generator {rate_per_s:.1}ec/s")
            } else {
                format!("rtg {rate_per_s:.1}ec/s")
            }
        }
        M::Drill {
            resource,
            base_rate_per_s,
            ..
        } => format!("drill {resource} {base_rate_per_s:.2}/s"),
        M::Converter { recipe, .. } => format!("converter {}", recipe.as_str()),
        M::Store { resource, capacity } => format!("store {resource} {capacity:.0}"),
        M::Battery { capacity } => format!("battery {capacity:.0}"),
        M::Radiator { area_m2, .. } => format!("radiator {area_m2:.0}m²"),
        M::LifeSupport { .. } => "life support".to_string(),
    }
}

/// The active contract's live progress line (W2.1) — the completion
/// predicate's measurable half, per kind. Mirrors the sim's
/// `objective_met`/`survey_returned_units` reads over the same state.
/// Takes the whole record — `TouristFlight`'s second leg reads the
/// progress state the params don't carry.
#[allow(clippy::too_many_lines)] // one arm per contract kind
fn contract_progress(sim: &SimWorld, contract: &csp_sim::ActiveContract) -> String {
    use csp_data::ContractParams as P;
    let params = &contract.params;
    match params {
        P::SatelliteOrbit { body, .. } => {
            // The predicate's orbit scan: any non-debris rails vessel
            // inside the band (completes on the next orbit stamp).
            let radius = sim.orbits().body_radius(body).unwrap_or(0.0);
            if let P::SatelliteOrbit {
                min_periapsis_alt_m,
                max_apoapsis_alt_m,
                ..
            } = params
            {
                let held = sim.vessels().values().any(|v| {
                    if v.debris_since.is_some() {
                        return false;
                    }
                    let Some(o) = v.orbit() else { return false };
                    let (a, e) = (o.elements.semi_major_axis_m, o.elements.eccentricity);
                    o.parent == *body
                        && e < 1.0
                        && a * (1.0 - e) - radius >= *min_periapsis_alt_m
                        && a * (1.0 + e) - radius <= *max_apoapsis_alt_m
                });
                if held {
                    "a vessel holds the band".to_string()
                } else {
                    format!("awaiting orbit at {}", body.as_str())
                }
            } else {
                String::new()
            }
        }
        P::SurveyData {
            experiment,
            body,
            min_units,
        } => {
            // `survey_returned_units`'s reading, replicated: collected
            // minus pooled/escrowed, only over keys with a return.
            let cfg = sim.science_config();
            let clock = sim.clock().ticks();
            let returned: f64 = sim
                .science()
                .keys
                .iter()
                .filter(|(k, s)| {
                    k.experiment == *experiment && k.body == *body && (s.transmitted || s.closed)
                })
                .map(|(k, s)| {
                    let pooled: f64 = sim.vessels().values().map(|v| v.science.units_at(k)).sum();
                    let escrowed: f64 = sim
                        .science()
                        .projects
                        .values()
                        .filter(|p| p.key == *k)
                        .map(|p| {
                            csp_sim::research::data_at(
                                p.data_start,
                                p.rate_science_per_s,
                                p.throughput_data_per_s,
                                cfg.science_per_data,
                                clock.saturating_sub(p.start_tick) as f64
                                    / csp_core::TICK_HZ as f64,
                            )
                        })
                        .sum();
                    (s.collected - pooled - escrowed).max(0.0)
                })
                .sum();
            format!("returned {returned:.1}/{min_units:.0} data")
        }
        P::PartTest { regime, .. } => {
            let sat = match regime {
                csp_data::ContractRegime::Orbit { body, min_alt_m } => {
                    let radius = sim.orbits().body_radius(body).unwrap_or(0.0);
                    sim.vessels().values().any(|v| {
                        let Some(o) = v.orbit() else { return false };
                        o.parent == *body
                            && o.elements.eccentricity < 1.0
                            && o.elements.semi_major_axis_m * (1.0 - o.elements.eccentricity)
                                - radius
                                >= *min_alt_m
                    })
                }
                csp_data::ContractRegime::Landed { body } => sim
                    .vessels()
                    .values()
                    .any(|v| matches!(&v.state, VesselState::SurfaceLatched(p) if p.body == *body)),
            };
            if sat {
                "a vessel satisfies the regime".to_string()
            } else {
                "awaiting the test regime".to_string()
            }
        }
        P::Rescue { body } => {
            let parked = sim
                .vessels()
                .values()
                .filter(|v| {
                    matches!(&v.state, VesselState::SurfaceLatched(p) if p.body == *body)
                        && v.debris_since.is_none()
                })
                .count();
            format!("{parked} vessel(s) parked on {}", body.as_str())
        }
        P::ColonyResupply {
            body,
            resource,
            amount,
        } => {
            // The predicate's cumulative read: deliveries into every
            // colony on the body (the arena ids carry the binding).
            let delivered: f64 = sim
                .colonies()
                .values()
                .filter(|c| c.body == *body)
                .map(|c| {
                    sim.contracts()
                        .facts
                        .colony_deliveries
                        .get(&(c.id.0, resource.clone()))
                        .copied()
                        .unwrap_or(0.0)
                })
                .sum();
            format!("delivered {delivered:.1}/{amount:.0} {resource}")
        }
        P::CrewTransport { site, count } => {
            // The predicate's level read: `Active` roster members
            // posted at colonies occupying the site.
            let posted = sim
                .economy()
                .crew
                .values()
                .filter(|m| {
                    m.status == csp_sim::CrewStatus::Active
                        && matches!(&m.location, csp_sim::CrewLocation::Colony(c)
                            if sim.colonies().get(c).is_some_and(|co| co.site == *site))
                })
                .count();
            format!("posted {posted}/{count} crew at {site}")
        }
        P::TouristFlight { .. } => {
            // Leg one's witness count + leg two's return read —
            // the sim's `stamp_tourist_flown`/`objective_met` pair.
            let Some(csp_sim::ContractProgress::TouristFlight { flown }) = &contract.progress
            else {
                return "awaiting the flight".to_string();
            };
            let home = flown
                .iter()
                .filter(|id| {
                    sim.economy().crew.get(id).is_some_and(|m| {
                        m.kind == csp_sim::CrewKind::Tourist
                            && m.status == csp_sim::CrewStatus::Active
                            && m.location == csp_sim::CrewLocation::Unassigned
                    })
                })
                .count();
            if home > 0 {
                "a tourist is home".to_string()
            } else {
                format!("{} tourist(s) flown — awaiting their return", flown.len())
            }
        }
    }
}

/// W2.0/W2.3's staging pass — the program arena beyond the focused
/// vessel: every vessel's residence/crew/link state, the crew roster,
/// the colony table with join/transfer flags, the site stockpiles,
/// live research projects, pending alarms, and in-flight transfers.
/// Sim reads happen HERE (the staging phase); the panel sees only
/// the mirror (§2.5).
/// W2.6 — stage the FACILITIES rows: in-flight `builds`, the
/// test-stand economics, and the per-family qualification table.
/// The stand lives at the dominant body's launch site — the same
/// site `launch_craft` resolves and `apply_bench_test` gates on.
fn stage_facilities(
    sim: &SimWorld,
    input: &mut csp_ui::program::ProgramInput,
    now: u64,
    tick_s: f64,
    name_of: &dyn Fn(&csp_data::LocKey, &str) -> String,
) {
    use csp_ui::program as up;
    // W2.6 — the FACILITIES surface: in-flight build orders, the
    // stand's economics, and per-family qualification rows. The
    // stand lives at the dominant body's launch site — the same
    // site `launch_craft` resolves and `apply_bench_test` gates on.
    for b in sim.builds().values() {
        input.builds.push(up::BuildRow {
            id: b.id.0,
            site: sim.site_defs().get(&b.site).map_or_else(
                || b.site.as_str().to_string(),
                |s| name_of(&s.display_name, s.id.as_str()),
            ),
            label: format!("{}-part craft", b.craft.parts.len()),
            eta_s: b.complete_tick.saturating_sub(now) as f64 / tick_s,
        });
    }
    let rcfg = sim.reliability_config();
    input.bench_funds_per_unit = rcfg.bench_funds_per_unit;
    input.bench_resource = rcfg.bench_resource.as_str().to_string();
    input.bench_resource_per_unit = rcfg.bench_resource_per_unit;
    input.bench_seconds_per_unit = rcfg.bench_seconds_per_unit;
    let stand = sim
        .dominant_body()
        .and_then(|d| sim.site_defs().values().find(|s| s.body == *d && s.launch));
    input.bench_site = stand.map(|s| s.id.as_str().to_string());
    input.bench_site_name = stand
        .map(|s| name_of(&s.display_name, s.id.as_str()))
        .unwrap_or_default();
    input.bench_stock = stand
        .and_then(|s| sim.economy().sites.get(&s.id))
        .and_then(|st| st.inventory.get(&rcfg.bench_resource))
        .copied()
        .unwrap_or(0.0);
    // The family table: catalog ∪ the qualification arena (a family
    // with no live record still deserves a row — the first bench
    // run is what the stand exists for). The hazard reading
    // evaluates the FIRST catalog part's authored curve — per-part
    // curves differ, so the row names its basis honestly.
    {
        let quals = &sim.tech().qualifications;
        let mut fams: std::collections::BTreeMap<String, (f64, u32)> =
            std::collections::BTreeMap::new();
        for f in sim.parts().values().map(|d| d.family.as_str().to_string()) {
            fams.entry(f).or_insert((0.0, 0));
        }
        for (f, q) in quals {
            fams.insert(
                f.as_str().to_string(),
                (q.flight_units + q.bench_units, q.observed_failures),
            );
        }
        for (f, (units, failures)) in fams {
            // The representative part: deterministic pick (the
            // catalog's BTree order) so the row's basis never flickers.
            let rep = sim
                .parts()
                .values()
                .find(|d| d.family.as_str() == f.as_str());
            let curve = rep.map(|d| &d.reliability.qualification_curve);
            let hazard_factor = curve.map_or(1.0, |c| csp_phys::qual_factor(c, units));
            let next_knot = curve
                .and_then(|c| c.knots.iter().find(|k| k.qualification_units > units))
                .map_or_else(String::new, |k| {
                    format!(
                        "→ ×{:.2} @ {:.0} u{}",
                        k.hazard_multiplier,
                        k.qualification_units,
                        rep.map(|d| format!(" on {}", d.id.as_str()))
                            .unwrap_or_default()
                    )
                });
            let bench_eta_s = sim
                .bench_runs()
                .values()
                .find(|b| b.family.as_str() == f.as_str())
                .map(|b| b.complete_tick.saturating_sub(now) as f64 / tick_s);
            input.families.push(up::FamilyRow {
                id: f.clone(),
                label: f,
                units,
                hazard_factor,
                next_knot,
                failures,
                bench_eta_s,
            });
        }
    }
}

#[allow(clippy::too_many_lines)] // one mirror = one staging pass
fn stage_program_world(
    sim: &SimWorld,
    input: &mut csp_ui::program::ProgramInput,
    focus: Option<csp_sim::VesselId>,
    now: u64,
    name_of: &dyn Fn(&csp_data::LocKey, &str) -> String,
    researchers: &mut Vec<(csp_sim::CrewId, u8)>,
    lab_slots: &mut std::collections::BTreeMap<u64, u32>,
) {
    use csp_ui::program as up;
    let tick_s = csp_core::TICK_HZ as f64;
    // The crew roster — the economy book's named individuals.
    for (id, m) in &sim.economy().crew {
        input.roster.push(up::CrewRow {
            id: id.0,
            name: m.name.clone(),
            kind: format!("{:?}", m.kind).to_lowercase(),
            skill: m.skill,
            active: m.status == csp_sim::CrewStatus::Active,
            status: format!("{:?}", m.status).to_lowercase(),
            salary_per_s: m.salary_per_s,
            location: match &m.location {
                csp_sim::CrewLocation::Unassigned => up::CrewWhere::Unassigned,
                csp_sim::CrewLocation::Vessel(v) => up::CrewWhere::Vessel(v.0),
                csp_sim::CrewLocation::Colony(c) => up::CrewWhere::Colony(c.0),
                csp_sim::CrewLocation::Facility(f) => {
                    up::CrewWhere::Facility(f.as_str().to_string())
                }
            },
        });
    }
    // The vessel arena — every vessel, residence + body + seats.
    for (id, v) in sim.vessels() {
        let (residence, body) = match &v.state {
            VesselState::InBubble(_) => (
                "in flight",
                sim.dominant_body()
                    .map(|b| b.as_str().to_string())
                    .unwrap_or_default(),
            ),
            VesselState::OnRails(o) => ("on rails", o.parent.as_str().to_string()),
            VesselState::SurfaceLatched(p) => ("landed", p.body.as_str().to_string()),
        };
        input.vessels.push(up::VesselRow {
            id: id.0,
            residence: residence.to_string(),
            body,
            debris: v.debris_since.is_some(),
            focused: focus == Some(*id),
            crew: v.crew.iter().map(|c| c.id.0).collect(),
            free_seats: free_seats(sim, v, *id),
            joined_colony: v.joined_colony.map(|c| c.0),
            linked_to_focus: focus.is_some_and(|f| sim.vessels_linked(f, *id)),
        });
    }
    // The colony arena — modules, epoch-anchored stores, the state
    // machine slot, and the focused vessel's join flags.
    for (id, c) in sim.colonies() {
        let live = matches!(
            c.state,
            csp_sim::ColonyState::Nominal | csp_sim::ColonyState::Degraded { .. }
        );
        input.colonies.push(up::ColonyRow {
            id: id.0,
            name: sim.site_defs().get(&c.site).map_or_else(
                || c.site.as_str().to_string(),
                |s| name_of(&s.display_name, s.id.as_str()),
            ),
            body: c.body.as_str().to_string(),
            site: c.site.as_str().to_string(),
            state: match &c.state {
                csp_sim::ColonyState::Nominal => "nominal".to_string(),
                csp_sim::ColonyState::Degraded { reason } => {
                    format!("degraded: {reason:?}").to_lowercase()
                }
                csp_sim::ColonyState::Dormant => "dormant".to_string(),
                csp_sim::ColonyState::Abandoned => "abandoned".to_string(),
            },
            live,
            daylight: match c.daylight {
                csp_sim::Daylight::Day => "day",
                csp_sim::Daylight::Night => "night",
            }
            .to_string(),
            crew: c.crew.iter().map(|c| c.0).collect(),
            // C09-F4: the panel's posting buttons mirror the sim's
            // `colony_accepts_crew` — only a colony with an undamaged
            // `LifeSupport` module can take crew while life support
            // is enabled.
            has_ls: !sim.life_support_enabled()
                || c.modules.iter().any(|m| {
                    !m.damaged && matches!(m.def, csp_data::ColonyModuleDef::LifeSupport { .. })
                }),
            modules: c
                .modules
                .iter()
                .zip(
                    c.module_fractions
                        .iter()
                        .copied()
                        .chain(std::iter::repeat(0.0)),
                )
                .map(|(m, f)| up::ColonyModuleRow {
                    label: colony_module_label(&m.def),
                    damaged: m.damaged,
                    fraction: f,
                })
                .collect(),
            stores: c
                .stores
                .iter()
                .map(|(r, s)| up::ColonyStoreRow {
                    resource: r.as_str().to_string(),
                    level: s.level_at(now),
                    capacity: s.capacity,
                    rate_per_s: s.net_rate_per_s,
                })
                .collect(),
            t_eq_k: c.t_eq_k,
            inspection_days: c
                .inspection_tick
                .map(|t| t.saturating_sub(now) as f64 / tick_s / 86_400.0),
            joinable: live && focus.is_some_and(|f| sim.colony_joinable(f, *id)),
            joined: focus
                .and_then(|f| sim.vessels().get(&f))
                .and_then(|v| v.joined_colony)
                == Some(*id),
        });
    }
    // W2.6 — the FACILITIES surface: in-flight build orders, the
    // stand's economics, and per-family qualification rows (split
    // out for the complexity bound; needs only the world half).
    stage_facilities(sim, input, now, tick_s, name_of);
    // The site table — stockpiles + the found-outpost slots.
    for s in sim.site_defs().values() {
        input.sites.push(up::SiteRow {
            id: s.id.as_str().to_string(),
            name: name_of(&s.display_name, s.id.as_str()),
            body: s.body.as_str().to_string(),
            launch: s.launch,
            // C09-F9: occupancy = a LIVE colony — `Abandoned` frees
            // the site for re-founding (the apply-side check agrees:
            // `AbandonColony` salvages the wreck, then the slot can
            // be re-founded).
            has_colony: sim
                .colonies()
                .values()
                .any(|c| c.site == s.id && !matches!(c.state, csp_sim::ColonyState::Abandoned)),
            inventory: sim
                .economy()
                .sites
                .get(&s.id)
                .map(|st| {
                    st.inventory
                        .iter()
                        .map(|(r, u)| (r.as_str().to_string(), *u))
                        .collect()
                })
                .unwrap_or_default(),
        });
    }
    // Live lab projects — closed-form reads at the staging tick.
    let cfg = sim.science_config();
    for p in sim.science().projects.values() {
        let t_s = now.saturating_sub(p.start_tick) as f64 / tick_s;
        input.projects.push(up::ProjectRow {
            id: p.id.0,
            vessel: p.vessel.0,
            label: format!(
                "{}@{}/{}{}",
                p.key.experiment.as_str(),
                p.key.biome.as_str(),
                p.key.body.as_str(),
                p.key.situation.name()
            ),
            rate_per_s: p.rate_science_per_s,
            accrued: csp_sim::research::accrued(p, &cfg, t_s),
            data_left: csp_sim::research::data_at(
                p.data_start,
                p.rate_science_per_s,
                p.throughput_data_per_s,
                cfg.science_per_data,
                t_s,
            ),
            eta_s: p.exhaust_tick.saturating_sub(now) as f64 / tick_s,
        });
    }
    // The agenda — pending player alarms and watched windows.
    for e in sim.pending_events() {
        let label = match &e.kind {
            csp_sim::EventKind::PlayerAlarm { label } => Some(format!("alarm: {label}")),
            csp_sim::EventKind::TransferWindow {
                origin,
                destination,
            } => Some(format!(
                "window {}→{}",
                origin.as_str(),
                destination.as_str()
            )),
            _ => None,
        };
        if let Some(label) = label {
            input.alarms.push(up::AlarmRow {
                tick: e.tick,
                in_s: e.tick.saturating_sub(now) as f64 / tick_s,
                label,
            });
        }
    }
    // In-flight transfers — pending `TransferComplete`s.
    for t in sim.transfers().values() {
        let end = |e: csp_sim::transfers::TransferEnd| match e {
            csp_sim::transfers::TransferEnd::Vessel(v) => format!("vessel {}", v.0),
            csp_sim::transfers::TransferEnd::Colony(c) => format!("colony {}", c.0),
        };
        let cargo = match &t.cargo {
            csp_sim::transfers::TransferCargo::Resource { resource, amount } => {
                format!("{resource} {amount:.1}")
            }
            csp_sim::transfers::TransferCargo::Crew { crew } => format!("crew {}", crew.0),
        };
        input.transfers.push(up::TransferRow {
            label: format!("{}: {} → {}", cargo, end(t.from), end(t.to)),
            eta_s: t.complete_tick.saturating_sub(now) as f64 / tick_s,
        });
    }
    // The focused vessel's logistics/science extras: cargo pool,
    // free seats, the joined colony, lab + chute nodes, and the
    // resolved researcher pool `StartResearch` truncates.
    if let Some(f) = focus
        && let Some(v) = sim.vessels().get(&f)
    {
        input.focus_joined = v.joined_colony.map(|c| c.0);
        input.focus_free_seats = free_seats(sim, v, f);
        let mut agg: std::collections::BTreeMap<String, f64> = std::collections::BTreeMap::new();
        for stores in v.tank_contents.values() {
            for (r, u) in stores {
                *agg.entry(r.as_str().to_string()).or_insert(0.0) += u;
            }
        }
        input.cargo = agg.into_iter().collect();
        if let Some(craft) = &v.craft {
            for (nid, node) in &craft.graph.nodes {
                let Some(part) = craft.parts.get(node.effective_index as usize) else {
                    continue;
                };
                let part_name = craft
                    .part_ids
                    .get(node.effective_index as usize)
                    .and_then(|pid| sim.parts().get(pid))
                    .map(|d| name_of(&d.display_name, d.id.as_str()))
                    .unwrap_or_else(|| format!("part {}", nid.0));
                // The part-action surfaces' name table — the
                // right-click pick resolves a node against these
                // rows (and the whole-vessel menu lists them).
                input.parts.push(up::PartRow {
                    node: nid.0,
                    part: part_name.clone(),
                });
                if let Some(lab) = &part.lab {
                    lab_slots.insert(nid.0, lab.researcher_slots);
                    input.labs.push(up::LabRow {
                        node: nid.0,
                        part: part_name.clone(),
                        slots: lab.researcher_slots,
                        running: sim
                            .science()
                            .projects
                            .values()
                            .any(|p| p.vessel == f && p.lab_node == *nid),
                    });
                }
                if part.parachute.is_some() {
                    input.chutes.push(up::ChuteRow {
                        node: nid.0,
                        part: part_name,
                        deployed: v.deployed.contains_key(nid),
                    });
                }
            }
        }
        // C22-F3: the LABS research buttons gate on the same crew
        // pool the `StartResearch` resolver truncates — staged once
        // so the panel disables with the reason instead of
        // dead-clicking onto the "NO CREW ABOARD" drop.
        input.crew_aboard = !v.crew.is_empty();
        for c in &v.crew {
            // C08-F7: the roster record is the claimed skill's source
            // of truth — `apply_start_research` cross-checks it
            // verbatim. An id-only `BoardCrew` member (no roster row)
            // claims the floor 1, the sim's `map_or(1, …)` convention;
            // a legacy skill-0 row claims its honest 0 (rate 0 —
            // `StartResearch` refuses, and no `.max(1)` may launder
            // it back into a legal assignment).
            let skill = sim.economy().crew.get(&c.id).map_or(1, |m| m.skill);
            researchers.push((c.id, skill));
        }
    }
}

/// The out-of-the-box stock craft — a three-part booster (pod over
/// tank over engine) built from raw `WeldPart`s so it needs no part
/// catalog (matches `csp-bench`'s scenario construction). The weak
/// adapter limits make a hard full-throttle ascent able to fail the
/// stack's joint — the fun gate's breakup demonstrator.
fn stock_flight_craft() -> csp_sim::CraftDef {
    use csp_phys::{ForceSite, ForceSiteKind, PartGraph, PartNode, PartNodeId, PartTransform};
    let mk = |mass: f64, half: f64, len: f64| csp_phys::WeldPart {
        experiments: Vec::new(),
        lab: None,
        engine: None,
        parachute: None,
        dock_ports: vec![],
        tanks: vec![],
        crew_seats: 0,
        structural: csp_data::StructuralDef {
            max_axial_n: 3.0e6,
            max_shear_n: 1.5e6,
            max_torque_nm: 2.0e6,
        },
        mass_kg: mass,
        com: csp_core::DVec3::ZERO,
        inertia: csp_phys::SymMat3::solid_box(mass, csp_core::DVec3::new(half, half, len)),
        collider: csp_phys::ColliderShape::Box {
            half_extents: csp_core::DVec3::new(half, half, len),
        },
        force_sites: Vec::new(),
        aero: csp_data::AeroDef {
            cd_curve: vec![csp_data::AeroKnot {
                mach: 0.0,
                coeff: 0.9,
            }],
            cl_curve: Vec::new(),
            ref_len_m: 2.0 * half,
        },
        thermal: csp_data::ThermalDef {
            heat_capacity_j_per_k: 400.0 * mass,
            max_temp_k: 1_500.0,
            emissivity: 0.8,
            conduction_w_per_k: 50.0,
        },
    };
    let mut engine = mk(500.0, 0.5, 0.6);
    engine.force_sites.push(ForceSite::new(
        csp_core::DVec3::new(0.0, 0.0, -0.6),
        csp_core::DVec3::Z,
        ForceSiteKind::Thrust,
        250_000.0,
    ));
    // The adapter under the tank is the authored weak link.
    let mut adapter = mk(150.0, 0.45, 0.2);
    adapter.structural = csp_data::StructuralDef {
        max_axial_n: 120_000.0,
        max_shear_n: 60_000.0,
        max_torque_nm: 40_000.0,
    };
    let tank = mk(4_500.0, 0.5, 1.2);
    let pod = mk(1_200.0, 0.55, 0.5);
    let (e, a, t, p) = (PartNodeId(1), PartNodeId(2), PartNodeId(3), PartNodeId(4));
    let mut graph = PartGraph::new(e);
    graph.insert(e, PartNode::new(None, PartTransform::IDENTITY, 0));
    graph.insert(
        a,
        PartNode::new(
            Some(e),
            PartTransform::new(
                csp_core::DVec3::new(0.0, 0.0, 0.8),
                csp_core::DQuat::IDENTITY,
            ),
            1,
        ),
    );
    graph.insert(
        t,
        PartNode::new(
            Some(a),
            PartTransform::new(
                csp_core::DVec3::new(0.0, 0.0, 1.4),
                csp_core::DQuat::IDENTITY,
            ),
            2,
        ),
    );
    graph.insert(
        p,
        PartNode::new(
            Some(t),
            PartTransform::new(
                csp_core::DVec3::new(0.0, 0.0, 1.7),
                csp_core::DQuat::IDENTITY,
            ),
            3,
        ),
    );
    csp_sim::CraftDef {
        part_build_resources: Vec::new(),
        hash_memo: Default::default(),
        part_ids: vec![],
        graph,
        parts: vec![engine, adapter, tank, pod],
        // M7-C: the stock craft's authored build cost — a modest
        // starter craft's price tag (the VAB path reads real resolved
        // `cost_funds`; this debug craft authors its own).
        part_costs: vec![12_000.0, 800.0, 4_000.0, 9_000.0],
        // M9b: hand-authored debug craft — no reliability model
        // (the empty-table convention).
        reliability: vec![],
    }
}

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod startup_tests {
    use super::*;

    /// §8.2 wired: a loadable-but-invalid `data/` tree is refused
    /// loudly at startup — a duplicate site id loads fine but the
    /// validator flags it, and `from_source` now runs that sweep
    /// before installing anything.
    #[test]
    fn from_source_refuses_invalid_data() {
        const BAD_SITES: &[u8] = br#"(
            schema_version: 2,
            sites: [
                (schema_version: 2, id: "dup", display_name: "s.dup",
                 body: "terrin", latitude_rad: 0.0, longitude_rad: 0.0,
                 altitude_m: 0.0, launch: true, resource_yields: []),
                (schema_version: 2, id: "dup", display_name: "s.dup",
                 body: "terrin", latitude_rad: 0.1, longitude_rad: 0.0,
                 altitude_m: 0.0, launch: false, resource_yields: []),
            ],
        )"#;
        let mut files: Vec<(&'static str, &'static [u8])> = crate::demo::FILES.to_vec();
        files.retain(|(p, _)| *p != "data/systems/home/sites.ron");
        files.push(("data/systems/home/sites.ron", BAD_SITES));
        let src = csp_data::EmbeddedSource::new(Box::leak(files.into_boxed_slice()));
        match View::from_source(&src, std::path::Path::new("data"), 7) {
            Err(ViewError::Validation(errs)) => {
                assert!(
                    errs.iter()
                        .any(|e| { matches!(e, csp_data::ValidationError::InvalidSite { .. }) })
                );
            }
            other => panic!(
                "expected Validation, got {}",
                other.map(|_| "Ok").unwrap_or_else(|e| e.to_string().leak())
            ),
        }
    }
}

#[cfg(test)]
// §2.13's no-panic rule covers shipping fallible paths; tests signal
// failure by panicking, so unwrap/expect stays legal inside test code.
#[allow(clippy::unwrap_used, clippy::expect_used)]
#[cfg(not(target_arch = "wasm32"))]
mod tests {
    use super::*;

    /// §2.5 + Appendix A 1–3, exercised headlessly: enqueue the warp
    /// controller's two commands, drive one frame through
    /// Input → SimStep → Sync → Presentation → UiBuild → RenderExtract,
    /// and assert the mirrors were written by Sync and the egui pass ran
    /// inside UiBuild.
    #[test]
    fn one_frame_mirrors_state_and_builds_ui() {
        let mut view = View::default();
        // §2.6.3's producer pattern: SetWarp then AdvanceTo, one source.
        view.enqueue_command(SourceClass::Ui, 0, SimCommand::SetWarp(2));
        view.enqueue_command(SourceClass::Ui, 0, SimCommand::AdvanceTo(64));

        view.begin_frame(); // Input — before SimStep, always (§2.5)
        view.sim_step(64).unwrap(); // SimStep — one step_frame call
        view.end_frame(); // Sync → Presentation → UiBuild → RenderExtract

        // Sync mirror writes happened (Appendix A item 1).
        assert_eq!(view.world.resource::<Clock>().0.ticks(), 64);
        assert_eq!(view.world.resource::<WarpMirror>().0, 2);
        let stats = view.world.resource::<FrameStats>();
        assert_eq!(stats.ticks_last_frame, 64);
        assert_eq!(stats.frames, 1);
        // UiBuild ran the egui pass and produced paintable output. The
        // `#[must_use]` TexturesDelta is cleared here the way a painter's
        // consumption would handle it (epaint debug-asserts on drop).
        let mut output = view.take_ui_frame().expect("UiBuild ran");
        output.textures_delta.clear();
        // Both commands logged in production order — the §7.6 artifact.
        let log = view.sim.command_log();
        assert_eq!(log.len(), 2);
        assert_eq!(log[0].1, SimCommand::SetWarp(2));
        assert_eq!(log[1].1, SimCommand::AdvanceTo(64));
        assert!(log[0].0 < log[1].0);
    }

    /// The UiBuild → Input hand-off: a panel request written during
    /// `UiBuild` is drained by `take_ui_outbox` for next frame's Input,
    /// exactly once.
    #[test]
    fn ui_outbox_drains_for_next_frames_input() {
        let mut view = View::default();
        *view.world.resource_mut::<UiOutbox>() = UiOutbox {
            warp_step: 1,
            ..UiOutbox::default()
        };
        let drained = view.take_ui_outbox();
        assert_eq!(drained.warp_step, 1);
        assert_eq!(view.take_ui_outbox().warp_step, 0); // consumed once
    }

    /// A zero-advance frame still runs the whole pipeline and mirrors the
    /// unchanged clock — `ticks_last_frame` reports 0, not a stale count.
    #[test]
    fn zero_advance_frame_reports_zero_ticks() {
        let mut view = View::default();
        view.sim_step(10).unwrap();
        view.end_frame();
        view.sim_step(10).unwrap(); // target already reached
        view.end_frame();
        assert_eq!(view.world.resource::<FrameStats>().ticks_last_frame, 0);
        assert_eq!(view.world.resource::<Clock>().0.ticks(), 10);
    }

    /// R15-F3 regression: `map_present` and `flight_present` sit in
    /// one unordered Presentation tuple — `flight_present` runs even
    /// in Map mode, so its chase solve must land on
    /// `flight::FlightCameraF64` and leave `map::MapCameraF64` alone.
    /// Before the resource split both wrote one `ResMut<MapCameraF64>`
    /// and the last writer won in unspecified order. The two solves
    /// land ~6e10 m vs 60 m apart, so a clobber is unmissable.
    #[test]
    fn flight_camera_cannot_clobber_the_map_camera() {
        let mut view = View::default();
        // Stage a vessel so flight_present takes the solving branch
        // (empty staging → the early `valid = false` path).
        view.world
            .resource_mut::<flight::FlightSnapshot>()
            .vessels
            .push(flight::FlightBody {
                id: 7,
                pos: csp_core::DVec3::ZERO,
                rot: csp_core::DQuat::IDENTITY,
                vel: csp_core::DVec3::ZERO,
                throttle: 0.0,
                debris: false,
                contact: false,
                plasma_w_m2: 0.0,
                nodes: 1,
                parts: std::sync::Arc::new(Vec::new()),
                plumes: Vec::new(),
                com: csp_core::DVec3::ZERO,
                latched: false,
                tanks: Vec::new(),
            });
        view.set_mode(ViewMode::Map);
        // Run just the Presentation stage — `end_frame` would first
        // re-stage `FlightSyncInput` from the (empty) sim and wipe the
        // staged vessel; the race this pins lives inside the tuple.
        view.schedules.presentation.run(&mut view.world);

        // The map camera is the map's own solve — not whatever the
        // chase solve happened to run second.
        let map_dist = view.world.resource::<map::MapInput>().distance_m;
        let cam = view.world.resource::<map::MapCameraF64>();
        assert!(cam.valid, "map camera must be solved in map mode");
        assert_eq!(cam.distance_m, map_dist);
        // …and the chase camera exists on its own resource.
        let flight_dist = view.world.resource::<flight::FlightInput>().distance_m();
        let fcam = view.world.resource::<flight::FlightCameraF64>();
        assert!(fcam.0.valid, "chase camera must solve with a staged vessel");
        assert_eq!(fcam.0.distance_m, flight_dist);
    }

    /// C14-F2's enumeration half: EVERY floating window the UiBuild
    /// pass draws over the VAB must sit in `vab_pointer_input`'s mask
    /// set — top bar, the three editor panels, help, settings, the
    /// §2.13 crash dialog, and the default-on tutorial card. A click
    /// through any of them used to reach the 3D scene behind it. The
    /// probe is `cursor_px`: it publishes `Some` only while the
    /// pointer is over unmasked viewport, so a window that forgot to
    /// mask itself leaks `Some` back at its own center.
    #[test]
    fn every_vab_overlay_window_masks_the_viewport() {
        let mut view = View::default();
        view.set_mode(ViewMode::Vab);
        // Turn every conditional overlay on: help + settings windows,
        // the crash dialog, and the tutorial card.
        view.toggle_help_panel();
        view.toggle_settings_panel();
        view.update_session(16.0, 1, 1, Some(SimError::Fatal("test halt")));
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        let input = |events: Vec<egui::Event>| egui::RawInput {
            screen_rect: Some(screen),
            focused: true,
            events,
            ..Default::default()
        };
        // Probe the overlay windows at their anchors — the host-side
        // half of C14-F2: `build_ui` must actually feed every drawn
        // window's rect into the pointer pass (the per-window contract
        // — that each returns a positive rect — is the csp-ui
        // enumeration test's job).
        let probes: [(&str, egui::Pos2); 6] = [
            ("mode_bar (top bar)", egui::pos2(400.0, 10.0)),
            ("Parts (palette)", egui::pos2(150.0, 300.0)),
            ("Craft (inspector)", egui::pos2(650.0, 300.0)),
            ("First flight (tutorial)", egui::pos2(400.0, 80.0)),
            ("Simulation halted (crash)", egui::pos2(400.0, 300.0)),
            ("Controls (help)", egui::pos2(700.0, 300.0)),
        ];
        for (title, pos) in probes {
            view.set_egui_input(input(vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Secondary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Secondary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ]));
            view.begin_frame();
            view.end_frame();
            view.with_vab_session(|v| {
                assert_eq!(
                    v.cursor_px(),
                    None,
                    "a click through '{title}' reached the 3D viewport"
                );
            });
        }
        // …and a click on genuinely free viewport still publishes the
        // hover ray — the mask set must not eat the whole screen. The
        // overlay union shifts as panels grow rows (the help card's
        // flight-controls list did), so probe a few candidate free
        // pixels — one clear viewport point must survive.
        let mut published = false;
        for pos in [
            egui::pos2(400.0, 500.0),
            egui::pos2(700.0, 500.0),
            egui::pos2(700.0, 400.0),
            egui::pos2(750.0, 550.0),
            egui::pos2(640.0, 450.0),
        ] {
            view.set_egui_input(input(vec![egui::Event::PointerMoved(pos)]));
            view.begin_frame();
            view.end_frame();
            if view.with_vab_session(|v| v.cursor_px().is_some()) {
                published = true;
                break;
            }
        }
        assert!(
            published,
            "every overlay mask still leaves a free viewport point"
        );
    }

    /// §3.5's recovery verbs reach the sim: the panel's Recover /
    /// Terminate actions map to `RecoverVessel` (naming the staged
    /// launch site — `None` is the remote-salvage leg) and
    /// `TerminateVessel`, against the focused latched vessel — the
    /// commands the pad-trap refusal text's "recover" relies on.
    #[test]
    fn program_command_maps_recovery_verbs() {
        use csp_ui::program::ProgramCommand as C;
        let mut dropped = Vec::new();
        let mirror = ProgramMirror {
            vessel: Some(csp_sim::VesselId(7)),
            recovery_site: Some(csp_data::SiteId::from("terrin-launch")),
            ..ProgramMirror::default()
        };
        assert_eq!(
            program_command(&mirror, C::RecoverVessel, &mut dropped),
            Some(SimCommand::RecoverVessel {
                vessel: csp_sim::VesselId(7),
                site: Some(csp_data::SiteId::from("terrin-launch")),
            })
        );
        assert_eq!(
            program_command(&mirror, C::TerminateVessel, &mut dropped),
            Some(SimCommand::TerminateVessel {
                vessel: csp_sim::VesselId(7),
            })
        );
        assert!(dropped.is_empty());
        // No site on the latched body → the remote-salvage leg.
        let remote = ProgramMirror {
            vessel: Some(csp_sim::VesselId(7)),
            ..ProgramMirror::default()
        };
        assert_eq!(
            program_command(&remote, C::RecoverVessel, &mut dropped),
            Some(SimCommand::RecoverVessel {
                vessel: csp_sim::VesselId(7),
                site: None,
            })
        );
        // No focused vessel → nothing emitted, but the drop is reported.
        let empty = ProgramMirror::default();
        assert_eq!(
            program_command(&empty, C::RecoverVessel, &mut dropped),
            None
        );
        assert_eq!(
            program_command(&empty, C::TerminateVessel, &mut dropped),
            None
        );
        assert_eq!(dropped.len(), 2, "each drop reports itself");
    }

    /// W2.0/W2.3's dispatch contract: every progression-surface
    /// `ProgramCommand` resolves to its `SimCommand` (focused-vessel
    /// verbs through the mirror's vessel, id-addressed verbs
    /// verbatim), and focused-vessel verbs drop with a notice on a
    /// stale mirror.
    #[allow(clippy::too_many_lines)] // the mapping table IS the test
    #[test]
    fn program_command_maps_progression_verbs() {
        use csp_ui::program::ProgramCommand as C;
        let mut dropped = Vec::new();
        let mirror = ProgramMirror {
            vessel: Some(csp_sim::VesselId(7)),
            now_ticks: 1_000,
            ..ProgramMirror::default()
        };
        let maps: &[(C, SimCommand)] = &[
            (
                C::TransferCrew {
                    crew: 2,
                    to_vessel: 9,
                },
                SimCommand::TransferCrew {
                    from_vessel: csp_sim::VesselId(7),
                    to_vessel: csp_sim::VesselId(9),
                    crew: csp_sim::CrewId(2),
                },
            ),
            (
                C::TransferResource {
                    to_vessel: 9,
                    resource: "ore".into(),
                    amount: 10.0,
                },
                SimCommand::TransferResource {
                    from_vessel: csp_sim::VesselId(7),
                    to_vessel: csp_sim::VesselId(9),
                    resource: csp_data::ResourceId::from("ore"),
                    amount: 10.0,
                },
            ),
            (
                C::AssignCrewToColony { crew: 2, colony: 3 },
                SimCommand::AssignCrewToColony {
                    colony: csp_sim::ColonyId(3),
                    crew: csp_sim::CrewId(2),
                },
            ),
            (
                C::EvacuateColony { colony: 3 },
                SimCommand::EvacuateColony {
                    colony: csp_sim::ColonyId(3),
                },
            ),
            (
                C::AbandonColony { colony: 3 },
                SimCommand::AbandonColony {
                    colony: csp_sim::ColonyId(3),
                },
            ),
            (
                C::RepairColonyModule {
                    colony: 3,
                    module: 1,
                },
                SimCommand::RepairColonyModule {
                    colony: csp_sim::ColonyId(3),
                    module: 1,
                },
            ),
            (
                C::ScheduleColonyInspection { colony: 3 },
                SimCommand::ScheduleColonyInspection {
                    colony: csp_sim::ColonyId(3),
                    tick: 1_000 + csp_core::TICK_HZ * 86_400,
                },
            ),
            (
                C::JoinColony { colony: 3 },
                SimCommand::JoinColonyInventory {
                    vessel: csp_sim::VesselId(7),
                    colony: csp_sim::ColonyId(3),
                },
            ),
            (
                C::LeaveColony,
                SimCommand::LeaveColonyInventory {
                    vessel: csp_sim::VesselId(7),
                },
            ),
            (
                C::TransferColonyResource {
                    colony: 3,
                    resource: "ore".into(),
                    amount: 5.0,
                    to_colony: true,
                },
                SimCommand::TransferColonyResource {
                    vessel: csp_sim::VesselId(7),
                    colony: csp_sim::ColonyId(3),
                    resource: csp_data::ResourceId::from("ore"),
                    amount: 5.0,
                    to_colony: true,
                },
            ),
            (
                C::TransferColonyCrew {
                    colony: 3,
                    crew: 2,
                    to_colony: false,
                },
                SimCommand::TransferColonyCrew {
                    vessel: csp_sim::VesselId(7),
                    colony: csp_sim::ColonyId(3),
                    crew: csp_sim::CrewId(2),
                    to_colony: false,
                },
            ),
            (
                C::DeployPart { node: 4 },
                SimCommand::DeployPart {
                    vessel: csp_sim::VesselId(7),
                    node: csp_phys::PartNodeId(4),
                },
            ),
            (
                C::StopResearch { project: 5 },
                SimCommand::StopResearch {
                    project: csp_sim::ResearchId(5),
                },
            ),
            (
                C::SetAlarm {
                    in_seconds: 60.0,
                    label: "x".into(),
                },
                SimCommand::SetAlarm {
                    tick: 1_000 + 60 * csp_core::TICK_HZ,
                    label: "x".into(),
                },
            ),
            (
                C::DisengageAutopilot,
                SimCommand::DisengageAutopilot {
                    vessel: csp_sim::VesselId(7),
                },
            ),
            (
                C::BoardCrew,
                SimCommand::BoardCrew {
                    vessel: csp_sim::VesselId(7),
                },
            ),
            (
                C::DeclineContract { offer: 6 },
                SimCommand::DeclineContract { offer: 6 },
            ),
            (
                C::CancelBuild { build: 8 },
                SimCommand::CancelBuild {
                    build: csp_sim::manufacture::BuildId(8),
                },
            ),
        ];
        for (pc, want) in maps {
            assert_eq!(
                program_command(&mirror, pc.clone(), &mut dropped),
                Some(want.clone()),
                "{pc:?}"
            );
        }
        // `FoundColony` builds the starter def (site verbatim).
        match program_command(
            &mirror,
            C::FoundColony {
                site: "rask-mines".into(),
            },
            &mut dropped,
        ) {
            Some(SimCommand::FoundColony { def }) => {
                assert_eq!(def.site.as_str(), "rask-mines");
                assert!(!def.modules.is_empty());
            }
            other => panic!("FoundColony → {other:?}"),
        }
        // `HireCrew` authors the kind/name/salary — the panel's
        // purchased skill rides through verbatim (C08-F7).
        match program_command(
            &mirror,
            C::HireCrew {
                kind: "scientist".into(),
                skill: 2,
            },
            &mut dropped,
        ) {
            Some(SimCommand::HireCrew { kind, .. }) => {
                assert_eq!(kind, csp_sim::CrewKind::Scientist)
            }
            other => panic!("HireCrew → {other:?}"),
        }
        assert!(dropped.is_empty(), "no drops with a focused vessel");

        // Stale mirror → focused-vessel verbs drop with notices.
        let empty = ProgramMirror::default();
        for pc in [
            C::BoardCrew,
            C::LeaveColony,
            C::JoinColony { colony: 1 },
            C::TransferColonyCrew {
                colony: 1,
                crew: 0,
                to_colony: true,
            },
            C::DeployPart { node: 0 },
        ] {
            assert_eq!(program_command(&empty, pc, &mut dropped), None);
        }
        // Id-addressed verbs still go through without a focus (a stale
        // id is the drain's Recoverable rejection, not a mirror drop).
        assert_eq!(
            program_command(&empty, C::EvacuateColony { colony: 3 }, &mut dropped),
            Some(SimCommand::EvacuateColony {
                colony: csp_sim::ColonyId(3),
            })
        );
        assert_eq!(
            program_command(&empty, C::DeclineContract { offer: 6 }, &mut dropped),
            Some(SimCommand::DeclineContract { offer: 6 })
        );
        assert_eq!(
            program_command(&empty, C::CancelBuild { build: 8 }, &mut dropped),
            Some(SimCommand::CancelBuild {
                build: csp_sim::manufacture::BuildId(8),
            })
        );
        assert_eq!(dropped.len(), 5, "every stale focus-drop reports");
    }

    /// C21-F7: a `StartResearch` drop names the real reason — a lab
    /// node absent from the staged `lab_slots` table is a stale row
    /// ("LAB NOT ON FOCUSED VESSEL"), distinct from the empty roster
    /// the same cue used to blame ("NO CREW ABOARD").
    #[test]
    fn start_research_drop_names_the_real_reason() {
        use csp_ui::program::ProgramCommand as C;
        let key = || PooledKey {
            key: csp_sim::research::DataKey {
                experiment: csp_data::ExperimentId::from("exp.materials_study"),
                body: csp_data::BodyId::from("terrin"),
                biome: csp_data::BiomeId::from(""),
                situation: csp_data::Situation::SpaceLow,
            },
        };
        let mut dropped = Vec::new();
        // Lab node absent from `lab_slots` — not aboard the focus.
        let stale = ProgramMirror {
            vessel: Some(csp_sim::VesselId(7)),
            keys: vec![key()],
            ..ProgramMirror::default()
        };
        assert_eq!(
            program_command(
                &stale,
                C::StartResearch {
                    lab_node: 9,
                    row: 0
                },
                &mut dropped
            ),
            None
        );
        assert_eq!(dropped, ["RESEARCH DROPPED — LAB NOT ON FOCUSED VESSEL"]);
        // Lab aboard but the roster is empty — the resolver's own
        // crew check is the reason.
        dropped.clear();
        let crewless = ProgramMirror {
            vessel: Some(csp_sim::VesselId(7)),
            keys: vec![key()],
            lab_slots: [(9_u64, 2_u32)].into_iter().collect(),
            ..ProgramMirror::default()
        };
        assert_eq!(
            program_command(
                &crewless,
                C::StartResearch {
                    lab_node: 9,
                    row: 0
                },
                &mut dropped
            ),
            None
        );
        assert_eq!(dropped, ["RESEARCH DROPPED — NO CREW ABOARD"]);
        // Crewed → the command resolves, researchers truncated to
        // the lab's slot count.
        let crewed = ProgramMirror {
            vessel: Some(csp_sim::VesselId(7)),
            keys: vec![key()],
            lab_slots: [(9_u64, 1_u32)].into_iter().collect(),
            researchers: vec![(csp_sim::CrewId(3), 2), (csp_sim::CrewId(4), 5)],
            ..ProgramMirror::default()
        };
        match program_command(
            &crewed,
            C::StartResearch {
                lab_node: 9,
                row: 0,
            },
            &mut dropped,
        ) {
            Some(SimCommand::StartResearch {
                vessel,
                lab_node,
                researchers,
                ..
            }) => {
                assert_eq!(vessel, csp_sim::VesselId(7));
                assert_eq!(lab_node, csp_phys::PartNodeId(9));
                assert_eq!(researchers.len(), 1, "truncated to the lab's slots");
                assert_eq!(researchers[0].crew, csp_sim::CrewId(3));
            }
            other => panic!("StartResearch → {other:?}"),
        }
    }

    /// C10-F2/C22-F5 + C22-F3's staged mirrors: the starter craft's
    /// reliability rows carry the §3.4 stress table (authored rules
    /// evaluated on the reconstructed regime), and `crew_aboard`
    /// tracks the focused vessel's crew for the LABS gate.
    #[test]
    fn program_mirror_stages_stress_table_and_crew_gate() {
        let mut view = View::new().expect("data/ load");
        view.seed_starter_craft().expect("starter seed");
        view.begin_frame();
        view.end_frame();
        {
            let mirror = view.world.resource::<ProgramMirror>();
            assert!(
                mirror
                    .input
                    .reliability
                    .iter()
                    .any(|r| !r.stress.is_empty()),
                "the shipped parts stage their authored stress rules: {:?}",
                mirror.input.reliability
            );
            assert!(
                mirror
                    .input
                    .reliability
                    .iter()
                    .all(|r| r.stress_product >= 1.0),
                "stress multipliers are ≥ 1 authored"
            );
            assert!(!mirror.input.crew_aboard, "the seed flies uncrewed");
        }
        view.sim.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::BoardCrew {
                vessel: csp_sim::VesselId(0),
            },
        );
        view.begin_frame();
        let report = view
            .sim_step(view.sim.clock().ticks() + 1)
            .expect("advance");
        assert!(report.rejected.is_empty(), "{:?}", report.rejected);
        view.end_frame();
        let mirror = view.world.resource::<ProgramMirror>();
        assert!(
            mirror.input.crew_aboard,
            "a boarded member flips the LABS gate"
        );
    }

    /// W1.2's gate: a rails vessel in space stages a REAL science
    /// context — the situation carries it (space situations have no
    /// biome; the sim accepts the empty `BiomeId` there), so a
    /// `RunExperiment` click in orbit resolves to a `SimCommand`
    /// instead of being silently dropped. Before the fix,
    /// `science_ctx` zipped on the absent biome and this command
    /// could never leave the panel.
    #[test]
    fn rails_vessel_stages_orbital_science_ctx() {
        let mut view = View::new_demo().expect("demo data loads");
        let (mu, a) = (3.5316e12_f64, 700_000.0_f64);
        view.sim.enqueue(
            csp_sim::SourceClass::Ui,
            0,
            SimCommand::SpawnVessel {
                parent: csp_data::BodyId::from("terrin"),
                elements: csp_data::KeplerElements {
                    semi_major_axis_m: a,
                    eccentricity: 0.0,
                    inclination_rad: 0.0,
                    ascending_node_rad: 0.0,
                    arg_periapsis_rad: 0.0,
                    mean_anomaly_epoch_rad: 0.0,
                    period_s: 2.0 * std::f64::consts::PI * (a * a * a / mu).sqrt(),
                },
            },
        );
        let report = view
            .sim_step(view.sim.clock().ticks() + 1)
            .expect("advance");
        assert!(
            report.rejected.is_empty(),
            "spawn rejected: {:?}",
            report.rejected
        );
        // The pad craft (berthed SurfaceLatched by `new_demo`'s
        // seed — the seed steps through its `LaunchCraft`
        // drain at construction, so it is already vessel 0) always
        // wins the flight-focus half of the mirror's vessel pick —
        // the map-focus path only runs when no flight vessel is
        // staged, so retire the pad craft like a player would.
        view.sim.enqueue(
            csp_sim::SourceClass::Ui,
            0,
            SimCommand::TerminateVessel {
                vessel: csp_sim::VesselId(0),
            },
        );
        let report = view
            .sim_step(view.sim.clock().ticks() + 1)
            .expect("advance");
        assert!(
            report.rejected.is_empty(),
            "terminate rejected: {:?}",
            report.rejected
        );
        // Focus the rails vessel (the map-focus path the player uses).
        view.world
            .resource_mut::<flight::FlightFocusOverride>()
            .target = None;
        view.world.resource_mut::<map::MapInput>().focus_vessel = Some(csp_sim::VesselId(1));
        view.end_frame();
        let mirror = view.world.resource::<ProgramMirror>();
        let dump: Vec<String> = view
            .sim
            .vessels()
            .iter()
            .map(|(id, v)| format!("{}={:?}", id.0, v.state))
            .collect();
        assert_eq!(mirror.vessel, Some(csp_sim::VesselId(1)), "{dump:?}");
        assert_eq!(
            mirror.science_ctx.as_ref().map(|(_, s)| *s),
            Some(csp_data::Situation::SpaceLow),
            "orbit stages a context — the biome half is empty by design; vessels: {dump:?}"
        );
        let mut dropped = Vec::new();
        let cmd = program_command(
            mirror,
            csp_ui::program::ProgramCommand::RunExperiment {
                node: 0,
                experiment: "exp.materials_study".into(),
            },
            &mut dropped,
        );
        assert!(dropped.is_empty(), "dropped: {dropped:?}");
        assert_eq!(
            cmd,
            Some(SimCommand::RunExperiment {
                vessel: csp_sim::VesselId(1),
                node: csp_phys::PartNodeId(0),
                experiment: csp_data::ExperimentId::from("exp.materials_study"),
                biome: csp_data::BiomeId::from(""),
                situation: csp_data::Situation::SpaceLow,
            }),
            "RunExperiment must reach the outbox in orbit"
        );
    }

    /// One 1 m³ cube of `mass_kg` — a sealed box for the water: at
    /// 500 kg it floats, at 2 000 kg it sinks.
    fn water_cube(mass_kg: f64) -> csp_sim::CraftDef {
        use csp_phys::{PartGraph, PartNode, PartNodeId, PartTransform};
        let mut graph = PartGraph::new(PartNodeId(1));
        graph.insert(
            PartNodeId(1),
            PartNode::new(None, PartTransform::IDENTITY, 0),
        );
        csp_sim::CraftDef {
            part_build_resources: Vec::new(),
            hash_memo: Default::default(),
            part_ids: vec![],
            graph,
            part_costs: vec![0.0],
            parts: vec![csp_phys::WeldPart {
                experiments: Vec::new(),
                lab: None,
                engine: None,
                parachute: None,
                dock_ports: vec![],
                tanks: vec![],
                crew_seats: 0,
                structural: csp_data::StructuralDef::unlimited(),
                mass_kg,
                com: csp_core::DVec3::ZERO,
                inertia: csp_phys::SymMat3::solid_box(mass_kg, csp_core::DVec3::new(0.5, 0.5, 0.5)),
                collider: csp_phys::ColliderShape::Box {
                    half_extents: csp_core::DVec3::new(0.5, 0.5, 0.5),
                },
                force_sites: Vec::new(),
                aero: csp_data::AeroDef::constant(0.8, 0.0, 1.0),
                thermal: csp_data::ThermalDef {
                    heat_capacity_j_per_k: 400_000.0,
                    max_temp_k: 1_500.0,
                    emissivity: 0.8,
                    conduction_w_per_k: 50.0,
                },
            }],
            reliability: vec![],
        }
    }

    /// Water physics (sub-phase 4.1): the situation label is the
    /// simulation's verdict, not altitude arithmetic. Over Terrin's
    /// ocean on the equator at longitude 0 — 193 m of water there — a
    /// sealed 500 kg cube floats and a 2 000 kg one sinks to the
    /// seabed, and both come to rest. The floater's origin ends a
    /// centimetre above the datum and the sinker's 192 m below it, so
    /// an altitude rule calls them `Landed` and `Splashed`; the view
    /// follows `SimWorld::splashed` and stages the reverse.
    #[test]
    fn situation_follows_the_simulation_not_altitude() {
        let mut view = View::new_demo().expect("demo data loads");
        let step = |view: &mut View| {
            view.sim_step(view.sim.clock().ticks() + 1).expect("step");
        };
        // The pad craft wins the mirror's flight-focus pick — retire it.
        view.enqueue_command(
            SourceClass::Ui,
            PLAYER_SOURCE_ID,
            SimCommand::TerminateVessel {
                vessel: csp_sim::VesselId(0),
            },
        );
        step(&mut view);
        for (mass_kg, situation, sign_of_altitude, what) in [
            (500.0, csp_data::Situation::Splashed, 1.0, "afloat"),
            (2_000.0, csp_data::Situation::Landed, -1.0, "on the seabed"),
        ] {
            let (center, radius, spin) = {
                let env = view.sim.environment().expect("physics");
                (env.body_center, env.radius_m, env.body_spin)
            };
            let pos = center + csp_core::DVec3::new(radius + 1.5, 0.0, 0.0);
            view.enqueue_command(
                SourceClass::Ui,
                PLAYER_SOURCE_ID,
                SimCommand::SpawnPhysicalVessel {
                    craft: water_cube(mass_kg),
                    pos,
                    rot: csp_sim::bubble::surface_upright(0.0, 0.0),
                    // At rest relative to the water, which turns with
                    // the planet.
                    vel: spin.cross(pos - center),
                    ang_vel: csp_core::DVec3::ZERO,
                },
            );
            step(&mut view);
            let id = *view
                .sim
                .vessels()
                .keys()
                .next_back()
                .expect("the cube spawned");
            // A second at a time: the sinker has 192 m to cover.
            let mut pose = None;
            for _ in 0..130 {
                view.sim_step(view.sim.clock().ticks() + 64).expect("step");
                if let VesselState::SurfaceLatched(p) = &view.sim.vessels()[&id].state {
                    pose = Some(p.clone());
                    break;
                }
            }
            let pose = pose.unwrap_or_else(|| panic!("the {what} cube never came to rest"));
            assert!(
                pose.altitude_m * sign_of_altitude > 0.0,
                "the {what} cube rests {} m from the datum",
                pose.altitude_m
            );
            view.world
                .resource_mut::<flight::FlightFocusOverride>()
                .target = None;
            view.world.resource_mut::<map::MapInput>().focus_vessel = Some(id);
            view.end_frame();
            let mirror = view.world.resource::<ProgramMirror>();
            assert_eq!(mirror.vessel, Some(id));
            assert_eq!(
                mirror.science_ctx.as_ref().map(|(_, s)| *s),
                Some(situation),
                "a cube {what} at {sign_of_altitude} altitude stages its situation from the sim"
            );
            view.enqueue_command(
                SourceClass::Ui,
                PLAYER_SOURCE_ID,
                SimCommand::TerminateVessel { vessel: id },
            );
            step(&mut view);
        }
    }

    /// W2.6: the §3.4 test stand is player-reachable — the panel's
    /// `RunBenchTest` maps through `program_command` to the sim
    /// command (the FACILITIES rows the buttons gate on stage too),
    /// the apply reserves funds + stockpile + time, and
    /// `BenchTestComplete` credits the family's qualification.
    #[test]
    fn bench_test_reaches_the_stand() {
        use csp_ui::program::ProgramCommand as C;
        let mut view = View::new().expect("data/ load");
        // Stage the Program mirror once — the FACILITIES rows the
        // bench buttons read must name the stand and the family.
        view.begin_frame();
        view.end_frame();
        let site = csp_data::SiteId::from("terrin-launch");
        let family = csp_data::FamilyId::from("csp.family.engine.launch");
        let mut dropped = Vec::new();
        let cmd = {
            let mirror = view.world.resource::<ProgramMirror>();
            assert_eq!(
                mirror.input.bench_site.as_deref(),
                Some("terrin-launch"),
                "the stand stages the dominant body's launch site"
            );
            assert!(
                mirror
                    .input
                    .families
                    .iter()
                    .any(|f| f.id == "csp.family.engine.launch"),
                "the catalog's engine family stages a qualification row"
            );
            assert!(
                mirror.input.bench_stock >= 5.0,
                "the seeded stockpile covers a 5-unit run"
            );
            program_command(
                mirror,
                C::RunBenchTest {
                    site: site.as_str().to_string(),
                    family: "csp.family.engine.launch".into(),
                    units: 5.0,
                },
                &mut dropped,
            )
        };
        assert!(dropped.is_empty(), "dropped: {dropped:?}");
        let cmd = cmd.expect("RunBenchTest must reach the outbox");
        assert_eq!(
            cmd,
            SimCommand::RunBenchTest {
                site: site.clone(),
                family: family.clone(),
                units: 5.0,
            },
        );
        // C08-F6: the stand is a facility verb — the shipped tree
        // gates `facility.test_stand` at `tech.rocketry.3`, a 60-
        // science chain this fixture cannot afford. Install a tiny
        // tree that locks the stand behind a free node, then take
        // the unlock through the real `ResearchTech` verb.
        view.sim.set_tech_tree(csp_data::TechTree {
            nodes: vec![csp_data::TechNode {
                id: csp_data::TechNodeId::from("tech.n"),
                display_name: "n".into(),
                description: "d".into(),
                science_cost: 0.0,
                requires: vec![],
                unlocks: csp_data::TechUnlocks {
                    track_levels: vec![],
                    facilities: vec![csp_data::FacilityId::from("facility.test_stand")],
                    contract_types: vec![],
                },
            }],
            tracks: vec![],
        });
        view.sim.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::ResearchTech {
                node: csp_data::TechNodeId::from("tech.n"),
            },
        );
        view.begin_frame();
        let r = view
            .sim_step(view.sim.clock().ticks() + 1)
            .expect("advance");
        assert!(r.rejected.is_empty(), "{:?}", r.rejected);
        view.end_frame();
        let funds0 = view.sim.funds_micros();
        view.sim.enqueue(SourceClass::Ui, 0, cmd);
        view.begin_frame();
        let report = view
            .sim_step(view.sim.clock().ticks() + 1)
            .expect("advance");
        assert!(report.rejected.is_empty(), "{:?}", report.rejected);
        view.end_frame();
        assert_eq!(view.sim.bench_runs().len(), 1, "the stand is running");
        assert!(
            view.sim.funds_micros() < funds0,
            "the run reserved its funds"
        );
        let complete = view.sim.bench_runs().values().next().unwrap().complete_tick;
        view.begin_frame();
        view.sim_step(complete).expect("advance");
        view.end_frame();
        assert!(view.sim.bench_runs().is_empty());
        assert_eq!(
            view.sim.tech().qualifications[&family].bench_units,
            5.0,
            "completion credited the family's bench units"
        );
    }

    /// The VAB LAUNCH path is `LaunchCraft` — the build order's
    /// atomic spend with NO build wait: funds + the pad's stockpile
    /// BOM pay at the drain, and the craft berths `SurfaceLatched` in
    /// the SAME drain (no `ActiveBuild`, no `ConstructionComplete`).
    /// The berthed craft takes flight focus.
    #[test]
    fn launch_berths_the_craft_in_one_drain() {
        let mut view = View::new().expect("data/ load");
        view.seed_starter_craft().expect("starter seed");
        // Free the berth so the second launch can place (the pad is a
        // single berth; the seeded craft claims it latched).
        view.sim.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::TerminateVessel {
                vessel: csp_sim::VesselId(0),
            },
        );
        view.begin_frame();
        let report = view
            .sim_step(view.sim.clock().ticks() + 1)
            .expect("advance");
        assert!(report.rejected.is_empty(), "{:?}", report.rejected);
        view.end_frame();
        let pad = csp_data::SiteId::from("terrin-launch");
        let metal = csp_data::ResourceId::from("metal");
        let funds0 = view.sim.funds_micros();
        let stock0 = view.sim.economy().sites[&pad].inventory[&metal];

        view.launch_craft()
            .expect("second launch issues LaunchCraft");
        view.begin_frame();
        let report = view
            .sim_step(view.sim.clock().ticks() + 1)
            .expect("advance");
        assert!(report.rejected.is_empty(), "{:?}", report.rejected);
        view.end_frame();
        // ONE drain: no in-flight build was ever minted, the BOM +
        // funds already debited, and the craft IS on the pad.
        assert!(
            view.sim.builds().is_empty(),
            "a launch mints no build order"
        );
        assert!(
            view.sim.funds_micros() < funds0,
            "the launch debited the craft's funds"
        );
        assert!(
            view.sim.economy().sites[&pad].inventory[&metal] < stock0,
            "the launch debited the stockpile BOM"
        );
        assert_eq!(
            view.sim.vessels().len(),
            1,
            "the launch berthed the craft in one drain"
        );
        let (id, v) = view.sim.vessels().iter().next().unwrap();
        assert!(
            matches!(v.state, csp_sim::VesselState::SurfaceLatched(_)),
            "berthed surface-latched, got {:?}",
            v.state
        );
        assert_eq!(
            view.flight_focus(),
            Some(*id),
            "the berthed craft takes focus like a spawn did"
        );
    }

    /// The real player flow end-to-end: a parked craft on the pad, a
    /// LAUNCH press that recovers it and berths the new craft in the
    /// same drain, then the throttle key — `SetThrottle` must unlatch
    /// the berthed craft, open the throttle, and climb it off the pad.
    #[test]
    fn launch_deliver_throttle_flies() {
        let mut view = View::new().expect("data/ load");
        view.seed_starter_craft().expect("starter seed");
        // The starter parks latched on the pad (the same berth the
        // second launch must clear through its RecoverVessel arm).
        view.begin_frame();
        view.sim_step(view.sim.clock().ticks() + 8).unwrap();
        view.end_frame();
        assert_eq!(view.sim.vessels().len(), 1, "the seed parked one craft");

        // LAUNCH with the pad occupied: the parked craft recovers,
        // the new one berths on the freed pad in the same drain.
        view.launch_craft().expect("launch issues LaunchCraft");
        view.begin_frame();
        let report = view
            .sim_step(view.sim.clock().ticks() + 1)
            .expect("advance");
        assert!(report.rejected.is_empty(), "{:?}", report.rejected);
        view.end_frame();
        assert!(
            view.sim.builds().is_empty(),
            "a launch mints no build order"
        );
        assert_eq!(
            view.sim.vessels().len(),
            1,
            "the launch berthed the craft in one drain"
        );

        // The berthed latched craft holds the flight focus — the
        // throttle keys route to it.
        let focus = view.flight_focus().expect("the berthed craft takes focus");
        assert!(
            matches!(
                view.sim.vessels()[&focus].state,
                csp_sim::VesselState::SurfaceLatched(_)
            ),
            "berthed surface-latched, got {:?}",
            view.sim.vessels()[&focus].state
        );

        // The Z key — `throttle_dispatch`'s snap-to-full arm sends one
        // `SetThrottle` on the player FIFO.
        view.enqueue_command(
            SourceClass::Ui,
            PLAYER_SOURCE_ID,
            SimCommand::SetThrottle {
                vessel: focus,
                throttle: 1.0,
            },
        );
        view.begin_frame();
        let report = view
            .sim_step(view.sim.clock().ticks() + 1)
            .expect("advance");
        assert!(
            report.rejected.is_empty(),
            "the unlatching SetThrottle must not reject: {:?}",
            report.rejected
        );
        view.end_frame();

        let body = view.sim.vessels()[&focus]
            .body()
            .expect("throttle unlatched the delivered craft");
        assert!(body.throttle > 0.0, "the throttle command applied");
        let alt0 = body.pos.norm();

        // Burn for a few seconds — the craft must actually leave the
        // pad (climb), not sit unlatched-but-inert.
        view.begin_frame();
        view.sim_step(view.sim.clock().ticks() + 240).unwrap();
        view.end_frame();
        let body = view.sim.vessels()[&focus]
            .body()
            .expect("still flying InBubble");
        assert!(
            body.pos.norm() > alt0 + 1.0,
            "full throttle climbs the delivered craft: r {} -> {}",
            alt0,
            body.pos.norm()
        );
    }

    /// The player save path end to end (§7.2 + §7.5): `save_game`'s
    /// payload round-trips the codec, `load_game` rebuilds a world
    /// that hashes identically and keeps ticking identically — the
    /// same proof §7.5's replay golden demands, at the player seam.
    #[test]
    fn player_save_load_round_trips() {
        let mut view = View::new_demo().unwrap();
        view.begin_frame();
        view.sim_step(view.sim.clock().ticks() + 64).unwrap();
        view.end_frame();
        let hash_before = view.sim.state_hash().unwrap();
        let save = view.save_game().expect("save at drained boundary");
        let bytes = save::encode_save(&save).unwrap();
        let decoded = save::decode_save(&bytes).unwrap();
        let warnings = view.load_game(decoded.snapshot).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(view.sim.state_hash().unwrap(), hash_before);
        // And a restored world still ticks deterministically.
        view.begin_frame();
        view.sim_step(view.sim.clock().ticks() + 32).unwrap();
        view.end_frame();
    }

    /// C02-F3 regression: the first post-load `sync_focus_commands`
    /// resolves the RESTORED focus, not the pre-load session's stale
    /// `FlightUiData.focus`. The mirror is session state that survives
    /// `rebuild_from_snapshot`, so a stale VesselId colliding with a
    /// live vessel in the restored arena used to enqueue
    /// `SetVesselTracked`/`SetActiveVessel` for a vessel the player
    /// never focused — one frame before `flight_present` re-resolved
    /// the armed pin. Here the save's `active_vessel` is vessel B
    /// while a different live vessel sits on A's id in the arena; the
    /// boundary must issue nothing for A.
    #[test]
    fn load_game_seeds_the_restored_focus_into_the_boundary() {
        let mut view = View::new().expect("data/ load");
        let (center, r, mu, atm_top) = {
            let env = view.sim.environment().expect("physics");
            (
                env.body_center,
                env.radius_m,
                env.mu_m3_per_s2,
                env.atmosphere.as_ref().map_or(0.0, |a| a.top_m),
            )
        };
        // Two orbiters on opposite sides of the same clearing orbit:
        // A (id 0) coasts — it may rail off on the first tick and only
        // needs to exist in the arena — while B (id 1) runs powered so
        // it stays staged long enough for the focus boundary to record
        // it as `active_vessel` (the `auto_rails` test's pattern).
        let alt = atm_top.max(200_000.0) + 50_000.0;
        let orb_r = r + alt;
        let speed = (mu / orb_r).sqrt();
        for sign in [1.0_f64, -1.0] {
            view.enqueue_command(
                SourceClass::Ui,
                PLAYER_SOURCE_ID,
                SimCommand::SpawnPhysicalVessel {
                    craft: stock_flight_craft(),
                    pos: center + csp_core::DVec3::new(sign * orb_r, 0.0, 0.0),
                    rot: csp_core::DQuat::IDENTITY,
                    vel: csp_core::DVec3::new(0.0, sign * speed, 0.0),
                    ang_vel: csp_core::DVec3::ZERO,
                },
            );
        }
        view.enqueue_command(
            SourceClass::Ui,
            PLAYER_SOURCE_ID,
            SimCommand::SetThrottle {
                vessel: csp_sim::VesselId(1),
                throttle: 1.0,
            },
        );
        // Spawn drain → B stages as the newest vessel → the boundary
        // tracks it and records `active_vessel`; the last step drains
        // those commands so the snapshot lands at a §7.4 boundary.
        for _ in 0..4 {
            view.begin_frame();
            view.sim_step(view.sim.clock().ticks() + 1).expect("step");
            view.end_frame();
        }
        let a = csp_sim::VesselId(0);
        let b = csp_sim::VesselId(1);
        assert_eq!(
            view.sim.active_vessel(),
            Some(b),
            "the boundary recorded B before the save"
        );
        let snap = view.sim.snapshot().expect("serializable");

        // The stale half of the scenario: the previous session's last
        // resolved focus named A's id (a foreign save's arena legit
        // holds a different vessel on that id — the boundary cannot
        // tell the ids apart, which is the collision under test).
        view.world.resource_mut::<flight::FlightUiData>().focus = Some(a.0);
        view.load_game(snap).expect("load");
        assert_eq!(
            view.resolved_focus(),
            Some(b),
            "the restored record resolves immediately, not the stale mirror"
        );
        // Two steps: the first runs `sync_focus_commands`; the second
        // drains whatever it issued into the §7.6 log — which the
        // restored world starts EMPTY, so every post-load issue is
        // visible here.
        for _ in 0..2 {
            view.begin_frame();
            view.sim_step(view.sim.clock().ticks() + 1).expect("step");
            view.end_frame();
        }
        let focus_cmds: Vec<&SimCommand> = view
            .sim
            .command_log()
            .iter()
            .map(|(_, c)| c)
            .filter(|c| {
                matches!(
                    c,
                    SimCommand::SetVesselTracked { .. } | SimCommand::SetActiveVessel { .. }
                )
            })
            .collect();
        assert!(
            focus_cmds.is_empty(),
            "the restored track/active records already match the pin — \
             the boundary must restamp nothing: {focus_cmds:?}"
        );
        assert_eq!(view.sim.active_vessel(), Some(b));
    }

    /// PLAN §11.80: `launch_craft` captures the pre-launch snapshot;
    /// `revert_to_launch` restores IT — the reverted world is back at
    /// the captured tick with the captured hash, not merely "a
    /// coherent world". The demo seeds its starter craft through
    /// `launch_craft`, so a revert point exists from boot — and the
    /// seed's own `AwaitingDelivery` arm resolves on the first real
    /// `sim_step`, moving the point to the post-delivery world (the
    /// seed delivered through direct `sim.step_frame` calls the
    /// matcher cannot see, so that step is the first boundary it can).
    #[test]
    fn revert_to_launch_restores_pre_launch_snapshot() {
        let mut view = View::new_demo().unwrap();
        let revert_tick = view.revert_tick().expect("demo seeds a revert point");
        let tick0 = view.sim.clock().ticks();
        view.begin_frame();
        view.sim_step(view.sim.clock().ticks() + 16).unwrap();
        view.end_frame();
        assert!(view.sim.clock().ticks() > tick0);
        // The recapture lands at this step's reached tick — the first
        // post-delivery boundary the session observed. The pre-order
        // capture (the empty, unspent world) must NOT be the point.
        let delivered_tick = view.sim.clock().ticks();
        assert_ne!(delivered_tick, revert_tick);
        assert_eq!(view.revert_tick(), Some(delivered_tick));
        view.revert_to_launch().expect("revert");
        // The reverted world is AT the captured tick — the snapshot
        // the delivery boundary re-took, not the live world's state.
        assert_eq!(view.sim.clock().ticks(), delivered_tick);
        // And it is itself a coherent restorable world — re-save and
        // re-load lands the same hash.
        let hash = view.sim.state_hash().unwrap();
        let save = view.save_game().unwrap();
        let bytes = save::encode_save(&save).unwrap();
        let decoded = save::decode_save(&bytes).unwrap();
        view.load_game(decoded.snapshot).unwrap();
        assert_eq!(view.sim.state_hash().unwrap(), hash);
    }

    /// C21-F1: a parked build occupying the pad must not kill the
    /// revert point — `launch_craft` captures it BEFORE enqueueing the
    /// `RecoverVessel` that clears the berth, so the §7.4
    /// "commands pending at serialization boundary" refusal can never
    /// fire on this path.
    #[test]
    fn parked_build_launch_keeps_revert_available() {
        let mut view = View::new().expect("data/ load");
        view.seed_starter_craft().expect("starter seed");
        view.begin_frame();
        view.sim_step(view.sim.clock().ticks() + 8).unwrap();
        view.end_frame();
        assert_eq!(view.sim.vessels().len(), 1, "the seed parked one craft");

        // LAUNCH with the pad occupied: the occupant's RecoverVessel
        // enqueue used to poison the snapshot capture that followed it.
        view.launch_craft().expect("launch places the order");
        assert!(
            view.revert_available(),
            "a parked-build launch still holds a revert point"
        );
        assert!(view.revert_reason().is_none());

        // …and the point survives the order drain intact.
        view.begin_frame();
        let report = view
            .sim_step(view.sim.clock().ticks() + 1)
            .expect("advance");
        assert!(report.rejected.is_empty(), "{:?}", report.rejected);
        view.end_frame();
        assert!(view.revert_available());
    }

    /// C21-F3: the revert point lives at the DELIVERY boundary
    /// (`manufacture.rs`'s post-delivery contract — "a retry of the
    /// flying", PLAN:1180), not the pre-order world: a revert after
    /// the launch berths keeps the paid craft latched on the pad and
    /// debits nothing again. `LaunchCraft`'s boundary IS its drain
    /// tick — the berth lands in the same apply the spend does. The
    /// pad is cleared first (empty-berth launch) so the funds delta
    /// is the launch's own debit — a launch-site `RecoverVessel`
    /// refunds at 1.0 efficiency and would net the comparison ~zero.
    #[test]
    fn revert_after_delivery_does_not_repay_the_build() {
        let mut view = View::new().expect("data/ load");
        view.seed_starter_craft().expect("starter seed");
        // Free the berth so this launch carries no recovery credit.
        view.sim.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::TerminateVessel {
                vessel: csp_sim::VesselId(0),
            },
        );
        view.begin_frame();
        let report = view
            .sim_step(view.sim.clock().ticks() + 1)
            .expect("advance");
        assert!(report.rejected.is_empty(), "{:?}", report.rejected);
        view.end_frame();
        let funds0 = view.sim.funds_micros();

        view.launch_craft().expect("launch issues LaunchCraft");
        // The interim point is the pre-ORDER capture — its tick is
        // before the launch drains, which is what makes the delivery
        // boundary assertion below non-vacuous.
        let order_tick = view.revert_tick().expect("launch arms a point");
        view.begin_frame();
        let report = view
            .sim_step(view.sim.clock().ticks() + 1)
            .expect("advance");
        assert!(report.rejected.is_empty(), "{:?}", report.rejected);
        view.end_frame();
        assert!(
            view.sim.builds().is_empty(),
            "a launch mints no build order"
        );
        let berth_tick = view.sim.clock().ticks();
        assert!(berth_tick > order_tick);

        // The delivery boundary: the recaptured point IS the drain
        // tick that berthed the craft, and the world it holds is the
        // paid one — funds debited once, the craft parked, no build
        // left to wait out.
        assert_eq!(
            view.revert_tick(),
            Some(berth_tick),
            "the revert point moved to the berth boundary"
        );
        let funds_paid = view.sim.funds_micros();
        assert!(funds_paid < funds0, "the launch debited the craft");
        view.revert_to_launch().expect("revert");
        assert_eq!(view.sim.clock().ticks(), berth_tick);
        assert_eq!(
            view.sim.funds_micros(),
            funds_paid,
            "revert restores the paid world — no second debit, no refund"
        );
        assert!(
            view.sim.builds().is_empty(),
            "revert must not mint a build wait"
        );
        assert_eq!(
            view.sim.vessels().len(),
            1,
            "the berthed craft is what the revert retries"
        );
    }

    /// C21-F5 + C22-F1: `load_game` clears the revert point — a revert
    /// on a loaded world can never resurrect the prior session's
    /// launch snapshot over it.
    #[test]
    fn load_game_drops_the_prior_sessions_revert_point() {
        let mut view = View::new().expect("data/ load");
        view.seed_starter_craft().expect("starter seed");
        view.begin_frame();
        view.sim_step(view.sim.clock().ticks() + 1).unwrap();
        view.end_frame();
        let save = view.save_game().expect("save at drained boundary");

        view.launch_craft().expect("launch A places the order");
        assert!(view.revert_available(), "launch A armed a point");

        view.load_game(save.snapshot).expect("load B");
        assert!(
            !view.revert_available(),
            "the loaded world carries no revert point"
        );
        assert!(
            view.revert_to_launch().is_err(),
            "revert refuses — session B never launched"
        );
    }

    /// A minimal one-node 10 kg craft for a FOREIGN `ManufactureCraft`
    /// order — deliberately unlike the starter craft so the arm's
    /// `(site, craft)` match cannot confuse the two builds.
    fn tiny_foreign_craft() -> csp_sim::CraftDef {
        let mut graph = csp_phys::PartGraph::new(csp_phys::PartNodeId(0));
        graph.nodes.insert(
            csp_phys::PartNodeId(0),
            csp_phys::PartNode::new(None, csp_phys::PartTransform::IDENTITY, 0),
        );
        csp_sim::CraftDef {
            part_build_resources: Vec::new(),
            hash_memo: Default::default(),
            part_ids: vec![],
            graph,
            parts: vec![csp_phys::WeldPart {
                experiments: Vec::new(),
                lab: None,
                engine: None,
                parachute: None,
                dock_ports: vec![],
                tanks: vec![],
                crew_seats: 0,
                structural: csp_data::StructuralDef::unlimited(),
                mass_kg: 10.0,
                com: csp_core::DVec3::ZERO,
                inertia: csp_phys::SymMat3::solid_box(10.0, csp_core::DVec3::new(0.5, 0.5, 0.5)),
                collider: csp_phys::ColliderShape::Box {
                    half_extents: csp_core::DVec3::new(0.5, 0.5, 0.5),
                },
                force_sites: vec![],
                aero: csp_data::AeroDef::constant(0.0, 0.0, 1.0),
                thermal: csp_data::ThermalDef {
                    heat_capacity_j_per_k: 10_000.0,
                    max_temp_k: 1_000.0,
                    emissivity: 0.8,
                    conduction_w_per_k: 10.0,
                },
            }],
            part_costs: vec![0.0],
            reliability: vec![],
        }
    }

    /// The dominant body's authored launch site — the same lookup
    /// `launch_craft` performs, for enqueuing a foreign order to the
    /// same pad.
    fn launch_site_id(view: &View) -> csp_data::SiteId {
        let dominant = view.sim.dominant_body().expect("dominant body").clone();
        view.sim
            .config()
            .sites
            .into_values()
            .find(|s| s.body == dominant && s.launch)
            .expect("authored launch site")
            .id
    }

    /// The recapture must not be moved by a foreign delivery: the
    /// armed `LaunchCraft` berths in its own drain — the arm resolves
    /// to `Delivered` and the point moves to THAT boundary — so a
    /// foreign `ManufactureCraft` (the server ingress enqueues on
    /// `SourceClass::External`) still building at the same site
    /// delivers later and must leave the point alone.
    #[test]
    #[allow(clippy::too_many_lines)] // one boundary per arrange/act/assert beat
    fn revert_recapture_ignores_a_foreign_builds_delivery() {
        let mut view = View::new().expect("data/ load");
        view.seed_starter_craft().expect("starter seed");
        let site_id = launch_site_id(&view);
        // Free the berth — the launch must be able to berth instantly
        // (a parked occupant defers a later delivery under the
        // single-berth rule, which is not what this test exercises).
        view.sim.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::TerminateVessel {
                vessel: csp_sim::VesselId(0),
            },
        );
        // This first real step also resolves the SEED launch's arm: its
        // `LaunchCraft` applied inside the seed's direct `step_frame`,
        // so the arm flips to `Delivered` and recaptures.
        view.begin_frame();
        let report = view
            .sim_step(view.sim.clock().ticks() + 1)
            .expect("advance");
        assert!(report.rejected.is_empty(), "{:?}", report.rejected);
        view.end_frame();
        assert!(view.sim.vessels().is_empty(), "berth is free");

        view.launch_craft().expect("launch issues LaunchCraft");
        let order_tick = view.revert_tick().expect("launch arms a point");
        // The foreign order: a different, much lighter craft on a
        // different channel — enqueued AFTER the arm, drained at the
        // same boundary, but still a RESERVATION: it builds while our
        // craft is already parked. C11-F4: it must
        // pass the bill check like every order — a real catalog
        // identity with its authored cost/BOM (anonymous hulls reject
        // in a catalog world now). `Discrete`-scale adapter: stats
        // stay authored, so the declared figures are verbatim.
        let adapter = view
            .sim
            .parts()
            .get(&csp_data::PartId::from("csp.structure.adapter"))
            .expect("catalog adapter")
            .clone();
        let mut foreign = tiny_foreign_craft();
        foreign.part_ids = vec![adapter.id.clone()];
        foreign.part_costs = vec![adapter.cost_funds];
        view.sim.enqueue(
            SourceClass::External,
            7,
            SimCommand::ManufactureCraft {
                site: site_id,
                craft: foreign,
                build_resources: adapter.build_resources.clone(),
            },
        );
        view.begin_frame();
        let report = view
            .sim_step(view.sim.clock().ticks() + 1)
            .expect("advance");
        assert!(report.rejected.is_empty(), "{:?}", report.rejected);
        view.end_frame();
        // The launch berthed instantly; only the foreign build is
        // still in flight.
        assert_eq!(
            view.sim.builds().len(),
            1,
            "only the foreign reservation is still building"
        );
        assert_eq!(
            view.sim.vessels().len(),
            1,
            "the launch berthed its craft in the drain"
        );
        let foreign_complete = view
            .sim
            .builds()
            .values()
            .next()
            .expect("the foreign record")
            .complete_tick;
        let berth_tick = view.sim.clock().ticks();
        // The arm already resolved at the launch's own drain — the
        // point moved off the pre-order world to the berth boundary.
        assert_eq!(
            view.revert_tick(),
            Some(berth_tick),
            "the recapture lands at the ARMED launch's berth tick"
        );
        assert_ne!(berth_tick, order_tick);
        assert!(
            view.revert_capture.is_none(),
            "the instant berth consumed the arm"
        );

        // Clear our berthed craft so the foreign delivery can land,
        // then step to its deadline: the foreign `ConstructionComplete`
        // fires — and the revert point does NOT move again.
        let ours = *view.sim.vessels().keys().next().expect("the berthed craft");
        view.sim.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::TerminateVessel { vessel: ours },
        );
        view.begin_frame();
        let report = view
            .sim_step(view.sim.clock().ticks() + 1)
            .expect("advance");
        assert!(report.rejected.is_empty(), "{:?}", report.rejected);
        view.end_frame();
        view.begin_frame();
        let report = view.sim_step(foreign_complete).expect("advance");
        assert!(
            report
                .fired_events
                .iter()
                .any(|e| matches!(e.kind, csp_sim::EventKind::ConstructionComplete { .. })),
            "the foreign build delivered"
        );
        view.end_frame();
        assert_eq!(
            view.revert_tick(),
            Some(berth_tick),
            "a foreign build's delivery must not move the armed launch's point"
        );
        assert!(view.sim.builds().is_empty(), "no build left pending");
        assert_eq!(
            view.sim.vessels().len(),
            1,
            "only the foreign build's craft parks now"
        );
    }

    /// The arm must not outlive its order: a `LaunchCraft` that
    /// rejects at the drain mints no vessel, so `AwaitingDelivery`
    /// would wait forever
    /// — and latch onto an unrelated later firing. Once the consuming
    /// boundary has run without a matching `command_log` entry the arm
    /// is gone and the pre-order point stands.
    #[test]
    fn revert_arm_disarms_when_the_order_rejects_at_drain() {
        let mut view = View::new().expect("data/ load");
        view.seed_starter_craft().expect("starter seed");
        view.begin_frame();
        view.sim_step(view.sim.clock().ticks() + 1)
            .expect("advance");
        view.end_frame();

        // Arm exactly what `launch_craft` installs, then queue a
        // launch the apply must reject (an unknown launch site). The
        // arm and the command carry the same `(site, craft)` — the
        // match key — so only the reject outcome is exercised.
        let arm_site = csp_data::SiteId::from("csp.test.no-such-site");
        let arm_craft = tiny_foreign_craft();
        view.pre_launch = Some(Ok(view.sim.snapshot().expect("snapshot at rest")));
        view.revert_capture = Some(RevertCapture::AwaitingDelivery {
            site: arm_site.clone(),
            craft: arm_craft.clone(),
            min_build_id: 0,
            armed_tick: view.sim.clock().ticks(),
        });
        view.sim.enqueue(
            SourceClass::Ui,
            PLAYER_SOURCE_ID,
            SimCommand::LaunchCraft {
                site: arm_site,
                craft: arm_craft,
                build_resources: vec![],
            },
        );
        view.begin_frame();
        let report = view
            .sim_step(view.sim.clock().ticks() + 1)
            .expect("advance");
        assert!(
            !report.rejected.is_empty(),
            "the bogus order must reject at the drain"
        );
        view.end_frame();
        assert!(
            view.revert_capture.is_none(),
            "a rejected order must not leave the arm armed"
        );
        assert!(
            view.revert_available(),
            "the pre-order revert point survives the disarm"
        );
        assert!(
            view.world
                .resource::<flight::FlightSfx>()
                .cues
                .iter()
                .any(|c| c.text.contains("BUILD ORDER REFUSED")),
            "the disarm surfaces on the cue channel"
        );
    }

    /// C19-F4 regression: the chase camera reframes on a focus-id
    /// change — a zoom held on one vessel must not carry to the next.
    /// Changed with the eased camera: the reframe sets the TARGET
    /// distance (the old test pinned a snap of the camera to 60 m; it
    /// now eases there, and the frame of the change moves nothing).
    #[test]
    fn camera_reframes_on_focus_change() {
        let mut view = View::new().expect("data/ load");
        let (center, r) = {
            let env = view.sim.environment().expect("physics");
            (env.body_center, env.radius_m)
        };
        // Two parked vessels; the newest (id 1) takes focus.
        for i in 0..2 {
            view.enqueue_command(
                SourceClass::Ui,
                PLAYER_SOURCE_ID,
                SimCommand::SpawnPhysicalVessel {
                    craft: stock_flight_craft(),
                    pos: center + csp_core::DVec3::new(r + 5.0 + i as f64 * 20.0, 0.0, 0.0),
                    rot: csp_core::DQuat::IDENTITY,
                    vel: csp_core::DVec3::ZERO,
                    ang_vel: csp_core::DVec3::ZERO,
                },
            );
        }
        view.begin_frame();
        view.sim_step(1).expect("step");
        view.end_frame();
        let focus = view.flight_focus().expect("a vessel is focused");
        fn radius_of(view: &View, vessel: csp_sim::VesselId) -> f64 {
            view.world
                .resource::<flight::CraftRadii>()
                .0
                .iter()
                .find(|(v, _)| *v == vessel.0)
                .expect("every staged vessel carries its radius")
                .1
        }
        let first = radius_of(&view, focus);
        // First focus: the target is the craft's frame distance.
        assert_eq!(
            view.world
                .resource::<flight::FlightInput>()
                .distance_target_m(),
            (4.0 * first).max(3.0).max(1.5 * first),
            "first focus frames the craft at four radii, clamped to the limits"
        );
        // Zoom in, then remove the focused vessel — focus moves.
        view.flight_event(flight::FlightEvent::Zoom { factor: 0.05 });
        view.begin_frame();
        view.end_frame();
        let input = view.world.resource::<flight::FlightInput>();
        assert_eq!(
            input.distance_target_m(),
            3.0_f64.max(1.5 * first),
            "the zoom applies to the target and stops at the distance floor"
        );
        // No frame time is supplied here, so the camera is still where
        // it started and nothing eases.
        let (was_at, was_eye, was_view) = {
            let cam = &view.world.resource::<flight::FlightCameraF64>().0;
            (input.distance_m(), cam.eye, cam.view_proj)
        };
        view.enqueue_command(
            SourceClass::Ui,
            PLAYER_SOURCE_ID,
            SimCommand::TerminateVessel { vessel: focus },
        );
        view.begin_frame();
        view.sim_step(view.sim.clock().ticks() + 1).expect("step");
        view.end_frame();
        assert_ne!(view.flight_focus(), Some(focus));
        let next = view.flight_focus().expect("the other vessel takes focus");
        let next_radius = radius_of(&view, next);
        let input = view.world.resource::<flight::FlightInput>();
        assert_eq!(
            input.distance_target_m(),
            (4.0 * next_radius).max(3.0).max(1.5 * next_radius),
            "a focus-id change must reframe to the new vessel's frame distance"
        );
        assert_eq!(
            input.distance_m(),
            was_at,
            "the distance the camera is at does not snap — it eases"
        );
        let cam = &view.world.resource::<flight::FlightCameraF64>().0;
        assert_eq!(
            (cam.eye, cam.view_proj),
            (was_eye, was_view),
            "the frame of the focus change moves neither the eye nor the view"
        );
    }

    /// C19-F2 regression: the focused vessel's bubble→rails
    /// conversion keeps the camera/HUD on it — the vessel stages
    /// through the handoff hint, the cue reads "ON RAILS" (never
    /// "VESSEL REMOVED"), and the focus boundary tracks + records it
    /// (C02-F1/C02-F2: `tracked_vessels` + `active_vessel`).
    #[allow(clippy::too_many_lines)] // one end-to-end regression: spawn → powered hold → coast → rails → cue → save→load
    #[test]
    fn auto_rails_keeps_focus_and_marks_the_cue() {
        let mut view = View::new().expect("data/ load");
        let (center, r, mu, atm_top) = {
            let env = view.sim.environment().expect("physics");
            (
                env.body_center,
                env.radius_m,
                env.mu_m3_per_s2,
                env.atmosphere.as_ref().map_or(0.0, |a| a.top_m),
            )
        };
        // A clearing circular orbit above the atmosphere top — the
        // auto-rails predicate (coasting, conic clears surface + atm)
        // converts it the first tick it coasts. Throttle up first so
        // the vessel spends real frames InBubble and focused — the
        // conversion mid-flight is the handoff under test, not an
        // instant spawn→rails.
        let alt = atm_top.max(200_000.0) + 50_000.0;
        let orb_r = r + alt;
        let speed = (mu / orb_r).sqrt();
        view.enqueue_command(
            SourceClass::Ui,
            PLAYER_SOURCE_ID,
            SimCommand::SpawnPhysicalVessel {
                craft: stock_flight_craft(),
                pos: center + csp_core::DVec3::new(orb_r, 0.0, 0.0),
                rot: csp_core::DQuat::IDENTITY,
                vel: csp_core::DVec3::new(0.0, speed, 0.0),
                ang_vel: csp_core::DVec3::ZERO,
            },
        );
        view.enqueue_command(
            SourceClass::Ui,
            PLAYER_SOURCE_ID,
            SimCommand::SetThrottle {
                vessel: csp_sim::VesselId(0),
                throttle: 1.0,
            },
        );
        for _ in 0..4 {
            view.begin_frame();
            view.sim_step(view.sim.clock().ticks() + 1).expect("step");
            view.end_frame();
        }
        let id = view
            .flight_focus()
            .expect("powered vessel staged + focused");
        // Cut to coast: the clearing conic converts next tick.
        view.enqueue_command(
            SourceClass::Ui,
            PLAYER_SOURCE_ID,
            SimCommand::SetThrottle {
                vessel: id,
                throttle: 0.0,
            },
        );
        let mut on_rails_at = None;
        for i in 0..20 {
            view.begin_frame();
            view.sim_step(view.sim.clock().ticks() + 1).expect("step");
            view.end_frame();
            if matches!(
                view.sim.vessels().values().next().map(|v| &v.state),
                Some(csp_sim::VesselState::OnRails(_))
            ) {
                on_rails_at = Some(i);
                break;
            }
        }
        assert!(on_rails_at.is_some(), "the vessel converted to rails");
        // Focus held through the conversion — the vessel still stages
        // (as rails), the camera stays valid, the HUD tracks it.
        assert_eq!(
            view.flight_focus(),
            Some(id),
            "focus must follow the railed vessel"
        );
        assert!(
            view.world
                .resource::<flight::RailsStaged>()
                .0
                .contains(&id.0),
            "the staged focus is marked rails-resident"
        );
        let cues: Vec<String> = view
            .world
            .resource::<flight::FlightSfx>()
            .cues
            .iter()
            .map(|c| c.text.clone())
            .collect();
        assert!(
            cues.iter().any(|c| c == "ON RAILS"),
            "the conversion cue fired: {cues:?}"
        );
        assert!(
            !cues.iter().any(|c| c == "VESSEL REMOVED"),
            "a live railed vessel must not read as removed: {cues:?}"
        );
        // C02-F1/C02-F2: the focus boundary landed the sim-side records.
        view.begin_frame();
        view.sim_step(view.sim.clock().ticks() + 1).expect("step");
        view.end_frame();
        assert!(
            view.sim.tracked_vessels().contains(&id),
            "focused ⇒ tracked"
        );
        assert_eq!(
            view.sim.active_vessel(),
            Some(id),
            "focused ⇒ active_vessel"
        );
        // And the record survives a save→load — the camera re-anchors
        // on it instead of the newest-spawn fallback.
        let snap = view.sim.snapshot().expect("serializable");
        view.load_game(snap).expect("load");
        view.begin_frame();
        view.sim_step(view.sim.clock().ticks() + 1).expect("step");
        view.end_frame();
        assert_eq!(
            view.flight_focus(),
            Some(id),
            "the restored active_vessel re-anchors the flight focus"
        );
        assert_eq!(view.sim.active_vessel(), Some(id));
    }

    /// C19-F2 cue fix, other half: a vessel that actually LEAVES the
    /// arena still reports "VESSEL REMOVED" — the live-count edge is
    /// honest in both directions.
    #[test]
    fn removal_still_fires_vessel_removed() {
        let mut view = View::new().expect("data/ load");
        let (center, r) = {
            let env = view.sim.environment().expect("physics");
            (env.body_center, env.radius_m)
        };
        for i in 0..2 {
            view.enqueue_command(
                SourceClass::Ui,
                PLAYER_SOURCE_ID,
                SimCommand::SpawnPhysicalVessel {
                    craft: stock_flight_craft(),
                    pos: center + csp_core::DVec3::new(r + 5.0 + i as f64 * 20.0, 0.0, 0.0),
                    rot: csp_core::DQuat::IDENTITY,
                    vel: csp_core::DVec3::ZERO,
                    ang_vel: csp_core::DVec3::ZERO,
                },
            );
        }
        view.begin_frame();
        view.sim_step(1).expect("step");
        view.end_frame();
        // Remove the NON-focused vessel (the older id): a live-count
        // drop with unchanged debris — the "VESSEL REMOVED" edge.
        let gone = *view.sim.vessels().keys().next().expect("older vessel");
        view.enqueue_command(
            SourceClass::Ui,
            PLAYER_SOURCE_ID,
            SimCommand::TerminateVessel { vessel: gone },
        );
        view.begin_frame();
        view.sim_step(view.sim.clock().ticks() + 1).expect("step");
        view.end_frame();
        let cues: Vec<String> = view
            .world
            .resource::<flight::FlightSfx>()
            .cues
            .iter()
            .map(|c| c.text.clone())
            .collect();
        assert!(
            cues.iter().any(|c| c == "VESSEL REMOVED"),
            "a real removal still cues: {cues:?}"
        );
    }
}
