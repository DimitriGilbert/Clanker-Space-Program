#![forbid(unsafe_code)]

//! egui panels, VAB session model, input bindings, settings (PLAN §2.1).
//!
//! Pure egui functions only: this crate never sees `bevy_ecs`, `csp-sim`,
//! `wgpu`, or `winit` (§2.1 dependency rules). Panels take plain-data input
//! structs populated by `csp-view`'s `ui_build` system from presentation
//! mirrors — the sim is never read directly — and return plain-data output
//! structs the same system stores for next frame's `Input` (§2.5). Painting
//! the resulting `egui::FullOutput` stays in `csp-app`/`egui-wgpu`.
//!
//! - [`vab`] — the M3 VAB: [`vab::VabSession`] owns the working-craft
//!   session instance (§2.2 category C), the [`vab::VabAction`] op
//!   channel, and the editor panels; [`top_bar`] is the always-present
//!   mode bar.

/// The M3 vehicle-assembly editor (PLAN M3 exit row): session instance,
/// op channel, and panels.
pub mod vab;

pub use vab::{ModeSelect, VabSession};

/// Everything the M0 debug panel needs — mirrors and session values only.
///
/// Populated per frame by the `UiBuild` system in `csp-view`. Every field is
/// either a presentation mirror of sim state (`sim_seconds`, `tick`,
/// `warp_level`, `ticks_last_frame`) or session/presentation data that never
/// reaches the sim (`frame_dt_ms`, adapter strings, `warp_requested`).
#[derive(Clone, Debug)]
pub struct DebugPanelInput<'a> {
    /// GPU adapter name — session info written once by `csp-app`.
    pub adapter_name: &'a str,
    /// wgpu backend label — session info written once by `csp-app`.
    pub backend: &'a str,
    /// Mirrored sim time in seconds (the `Sync` bridge's `Clock` copy).
    pub sim_seconds: f64,
    /// Mirrored sim tick count.
    pub tick: u64,
    /// Ticks the last `SimStep` advanced — a `Sync`-written mirror.
    pub ticks_last_frame: u64,
    /// Total frames rendered — presentation stat.
    pub frames: u64,
    /// Wall-clock frame delta for the HUD. Display-only: wall time never
    /// reaches the sim (§2.5).
    pub frame_dt_ms: f64,
    /// The sim-stored warp level, mirrored verbatim — with the M0 ladder
    /// encoding that IS the tick multiplier (§2.6.3).
    pub warp_level: u32,
    /// The multiplier the player asked for (warp-controller session state).
    pub warp_requested: u64,
    /// The multiplier the controller emits after §2.6.3 auto-derate; when it
    /// differs from `warp_requested` the panel says so.
    pub warp_effective: u64,
    /// §2.13 halt reason when the sim is stopped, else `None`.
    pub fatal_reason: Option<&'a str>,
}

/// Requests the panel emitted during `UiBuild` — plain data, consumed by
/// `csp-app` at the *next* frame's Input phase (§2.5's accepted one-frame
/// latency for UI-issued changes).
#[derive(Clone, Copy, Debug, Default)]
pub struct DebugPanelOutput {
    /// Net warp-ladder steps requested this pass (buttons `-`/`+`; the `,`/`.`
    /// keys reach the controller through winit instead).
    pub warp_step: i32,
}

/// The M0 debug panel — the egui content of the `UiBuild` schedule (§2.5 #5,
/// Appendix A item 3). Reads only [`DebugPanelInput`] mirrors; emits only
/// [`DebugPanelOutput`] requests. All mutation of the real world happens via
/// those requests a frame later.
pub fn debug_panel(
    ctx: &egui::Context,
    input: &DebugPanelInput<'_>,
    output: &mut DebugPanelOutput,
) {
    egui::Window::new("CSP — M0 scaffold").show(ctx, |ui| {
        ui.label(format!("adapter: {}", input.adapter_name));
        ui.label(format!("backend: {}", input.backend));
        ui.separator();
        ui.label(format!("sim time: {:.2} s", input.sim_seconds));
        ui.label(format!("tick: {}", input.tick));
        ui.label(format!("ticks last frame: {}", input.ticks_last_frame));
        ui.label(format!("frames: {}", input.frames));
        ui.label(format!("frame dt: {:.2} ms", input.frame_dt_ms));
        ui.separator();
        // §2.6.3: the stored level IS the multiplier; the controller's
        // request may sit higher under an auto-derate.
        ui.label(format!("warp: ×{}", input.warp_level));
        if input.warp_requested != input.warp_effective {
            ui.label(format!(
                "requested ×{} — derated to ×{}",
                input.warp_requested, input.warp_effective
            ));
        }
        ui.horizontal(|ui| {
            ui.label("timewarp");
            if ui.button("−").clicked() {
                output.warp_step -= 1;
            }
            if ui.button("+").clicked() {
                output.warp_step += 1;
            }
        });
        if let Some(reason) = input.fatal_reason {
            ui.colored_label(egui::Color32::RED, format!("SIM HALTED — {reason}"));
        }
    });
}

/// One map-view label — a clip-space anchor + text, produced by
/// `csp-view`'s `map_present` system (M1-D). Plain data; `map_overlay`
/// converts the NDC anchor into screen coordinates.
#[derive(Clone, Debug)]
pub struct MapLabel {
    /// Clip-space anchor: x/y in `[-1,1]` (wgpu NDC, y up), z = depth.
    pub ndc: [f32; 3],
    /// Label text (body id / vessel id at M1).
    pub text: String,
    /// Linear RGBA.
    pub color: [f32; 4],
}

/// The M1 map-view overlay (§5.1 R0): a non-interactive `Background`
/// layer painting name labels at projected body/vessel positions plus a
/// hint bar — the egui half of the map; the wgpu half (orbit lines,
/// markers) is `csp_render::map`. Called by `csp-view`'s `UiBuild`
/// system only while `MapMode::Map` is active.
///
/// NDC → screen: `x_px = (ndc.x + 1)/2 · w`, `y_px = (1 − ndc.y)/2 · h`
/// — anchors off-screen (|x|,|y| > 1.2) or behind the far field are
/// skipped.
pub fn map_overlay(
    ctx: &egui::Context,
    focus_name: &str,
    camera_distance_m: f64,
    labels: &[MapLabel],
) {
    let rect = ctx.content_rect();
    egui::Area::new(egui::Id::unique("map_overlay"))
        .order(egui::Order::Background)
        .interactable(false)
        .fixed_pos(egui::pos2(0.0, 0.0))
        .show(ctx, |ui| {
            let painter = ui.painter();
            for label in labels {
                let [nx, ny, _z] = label.ndc;
                if nx.abs() > 1.2 || ny.abs() > 1.2 {
                    continue;
                }
                let pos = egui::pos2(
                    rect.left() + (nx * 0.5 + 0.5) * rect.width(),
                    rect.top() + (0.5 - ny * 0.5) * rect.height(),
                );
                let c = egui::Color32::from_rgba_unmultiplied(
                    (label.color[0] * 255.0) as u8,
                    (label.color[1] * 255.0) as u8,
                    (label.color[2] * 255.0) as u8,
                    (label.color[3] * 255.0) as u8,
                );
                painter.text(
                    pos + egui::vec2(7.0, -7.0),
                    egui::Align2::LEFT_BOTTOM,
                    &label.text,
                    egui::FontId::proportional(12.0),
                    c,
                );
            }
            painter.text(
                egui::pos2(rect.left() + 10.0, rect.bottom() - 8.0),
                egui::Align2::LEFT_BOTTOM,
                format!(
                    "MAP  ·  focus: {focus_name}  ·  camera {camera_distance_m:.3e} m  \
                     —  [M] flight  [Tab] focus  drag: orbit  wheel: zoom  WASD: pan"
                ),
                egui::FontId::proportional(13.0),
                egui::Color32::from_gray(180),
            );
        });
}

/// Mirrors for the M4c flight HUD — populated by `csp-view`'s
/// `flight_present` system (`FlightUiData`); `csp-ui` sees only
/// this plain data (§2.1's dependency rule).
#[derive(Clone, Debug, Default)]
pub struct FlightHudInput<'a> {
    /// A focused vessel exists (else the HUD shows "no vessel").
    pub has_vessel: bool,
    /// Altitude above surface (m).
    pub alt_m: f64,
    /// CoM speed (m/s).
    pub speed_mps: f64,
    /// Vertical speed (m/s).
    pub vspeed_mps: f64,
    /// Throttle [0,1].
    pub throttle: f64,
    /// Total vessels tracked.
    pub vessel_count: usize,
    /// Debris-flagged vessels.
    pub debris_count: usize,
    /// §3.7's visual-SFX cue lines (engine, staging, impact,
    /// breakup) — drawn as top-center event flashes.
    pub sfx: &'a [String],
}

/// The M4c flight HUD (§3.7's presentation floor): bottom-left
/// telemetry strip — altitude, speed, vertical speed, throttle bar,
/// vessel/debris counts — plus the controls hint and the visual-SFX
/// event flashes. Drawn by `csp-view`'s `UiBuild` while
/// `ViewMode::Flight` is active.
pub fn flight_hud(ctx: &egui::Context, input: &FlightHudInput<'_>) {
    // §3.7's visual-SFX channel: each live cue is a top-center flash,
    // newest at the bottom of the stack.
    if !input.sfx.is_empty() {
        let rect = ctx.content_rect();
        egui::Area::new(egui::Id::unique("flight_sfx"))
            .order(egui::Order::Foreground)
            .interactable(false)
            .fixed_pos(egui::pos2(rect.center().x, rect.top() + 48.0))
            .pivot(egui::Align2::CENTER_TOP)
            .show(ctx, |ui| {
                for line in input.sfx {
                    ui.label(
                        egui::RichText::new(format!("« {line} »"))
                            .font(egui::FontId::proportional(17.0))
                            .color(egui::Color32::from_rgb(255, 196, 64)),
                    );
                }
            });
    }
    let rect = ctx.content_rect();
    egui::Area::new(egui::Id::unique("flight_hud"))
        .order(egui::Order::Foreground)
        .interactable(false)
        .fixed_pos(egui::pos2(10.0, rect.bottom() - 116.0))
        .show(ctx, |ui| {
            if !input.has_vessel {
                ui.label(
                    egui::RichText::new("FLIGHT — no vessel in the bubble")
                        .color(egui::Color32::from_gray(200)),
                );
                ui.label(
                    egui::RichText::new("[V] VAB  ·  [M] map").color(egui::Color32::from_gray(150)),
                );
                return;
            }
            let mono = egui::FontId::monospace(13.0);
            let c = egui::Color32::from_gray(220);
            ui.label(
                egui::RichText::new(format!("ALT  {:>12.1} m", input.alt_m))
                    .font(mono.clone())
                    .color(c),
            );
            ui.label(
                egui::RichText::new(format!("SPD  {:>12.1} m/s", input.speed_mps))
                    .font(mono.clone())
                    .color(c),
            );
            ui.label(
                egui::RichText::new(format!("V/S  {:>12.1} m/s", input.vspeed_mps))
                    .font(mono.clone())
                    .color(c),
            );
            ui.add(
                egui::ProgressBar::new(input.throttle as f32)
                    .text(format!("THR  {:>5.1}%", input.throttle * 100.0))
                    .desired_width(180.0),
            );
            ui.label(
                egui::RichText::new(format!(
                    "vessels {}  ·  debris {}",
                    input.vessel_count, input.debris_count
                ))
                .font(mono.clone())
                .color(egui::Color32::from_gray(170)),
            );
            ui.label(
                egui::RichText::new(
                    "[Z] full  [X] cut  drag: orbit  wheel: zoom  [M] map  [V] VAB",
                )
                .color(egui::Color32::from_gray(140)),
            );
        });
}

// ---------------------------------------------------------------------------
// M5 maneuver panel — node placement + current-node status (map mode).
// ---------------------------------------------------------------------------

/// The panel's editable state — session data owned by `csp-view` (§2.2
/// category C), passed in and mutated by the pure panel function.
#[derive(Clone, Debug)]
pub struct ManeuverPanelState {
    /// The vessel the edits target.
    pub vessel: u64,
    /// Seconds from "now" the node sits at — the placement input.
    pub t_plus_s: f64,
    /// Δv components `[prograde, normal, radial]` in m/s.
    pub dv: [f64; 3],
}

impl Default for ManeuverPanelState {
    fn default() -> Self {
        Self {
            vessel: 0,
            t_plus_s: 600.0,
            dv: [10.0, 0.0, 0.0],
        }
    }
}

/// One on-rails vessel's node status, mirrored for the panel — plain
/// data (`csp-ui` never sees `csp-sim`).
#[derive(Clone, Copy, Debug)]
pub struct ManeuverVessel {
    /// The `VesselId`.
    pub id: u64,
    /// The vessel's scheduled node `(tick, Δv m/s)`, if armed.
    pub node: Option<(u64, f64)>,
}

/// The one action the panel can emit per frame — translated into
/// `SetManeuver`/`DeleteManeuver` `SimCommand`s by `csp-view` (the
/// §2.1 rule: `csp-ui` cannot name sim types).
#[derive(Clone, Copy, Debug)]
pub enum ManeuverCommand {
    /// Set/replace the vessel's node at `tick` with `dv`.
    Set {
        /// The target vessel.
        vessel: u64,
        /// The burn tick (panel input `t_plus_s` resolved against the
        /// mirrored clock by the caller).
        tick: u64,
        /// `[prograde, normal, radial]` m/s.
        dv: [f64; 3],
    },
    /// Delete the vessel's node (cancel a burn in progress too).
    Delete {
        /// The target vessel.
        vessel: u64,
    },
}

/// The M5 node panel — shown in map mode beside the orbit lines it
/// edits. Vessel picker, live node status, and the placement fields:
/// `T+` seconds to node plus the three Δv components. Emits at most
/// one [`ManeuverCommand`] per frame.
pub fn maneuver_panel(
    ctx: &egui::Context,
    tick: u64,
    vessels: &[ManeuverVessel],
    state: &mut ManeuverPanelState,
    out: &mut Option<ManeuverCommand>,
) {
    if vessels.is_empty() {
        return;
    }
    if !vessels.iter().any(|v| v.id == state.vessel) {
        state.vessel = vessels[0].id;
    }
    egui::Window::new("Maneuver node").show(ctx, |ui| {
        egui::ComboBox::from_label("vessel")
            .selected_text(format!("vessel {}", state.vessel))
            .show_ui(ui, |ui| {
                for v in vessels {
                    ui.selectable_value(&mut state.vessel, v.id, format!("vessel {}", v.id));
                }
            });
        if let Some(n) = vessels
            .iter()
            .find(|v| v.id == state.vessel)
            .and_then(|v| v.node)
        {
            let eta = n.0.saturating_sub(tick) as f64 / 64.0;
            ui.label(format!("armed: T−{eta:.0} s · Δv {:.1} m/s", n.1));
        } else {
            ui.label("no node armed");
        }
        ui.add(
            egui::DragValue::new(&mut state.t_plus_s)
                .prefix("T+ ")
                .suffix(" s")
                .speed(1.0),
        );
        ui.horizontal(|ui| {
            for (c, label) in state.dv.iter_mut().zip(["pro", "nrm", "rad"]) {
                ui.add(
                    egui::DragValue::new(c)
                        .prefix(format!("{label} "))
                        .speed(0.1),
                );
            }
        });
        ui.horizontal(|ui| {
            if ui.button("Set node").clicked() {
                let ticks = (state.t_plus_s.max(0.0) * 64.0) as u64;
                *out = Some(ManeuverCommand::Set {
                    vessel: state.vessel,
                    tick: tick.saturating_add(ticks).max(tick + 1),
                    dv: state.dv,
                });
            }
            if ui.button("Delete node").clicked() {
                *out = Some(ManeuverCommand::Delete {
                    vessel: state.vessel,
                });
            }
        });
    });
}
