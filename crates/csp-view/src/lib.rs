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

/// `SimCommand` vocabulary, re-exported so `csp-app` can produce commands
/// without depending on `csp-sim` directly — §2.1's rule that `csp-view` is
/// the only crate depending on BOTH `csp-sim` and `csp-render` stays true.
/// `AdvanceReport`/`EventClass` ride along because `csp-app` reads the
/// fired-event classes for §2.6.5 Clamping demotion.
pub use csp_sim::{AdvanceReport, EventClass, SimCommand, SourceClass};
// Re-exported so `csp-app` names mode requests without a csp-ui edge.
pub use csp_ui::ModeSelect;

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
}

/// The maneuver panel's session state — `csp-view` owns it (§2.2 C),
/// `csp_ui::maneuver_panel` mutates it inside `UiBuild`.
#[derive(Resource, Default)]
pub struct ManeuverPanelRes(pub csp_ui::ManeuverPanelState);

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
        csp_ui::vab::top_bar(ctx, mode, &mut vab_out);
        if mode != csp_ui::ModeSelect::Vab {
            csp_ui::debug_panel(ctx, &input, &mut panel_output);
        }
        if map_ui.mode == map::MapMode::Map {
            csp_ui::map_overlay(
                ctx,
                &map_ui.focus_name,
                map_ui.camera_distance_m,
                &map_ui.labels,
            );
            // M5 node panel — placement edits → outbox commands.
            let mut mcmd = None;
            csp_ui::maneuver_panel(
                ctx,
                clock.0.ticks(),
                &map_snap
                    .vessels
                    .iter()
                    .map(|(id, _, n)| csp_ui::ManeuverVessel {
                        id: id.0,
                        node: n.as_ref().map(|n| (n.tick, n.dv_magnitude())),
                    })
                    .collect::<Vec<_>>(),
                &mut maneuver_state.0,
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
        if mode == csp_ui::ModeSelect::Vab {
            csp_ui::vab::vab_editor(ctx, &mut vab.0, &files, &mut vab_out);
        }
        if mode == csp_ui::ModeSelect::Flight {
            csp_ui::flight_hud(
                ctx,
                &csp_ui::FlightHudInput {
                    has_vessel: flight_ui.has_vessel,
                    alt_m: flight_ui.alt_m,
                    speed_mps: flight_ui.speed_mps,
                    vspeed_mps: flight_ui.vspeed_mps,
                    throttle: flight_ui.throttle,
                    vessel_count: flight_ui.vessel_count,
                    debris_count: flight_ui.debris_count,
                    sfx: &flight_ui.sfx,
                },
            );
        }
    }));
    outbox.warp_step = panel_output.warp_step;
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
}

impl std::fmt::Display for ViewError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Load(e) => write!(f, "data/ startup load failed: {e}"),
            Self::Sim(e) => write!(f, "data/ bodies rejected by the orbit model: {e}"),
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
        let root = data_root();
        let sys = csp_data::load_system(&root.join("systems").join("home"))?;
        let defs: Vec<csp_data::BodyDef> = sys.bodies.iter().map(|b| b.def.clone()).collect();
        let orbits =
            csp_sim::orbit::PatchedConics::from_body_defs(&defs).map_err(ViewError::Sim)?;
        let balance = csp_data::load_balance(&root.join("balance.ron"))?;
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
        sim.set_docking_config(csp_sim::DockingConfig::from_balance(&balance));
        sim.set_transfer_config(csp_sim::TransferConfig::from_balance(&balance));
        // M7-A §3.2: the science tunables and the experiment-def table —
        // same data-derived wiring (`csp-bench`'s scenario builder does
        // the identical three calls); without them science commands
        // would run on the no-data defaults and reject every run.
        sim.set_science_config(csp_sim::ScienceConfig::from_balance(&balance));
        let experiments = csp_data::load_experiments(&root.join("experiments"))?;
        sim.set_experiments(
            experiments
                .experiments
                .iter()
                .map(|e| (e.def.id.clone(), e.def.clone()))
                .collect(),
        );
        // M7-B §3.3: the merged tech tree — same data-derived wiring;
        // without it research commands reject with "no tech tree".
        sim.set_tech_tree(csp_data::load_techtree(&root.join("techtree"))?.merged());
        // M7-C §3.5: the economy tunables (starting funds, the salary
        // cadence, hire cost, milestone payout, recovery shares) and
        // the system's site table — the same data-derived wiring;
        // without them the funds paths run on the no-data defaults
        // and `RecoverVessel` has no sites to name.
        sim.set_economy_config(csp_sim::economy::EconomyConfig::from_balance(&balance));
        let sites = csp_data::load_sites(&root.join("systems").join("home").join("sites.ron"))?;
        sim.set_sites(
            sites
                .sites
                .iter()
                .map(|s| (s.id.clone(), s.clone()))
                .collect(),
        );
        // M7-D §3.6: the contract template table + the board-size
        // tunable — the same data-derived wiring.
        sim.set_contracts(
            csp_data::load_contracts(&root.join("contracts"))?
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
            csp_data::load_recipes(&root.join("recipes"))?
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
        sim.found_authored_colonies(
            &csp_data::load_colonies(&root.join("systems").join("home").join("colonies.ron"))?
                .colonies,
        )
        .map_err(ViewError::Sim)?;
        let mut view = Self::with_sim(sim);
        // M3 VAB: the part catalog + resource registry are data-derived
        // constants for the session model (like BodyTable — §7.3's
        // re-derived-on-load class).
        let parts = csp_data::load_parts(&root.join("parts"))?;
        let resources = csp_data::load_resources(&root.join("resources.ron"))?;
        view.install_vab_catalog(parts, resources);
        // R2: the authored terrain table (noise seeds, ocean datums,
        // decoded biome palettes) — installed over the empty default.
        view.world
            .insert_resource(terrain::config_from_bodies(&sys.bodies));
        Ok(view)
    }

    /// wasm: no `data/` filesystem exists at this milestone — the wasm
    /// slice runs the trivial M1 world (empty system, mirror-pinned
    /// default budget; `csp-bench`'s `balance_ron_mirrors_sim_event_budget`
    /// test keeps that mirror honest). Native is the authoritative data
    /// path; shipping `data/` inside the wasm bundle is a packaging
    /// problem for a later milestone.
    #[cfg(target_arch = "wasm32")]
    pub fn new() -> Result<Self, ViewError> {
        Ok(Self::with_sim(SimWorld::new()))
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
        world.insert_resource(map::MapFrame::default());
        world.insert_resource(ManeuverPanelRes::default());

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
        world.insert_resource(flight::FlightInputEvents::default());
        world.insert_resource(flight::FlightSyncInput::default());
        world.insert_resource(flight::FlightSnapshot::default());
        world.insert_resource(flight::FlightUiData::default());
        world.insert_resource(flight::FlightFrame::default());
        // R2 surface resources (see terrain.rs): empty config ⇒
        // per-body params synthesised from the body id; `View::new`
        // installs the authored table over it.
        world.insert_resource(terrain::TerrainConfig::default());
        world.insert_resource(terrain::TerrainCache::default());
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
        presentation.add_systems((map::map_present, vab::vab_present, flight::flight_present));
        let mut render_extract = Schedule::default();
        render_extract.add_systems((map::extract_map, vab::extract_vab, flight::extract_flight));

        let mut ui_build = Schedule::default();
        ui_build.add_systems(build_ui);

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
        }
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
        // staging convention (the schedules never read `sim`).
        self.world
            .insert_resource(flight::capture_sync_input(&self.sim));
        self.ticks_last_frame = 0;
        self.schedules.sync.run(&mut self.world);
        self.schedules.presentation.run(&mut self.world);
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
    pub fn set_mode(&mut self, mode: ViewMode) {
        {
            let mut v = self.world.resource_mut::<vab::VabInput>();
            v.open = mode == ViewMode::Vab;
        }
        let focus = self.world.resource::<map::MapInput>().focus;
        let want_map = mode == ViewMode::Map;
        let refocus_dist = self
            .world
            .resource::<map::BodyTable>()
            .bodies
            .get(focus)
            .and_then(|b| b.soi_m)
            .map(|soi| (5.0 * soi).clamp(1.0e5, 2.0e11))
            .unwrap_or(6.0e10);
        let mut m = self.world.resource_mut::<map::MapInput>();
        if want_map != (m.mode == map::MapMode::Map) {
            m.mode = if want_map {
                map::MapMode::Map
            } else {
                map::MapMode::Flight
            };
            if want_map {
                // Re-derive framing like CycleFocus's refocus.
                m.pan_offset = csp_core::DVec3::ZERO;
                m.distance_m = refocus_dist;
            }
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

    /// The focused vessel's `VesselId` — the throttle/attitude
    /// command target (`None` when no InBubble vessel exists).
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
    pub fn launch_craft(&mut self) {
        let craft = self.with_vab_session(|v| {
            let session = v.session().clone();
            let analysis = v.analysis().cloned();
            (session, analysis)
        });
        let craft_def = (|| {
            let (session, analysis) = craft;
            if session.is_empty() {
                return Some(stock_flight_craft());
            }
            let (graph, _built) = session.to_part_graph().ok()?;
            let analysis = analysis?;
            let parts: Vec<csp_phys::WeldPart> = analysis
                .effective
                .iter()
                .map(|e| e.weld_input.clone())
                .collect();
            let part_costs: Vec<f64> = analysis.effective.iter().map(|e| e.cost_funds).collect();
            Some(csp_sim::CraftDef {
                graph,
                parts,
                part_costs,
            })
        })();
        let Some(craft) = craft_def else { return };
        let Some(env) = self.sim.environment() else {
            return;
        };
        let (r, spin, centre) = (env.radius_m, env.body_spin, env.body_center);
        let pos = centre + csp_core::DVec3::X * (r + 12.0);
        let rot =
            csp_core::DQuat::from_axis_angle(csp_core::DVec3::Y, core::f64::consts::FRAC_PI_2);
        let vel = spin.cross(pos - centre);
        let ang_vel = rot.conjugate().rotate(spin);
        self.enqueue_command(
            SourceClass::Ui,
            1,
            SimCommand::SpawnPhysicalVessel {
                craft,
                pos,
                rot,
                vel,
                ang_vel,
            },
        );
        self.set_mode(ViewMode::Flight);
    }

    /// Run a closure against the working-craft session — the plumbing
    /// accessor for tests/tools (app code reaches the session only via
    /// `vab_event` and the UiBuild pass, §2.5).
    pub fn with_vab_session<R>(&mut self, f: impl FnOnce(&mut csp_ui::vab::VabSession) -> R) -> R {
        f(&mut self.world.resource_mut::<vab::VabSessionRes>().0)
    }

    /// Install the part catalog + resource registry (called by
    /// `View::new` after `data/` loads; an unset catalog leaves the
    /// VAB honest-but-empty — the wasm slice at M3).
    pub fn install_vab_catalog(
        &mut self,
        parts: csp_data::LoadedParts,
        resources: csp_data::ResourceFile,
    ) {
        self.world
            .resource_mut::<vab::VabSessionRes>()
            .0
            .set_catalog(parts, resources);
    }

    /// The craft-file name list the save/load UI shows — plumbed by
    /// `csp-app` (its file store owns the bytes).
    pub fn set_craft_files(&mut self, files: Vec<String>) {
        self.world.resource_mut::<vab::CraftFileList>().0 = files;
    }
}

/// The data/ directory root: `./data` when it exists (the app runs from
/// the repo root), else the repo layout relative to this crate's manifest
/// so tests and tools resolve the same files from any CWD.
#[cfg(not(target_arch = "wasm32"))]
fn data_root() -> std::path::PathBuf {
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
        graph,
        parts: vec![engine, adapter, tank, pod],
        // M7-C: the stock craft's authored build cost — a modest
        // starter craft's price tag (the VAB path reads real resolved
        // `cost_funds`; this debug craft authors its own).
        part_costs: vec![12_000.0, 800.0, 4_000.0, 9_000.0],
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
}
