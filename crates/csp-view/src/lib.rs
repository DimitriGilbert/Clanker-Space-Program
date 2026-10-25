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
use csp_sim::World as SimWorld;

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
/// fingerprint fields (§2.14) and §7.7's drift-warning key.
pub use csp_data::{DATA_HASH_VERSION, datahash::data_set_hash};
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
// Re-exported so `csp-app` names mode requests without a csp-ui edge.
pub use csp_ui::ModeSelect;
/// M10: the settings model + panel (`csp_ui::settings`) — re-exported so
/// `csp-app` owns the file IO without a `csp-ui` edge of its own.
pub use csp_ui::settings;

/// Integer sim clock mirrored for presentation — a COPY, written each frame
/// by the `sync` schedule's [`sync_mirrors`] system (PLAN §2.2, Appendix A
/// item 1). Never authoritative.
#[derive(Resource, Default)]
pub struct Clock(pub SimTime);

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
    /// The settings panel's "Apply" — `csp-app` copies the staged
    /// settings into its live set and persists them.
    pub settings_apply: bool,
}

/// The maneuver panel's session state — `csp-view` owns it (§2.2 C),
/// `csp_ui::maneuver_panel` mutates it inside `UiBuild`.
#[derive(Resource, Default)]
pub struct ManeuverPanelRes(pub csp_ui::ManeuverPanelState);

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
}

/// The Sync bridge (§2.5 #3): copy the staged post-step sim values into the
/// presentation-only mirrors. The sole writer of [`Clock`], [`WarpMirror`],
/// and [`FrameStats`].
fn sync_mirrors(
    input: Res<SyncInput>,
    mut clock: ResMut<Clock>,
    mut stats: ResMut<FrameStats>,
    mut warp: ResMut<WarpMirror>,
) {
    clock.0 = input.clock;
    warp.0 = input.warp_level;
    stats.ticks_last_frame = input.ticks_last_frame;
    stats.frames += 1;
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
    map_ui: Res<map::MapUiData>,
    vab_input: Res<vab::VabInput>,
    mut vab: ResMut<vab::VabSessionRes>,
    craft_files: Res<vab::CraftFileList>,
    flight_ui: Res<flight::FlightUiData>,
    map_snap: Res<map::MapSnapshot>,
    mut maneuver_state: ResMut<ManeuverPanelRes>,
    mut settings_state: ResMut<SettingsPanelRes>,
    mut panels: ResMut<PanelFlags>,
    mut tutorial: ResMut<tutorial::TutorialRes>,
    mut outbox: ResMut<UiOutbox>,
) {
    let mode = if vab_input.open {
        csp_ui::ModeSelect::Vab
    } else if map_ui.mode == map::MapMode::Map {
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
        warp_level: warp.0,
        warp_requested: session.warp_requested,
        warp_effective: session.warp_effective,
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
        if panels.debug && mode != csp_ui::ModeSelect::Vab {
            csp_ui::debug_panel(ctx, &input, &mut panel_output);
        }
        if panels.help {
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
                panels.help = false;
            }
        }
        if map_ui.mode == map::MapMode::Map {
            map_panels(
                ctx,
                &map_ui,
                &map_snap,
                clock.0.ticks(),
                &session,
                &mut maneuver_state.0,
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
                    propellant: &flight_ui.propellant,
                    stages_left: flight_ui.stages_left,
                    stages_total: flight_ui.stages_total,
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
        // M10: the settings window + the §2.13 Fatal dialog + the
        // tutorial checklist — session-state panels, drawn every mode.
        ui_masks.extend(m10_panels(
            ctx,
            &mut settings_state.0,
            &session,
            &mut outbox,
            &mut tutorial.0,
            mode == csp_ui::ModeSelect::Vab,
        ));
        // The §3.7 cue stack — the GLOBAL refusal/transition channel.
        // Drawn in every mode: a refusal that can only flash inside
        // `flight_hud` is invisible exactly where it fires (Map
        // recover-control, VAB launch, warp demotion) — the dead-click
        // complaint. `flight_ui.sfx` is the Presentation-mirrored live
        // stack; `flight_present` ages the TTLs every frame, so cues
        // pushed from any mode still fade on schedule.
        csp_ui::notice_overlay(ctx, &flight_ui.sfx);
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
        panels.help = !panels.help;
    }
    // Warp steps may come from the debug panel's buttons or the top
    // bar's −/+ now that the scaffold is dev-gated.
    outbox.warp_step = panel_output.warp_step + vab_out.warp_step;
    outbox.mode = vab_out.mode;
    outbox.launch = vab_out.launch;
    // Save carries the serialized bytes — the app owns the file store.
    // The `if let` keeps `to_bytes` (a bincode pass over the session)
    // behind the actual save request — `Option::zip` would serialize on
    // every VAB frame.
    outbox.craft_save = if let Some(name) = vab_out.save_name {
        vab.0.to_bytes().ok().map(|bytes| (name, bytes))
    } else {
        None
    };
    outbox.craft_load = vab_out.load_name;
}

/// The map mode's UiBuild panels — the M1 overlay + the M5 node panel
/// (its `SetManeuver`/`DeleteManeuver` output rides the outbox).
/// Extracted from [`build_ui`] under the line budget.
fn map_panels(
    ctx: &egui::Context,
    map_ui: &map::MapUiData,
    map_snap: &map::MapSnapshot,
    tick: u64,
    session: &SessionMirror,
    maneuver_state: &mut csp_ui::ManeuverPanelState,
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
            .filter(|(.., on_rails)| *on_rails)
            .map(|(id, _, n, _)| csp_ui::ManeuverVessel {
                id: id.0,
                node: n.as_ref().map(|n| (n.tick, n.dv_magnitude())),
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
    });
    if !errors.is_empty() {
        return Err(ViewError::Validation(errors));
    }
    Ok(d)
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
        Self::from_source(&csp_data::FsDataSource, &data_root())
    }

    /// The M10-C shared half: load a `data/` tree from any
    /// [`csp_data::DataSource`] and wire the sim world + presentation
    /// resources from it. `View::new` (native) reads the filesystem;
    /// [`View::new_demo`] reads the §6.3 embedded set — identical wiring,
    /// identical file-shape checks, different source.
    fn from_source(
        src: &dyn csp_data::DataSource,
        root: &std::path::Path,
    ) -> Result<Self, ViewError> {
        // §8.2: load the whole `data/` tree up front and run the
        // semantic validator BEFORE anything reaches the sim — the
        // same sweep the data tests run, so an invalid tree refuses
        // loudly at startup instead of shipping silently (the
        // loaders' "the validator flags it loudly" docs, made true).
        let d = load_validated_dataset(src, root)?;
        let (sys, balance, experiments, techtree, sites, contracts) = (
            d.system,
            d.balance,
            d.experiments,
            d.techtree,
            d.sites,
            d.contracts,
        );
        let (recipes, colonies, parts, resources, loc) =
            (d.recipes, d.colonies, d.parts, d.resources, d.loc);
        let defs: Vec<csp_data::BodyDef> = sys.bodies.iter().map(|b| b.def.clone()).collect();
        let orbits =
            csp_sim::orbit::PatchedConics::from_body_defs(&defs).map_err(ViewError::Sim)?;
        // M4c presentation floor: the game world runs the physics
        // bubble over the home planet (same `Environment` convention
        // `csp-bench`'s flight scenarios use).
        let terrin = defs
            .iter()
            .find(|b| b.id.as_str() == "terrin")
            .ok_or(ViewError::Sim(SimError::Fatal(
                "home system has no 'terrin' body",
            )))?;
        let mut sim = SimWorld::with_physics(
            orbits,
            csp_sim::PhysicsConfig {
                environment: csp_phys::Environment::for_body(
                    terrin,
                    csp_core::DVec3::ZERO,
                    terrin.rotation_period_s,
                ),
                dominant: terrin.id.clone(),
            },
            balance.advance_to_max_events_per_frame,
        );
        sim.set_vessel_caps(csp_sim::VesselCaps {
            max_vessels: balance.max_tracked_vessels,
            max_debris: balance.max_debris,
        });
        // M9a §2.10: the script host + autopilot driver (construction
        // config — re-installed identically by csp-bench's replay
        // harness) and the per-save op budget.
        sim.set_script_op_budget(balance.script_op_budget);
        sim.set_script_engine(Box::new(csp_script::RhaiHost::new()));
        sim.set_autopilot_driver(Box::new(csp_ai::AscentAutopilot::new()));
        sim.set_docking_config(csp_sim::DockingConfig::from_balance(&balance))
            .map_err(ViewError::Sim)?;
        sim.set_transfer_config(csp_sim::TransferConfig::from_balance(&balance));
        // M9b §3.4: the reliability tunables — dormant scale,
        // ignition window, effect magnitudes, test-stand economics.
        sim.set_reliability_config(csp_sim::ReliabilityConfig::from_balance(&balance));
        // M7-A §3.2: the science tunables and the experiment-def table —
        // same data-derived wiring (`csp-bench`'s scenario builder does
        // the identical three calls); without them science commands
        // would run on the no-data defaults and reject every run.
        sim.set_science_config(csp_sim::ScienceConfig::from_balance(&balance));
        sim.set_experiments(
            experiments
                .by_id()
                .into_iter()
                .map(|(id, def)| (id, def.clone()))
                .collect(),
        );
        // M7-B §3.3: the merged tech tree + the part catalog the
        // `CraftDef::part_ids` gate resolves requirements against —
        // same data-derived wiring; without them research commands
        // reject with "no tech tree" and gated parts cannot resolve.
        sim.set_tech_tree(techtree.merged());
        sim.set_parts(
            parts
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
        sim.set_economy_config(csp_sim::economy::EconomyConfig::from_balance(&balance))
            .map_err(ViewError::Sim)?;
        // The site's table collects first-wins on a duplicate id —
        // the `by_id()` convention (`or_insert`), not the `collect`
        // last-wins it replaced (the validator flags the duplicate
        // loudly either way; the tie-break just has to be one rule).
        sim.set_sites(
            sites
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
            contracts
                .by_id()
                .into_iter()
                .map(|(id, def)| (id, def.clone()))
                .collect(),
        );
        sim.set_contract_config(csp_sim::ContractConfig::from_balance(&balance));
        // M8-A §3.5.1: the per-body colony environments — rotation
        // and authored insolation from the loaded body defs (the
        // `set_sites` precedent).
        sim.set_colony_env(csp_sim::colony_env_from_body_defs(&defs));
        // M8-C §3.5: the life-support tunables — same data-derived
        // wiring (the `set_economy_config` precedent); without it the
        // check cadence/rates run on the no-data `DEFAULT_*` mirrors.
        // The per-save flag itself is sim state, not config — it
        // stays opt-in (`World::new` starts disabled).
        sim.set_life_support_config(csp_sim::LifeSupportConfig::from_balance(&balance));
        // M8-B §3.5.1: the recipe table — the same data-derived
        // channel; `FoundColony`'s Converter modules resolve
        // `RecipeId`s against it.
        sim.set_recipes(
            recipes
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
        sim.found_authored_colonies(&colonies.colonies)
            .map_err(ViewError::Sim)?;
        let mut view = Self::with_sim(sim);
        // M3 VAB: the part catalog + resource registry are data-derived
        // constants for the session model (like BodyTable — §7.3's
        // re-derived-on-load class).
        view.install_vab_catalog(parts, resources);
        // The catalog's display-name half: `PartDef::display_name` is a
        // `LocKey`, so the English table is what lets the palette print
        // "Sparc Launch Engine" instead of the raw part id — same
        // data-derived-constant class as the catalog itself.
        view.with_vab_session(|v| v.set_loc(loc));
        // R2: the authored terrain table (noise seeds, ocean datums,
        // decoded biome palettes) — installed over the empty default.
        let art = art::load();
        let mut tc = terrain::config_from_bodies(&sys.bodies);
        tc.art = art.0.terrain.clone();
        view.world.insert_resource(tc);
        view.world.insert_resource(art);
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
        let mut view = Self::from_source(&demo::source(), std::path::Path::new(demo::DEMO_ROOT))?;
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
    pub fn with_sim(sim: SimWorld) -> Self {
        let mut world = World::new();
        world.insert_resource(Clock::default());
        world.insert_resource(FrameStats::default());
        world.insert_resource(WarpMirror::default());
        world.insert_resource(SessionMirror::default());
        world.insert_resource(EguiPass::default());
        world.insert_resource(UiOutbox::default());
        world.insert_resource(SyncInput::default());

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
        // M10: the settings window's staged state (§2.2-C session data).
        world.insert_resource(SettingsPanelRes::default());
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
        // The non-debris vessel count across all residences — the
        // tutorial's alive-or-lost mirror (P12-F2).
        world.insert_resource(flight::LiveVesselCount::default());
        // The propulsion walk mirror — the HUD's live TWR/Δv/
        // burn-time figures (same standalone-resource pattern as
        // `LiveVesselCount`: `FlightSnapshot` cannot grow).
        world.insert_resource(flight::VesselPropulsionStaged::default());
        world.insert_resource(flight::FlightUiData::default());
        world.insert_resource(flight::StagePlan::default());
        world.insert_resource(flight::FlightFrame::default());
        // §2.6.4: the latched-vessel weld cache — the staging path's
        // session state, so a parked craft isn't re-welded per frame.
        world.insert_resource(flight::LatchedWeldCache::default());
        // R2 surface resources (see terrain.rs): empty config ⇒
        // per-body params synthesised from the body id; `View::new`
        // installs the authored table over it.
        world.insert_resource(terrain::TerrainConfig::default());
        world.insert_resource(terrain::TerrainCache::default());
        world.insert_resource(art::ArtAssets::default());
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
        let report = self.sim.step_frame(target_tick)?;
        self.ticks_last_frame = report.reached_tick - start;
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
        self.schedules.sim_step.run(&mut self.world);
        Ok(report)
    }

    /// Once per frame, after SimStep: Sync → Presentation → UiBuild →
    /// RenderExtract (PLAN §2.5 ordering, Appendix A item 2). Stages the
    /// post-step sim values into `SyncInput` first so the mirror writes are
    /// the `sync` schedule's [`sync_mirrors`] system, not bare statements.
    pub fn end_frame(&mut self) {
        self.world.insert_resource(SyncInput {
            clock: self.sim.clock(),
            warp_level: self.sim.warp_level(),
            ticks_last_frame: self.ticks_last_frame,
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
            let art = self.world.resource::<art::ArtAssets>().clone();
            let mut cache = self.world.resource_mut::<flight::LatchedWeldCache>();
            flight::capture_sync_input(&self.sim, &mut cache, &art)
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

    /// M4c launch: turn the VAB working craft into a physically
    /// spawned vessel on the pad and switch to the flight view. The
    /// craft comes from the session's `to_part_graph` +
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
    /// notice nobody reads for a spawn. `Ok(())` means the
    /// `SpawnPhysicalVessel` command is enqueued and the mode is
    /// Flight.
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
                v.analysis().cloned(),
                v.analysis_error().map(str::to_string),
                stage_plan_of(v),
            )
        });
        let (session, analysis, analysis_error, stage_plan) = craft;
        let craft_def: Result<csp_sim::CraftDef, String> = (|| {
            if session.is_empty() {
                return Ok(stock_flight_craft());
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
            let (graph, built) = session.to_part_graph().map_err(|e| e.to_string())?;
            let parts: Vec<csp_phys::WeldPart> = analysis
                .effective
                .iter()
                .map(|e| e.weld_input.clone())
                .collect();
            let part_costs: Vec<f64> = analysis.effective.iter().map(|e| e.cost_funds).collect();
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
            let part_ids: Vec<csp_data::PartId> = built.iter().map(|b| b.part_id.clone()).collect();
            Ok(csp_sim::CraftDef {
                graph,
                parts,
                part_costs,
                reliability,
                part_ids,
            })
        })();
        let craft = match craft_def {
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
        let (lat, lon, site_alt) = site.map_or((0.0, 0.0, 0.0), |s| {
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
        let z_min = candidate
            .collider
            .hulls
            .iter()
            .flat_map(|h| h.vertices.iter().map(|v| v.z))
            .fold(f64::INFINITY, f64::min)
            .min(0.0);
        let com_local = candidate.com_local;
        // The spawn kinematics are the §2.6.4 latch record's exact
        // restore: upright ENU pose at the site, position resolved at
        // the CURRENT tick's spin phase (the pad co-rotates — spawning
        // at the stale t=0 longitude would put the craft downrange of
        // the site), velocity the surface co-rotation `ω × r` of the
        // CoM — a pad-resting craft, not an inertially-dropped one.
        let pose = csp_sim::SurfacePose {
            body: dominant,
            latitude_rad: lat,
            longitude_rad: lon,
            altitude_m: site_alt + PAD_CLEARANCE_M - z_min,
            orientation: csp_sim::bubble::surface_upright(lat, lon),
        };
        let (pos, rot, vel, ang_vel) =
            csp_sim::bubble::surface_pose_restore(&env, &pose, com_local, self.sim.clock());
        // `launch_preflight` runs `apply_spawn_physical`'s refusals
        // BEFORE the enqueue + mode switch, so a doomed spawn keeps
        // the player in the VAB with the reason on the notice row.
        // The candidate is posed exactly as the spawn would insert it.
        candidate.pos = pos;
        candidate.rot = rot.normalize();
        candidate.vel = vel;
        candidate.ang_vel = ang_vel;
        if let Some(reason) = self.launch_preflight(&craft, &candidate) {
            return Err(reason);
        }
        // The staged craft's firing plan rides session state (the
        // spawned graph drops stage metadata); `fire_stage` drains it
        // one set per press. The plan binds to THIS launch's vessel —
        // captured before the enqueue so `min_id` bounds the binding
        // to ids minted after this point (the spawn's `VesselId` is
        // allocated at drain, so prediction is impossible — the
        // fingerprint + bound are the identity).
        let plan = flight::StagePlan::for_launch(stage_plan, &self.sim, &craft);
        self.enqueue_command(
            SourceClass::Ui,
            PLAYER_SOURCE_ID,
            SimCommand::SpawnPhysicalVessel {
                craft,
                pos,
                rot,
                vel,
                ang_vel,
            },
        );
        // Only a successfully enqueued launch leaves the VAB — every
        // refusal above returned before this point. A recover-control
        // pin from an earlier T press must not survive it: the
        // documented rule is newest-spawn-takes-focus, so the fresh
        // launch gets the camera, HUD, and command route.
        self.clear_flight_focus_override();
        self.world.insert_resource(plan);
        self.set_mode(ViewMode::Flight);
        Ok(())
    }

    /// Fire the launched craft's next stage: pop the front drop set
    /// off [`flight::StagePlan`] and issue one logged `JettisonPart`
    /// per node — the session's decoupler semantics ("severs all its
    /// child edges") mapped onto the sim's v1 command surface.
    /// Returns the nodes jettisoned (`0` = no stage left, or the
    /// press was refused before popping).
    ///
    /// The plan belongs to the vessel its launch spawned — NOT the
    /// flight focus, which a newer spawn or a breakup fragment can
    /// take over without retargeting this craft's stages (the
    /// jettisons must reach the launched graph's node ids, not
    /// whatever holds focus). [`flight::StagePlan::vessel_for`] binds
    /// that vessel once the spawn lands.
    ///
    /// `JettisonPart` is `InBubble`-only in v1, so the pop happens
    /// only after the bound vessel checks bubble-resident: on a
    /// surface-latched pad craft or a rails vessel the commands would
    /// all reject while the stage was still eaten — a silent no-op
    /// that permanently spent the firing. Every outcome now lands a
    /// [`flight::FlightSfx`] cue (the HUD's visible feedback
    /// channel): the press either visibly does something or says why
    /// not. Honest limitation: a jettisoned part leaves the sim
    /// outright — no KSP-style debris fragment, and engines are
    /// globally armed so a firing separates but never ignites.
    pub fn fire_stage(&mut self) -> usize {
        let (vessel, ambiguous, refuse) = {
            let mut plan = self.world.resource_mut::<flight::StagePlan>();
            if plan.stages.is_empty() {
                // `total` keeps the cases honest — an expended
                // multi-stage plan and a single-stage craft are not
                // the same "nothing".
                (
                    None,
                    false,
                    Some(if plan.total > 0 {
                        "ALL STAGES FIRED — NOTHING LEFT"
                    } else {
                        "SINGLE-STAGE CRAFT — NOTHING TO STAGE"
                    }),
                )
            } else {
                let v = plan.vessel_for(&self.sim);
                (v, plan.ambiguous, None)
            }
        };
        if let Some(msg) = refuse {
            self.flight_notice(msg.to_string());
            return 0;
        }
        if ambiguous {
            // P05-F1: two same-craft mints ≥ min_id — the plan can
            // never name its vessel honestly, so a firing now would
            // hit a random one. Retire it like the gone-vessel branch
            // rather than leaving a loaded trap for the next press.
            {
                let mut plan = self.world.resource_mut::<flight::StagePlan>();
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
                    let mut plan = self.world.resource_mut::<flight::StagePlan>();
                    plan.stages.clear();
                    plan.vessel = None;
                }
                self.flight_notice("NOTHING TO STAGE — LAUNCHED CRAFT GONE".to_string());
                return 0;
            }
        }
        // `stages` is stored front-first firing order; `remove(0)`
        // takes the next firing (pop() would take the last).
        let nodes = self
            .world
            .resource_mut::<flight::StagePlan>()
            .stages
            .remove(0);
        let fired = nodes.len();
        for node in nodes {
            self.enqueue_command(
                SourceClass::Ui,
                PLAYER_SOURCE_ID,
                SimCommand::JettisonPart { vessel, node },
            );
        }
        self.flight_notice(format!("STAGED — {fired} PART(S) JETTISONED"));
        fired
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

    /// The pre-flight half of `apply_spawn_physical`'s refusals —
    /// every gate the spawn's drain-time apply would reject on,
    /// checked BEFORE [`View::launch_craft`] commits so a doomed spawn
    /// keeps the player in the VAB with the reason on the notice row
    /// instead of landing in an empty flight view with only a logged
    /// command reject to show for it. `candidate` is the welded body
    /// already posed at the spawn point. Returns the refusal text
    /// (already posted through [`Self::launch_refused`]) or `None`
    /// when the launch is clear. The drain re-runs every gate
    /// regardless — a second source can spawn in between — so this
    /// check only ever *refuses* early; it can never let a bad spawn
    /// through.
    fn launch_preflight(
        &mut self,
        craft: &csp_sim::CraftDef,
        candidate: &csp_phys::WeldedBody,
    ) -> Option<String> {
        // §2.9's cap — vessels + in-flight builds (each build already
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
        // The pad is a single berth — the sim's occupancy predicate
        // counts InBubble hulls directly and re-materializes each
        // latched vessel's stored pose at the current phase, so a
        // parked craft still blocks the site.
        if self.sim.spawn_site_occupied(candidate) {
            return Some(
                self.launch_refused(
                    "launch site occupied — recover or move the parked vessel first",
                ),
            );
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
    /// catalog leaves the VAB honest-but-empty).
    pub fn install_vab_catalog(
        &mut self,
        parts: csp_data::LoadedParts,
        resources: csp_data::ResourceFile,
    ) {
        self.world
            .resource_mut::<vab::VabSessionRes>()
            .0
            .set_catalog(parts, resources);
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
/// firing sequence resolved into per-firing drop sets for
/// [`flight::StagePlan`]: each stage's drop set is every node in the
/// subtree below each decoupler assigned to that stage (the session's
/// "severs all its child edges" rule — the decoupler stays
/// parent-side). Stages with no decoupler, or a decoupler severing
/// nothing, contribute nothing.
fn stage_plan_of(vab: &csp_ui::vab::VabSession) -> Vec<Vec<csp_phys::PartNodeId>> {
    let session = vab.session();
    let mut plan = Vec::new();
    for stage in session.stage_order() {
        let mut drop_set = std::collections::BTreeSet::new();
        for id in session.parts_in_stage(stage) {
            let is_decoupler = vab.def_of(id).is_some_and(|d| {
                d.modules
                    .iter()
                    .any(|m| matches!(m, csp_data::ModuleDef::Decoupler))
            });
            if !is_decoupler {
                continue;
            }
            let mut stack = session.children_ids(id);
            while let Some(n) = stack.pop() {
                if drop_set.insert(n) {
                    stack.extend(session.children_ids(n));
                }
            }
        }
        if !drop_set.is_empty() {
            plan.push(drop_set.into_iter().collect());
        }
    }
    plan
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
        match View::from_source(&src, std::path::Path::new("data")) {
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
                parts: Vec::new(),
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
        let flight_dist = view.world.resource::<flight::FlightInput>().distance_m;
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
        // hover ray — the mask set must not eat the whole screen.
        view.set_egui_input(input(vec![egui::Event::PointerMoved(egui::pos2(
            400.0, 500.0,
        ))]));
        view.begin_frame();
        view.end_frame();
        view.with_vab_session(|v| {
            assert!(
                v.cursor_px().is_some(),
                "unmasked viewport no longer publishes the hover ray"
            );
        });
    }
}
