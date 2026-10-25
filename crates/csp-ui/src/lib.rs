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

/// The M10 settings surface (PLAN §2.1's "settings, input bindings"):
/// the serializable [`settings::Settings`] model, the rebindable
/// [`settings::Keymap`], and the settings panel.
pub mod settings;

/// The M10 first-flight tutorial (PLAN M10 exit row): the
/// [`tutorial::TutorialState`] session model — the step table and the
/// real-progress predicates — plus the [`tutorial::tutorial_overlay`]
/// checklist panel.
pub mod tutorial;

/// The flight attitude instrument — the navball ball + heading tape,
/// pure egui over body-frame staged vectors.
pub mod navball;

pub use settings::{
    AudioSettings, DifficultySettings, GameplaySettings, GraphicsQuality, GraphicsSettings,
    KeyAction, KeyBinding, Keymap, Settings, SettingsPanelOutput, SettingsPanelState,
};
pub use tutorial::{STEP_LAUNCH, STEPS, TutorialInput, TutorialState, TutorialStep};
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

/// The §2.13 `Fatal` surfacing (M10): a readable error window naming the
/// halt reason and the bug-report bundle's path — the on-screen half of
/// "abort the tick, surface a dialog, mark the session read-only, write
/// a bug-report bundle". Drawn by `csp-view`'s `UiBuild` while the
/// session is halted; the bundle write itself is `csp-app`'s.
///
/// `bundle` is the human-readable path (or the write failure's message)
/// the app recorded.
pub fn crash_dialog(ctx: &egui::Context, reason: &str, bundle: Option<&str>) {
    egui::Window::new("Simulation halted")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .show(ctx, |ui| {
            ui.colored_label(
                egui::Color32::RED,
                egui::RichText::new("A fatal sim error stopped the clock.")
                    .font(egui::FontId::proportional(15.0)),
            );
            ui.add_space(4.0);
            ui.label(format!("reason: {reason}"));
            ui.label("The session is read-only — it cannot be saved or continued.");
            match bundle {
                Some(path) => {
                    ui.label("A bug-report bundle was written — send it to the developer:");
                    ui.monospace(path);
                }
                None => {
                    ui.label("Writing the bug-report bundle failed; see the log.");
                }
            }
        });
}

/// One propellant resource's aggregate fill on the focused vessel —
/// plain data for the flight HUD's fuel gauges (units/capacity
/// summed over the vessel's live tanks, per resource).
#[derive(Clone, Debug)]
pub struct PropellantGauge {
    /// Resource display id.
    pub resource: String,
    /// Current contents across the vessel's tanks (units).
    pub units: f64,
    /// Total capacity (units).
    pub capacity: f64,
}

/// What a map label names — drives the overlay's styling (apsis marks
/// draw smaller and dimmer than object names) and the de-overlap
/// pass's priority.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapLabelKind {
    /// A celestial body's name.
    Body,
    /// A vessel's name.
    Vessel,
    /// A maneuver node's status line.
    Node,
    /// An apsis mark (Ap/Pe) on the focused orbit.
    Apsis,
}

/// One map-view label — a clip-space anchor + text, produced by
/// `csp-view`'s `map_present` system (M1-D). Plain data; `map_overlay`
/// converts the NDC anchor into screen coordinates.
#[derive(Clone, Debug)]
pub struct MapLabel {
    /// Clip-space anchor: x/y in `[-1,1]` (wgpu NDC, y up), z = depth.
    pub ndc: [f32; 3],
    /// Label text (body id / vessel id / apsis mark).
    pub text: String,
    /// Linear RGBA.
    pub color: [f32; 4],
    /// What this label names — styling + de-overlap priority.
    pub kind: MapLabelKind,
}

/// The focused orbit's readout for the map's info panel — plain data
/// built by `csp-view` (`csp-ui` never sees sim types). Radii are
/// parent-centred; `parent_radius_m` turns them into surface altitudes
/// when the body table carries the parent's radius.
#[derive(Clone, Debug)]
pub struct MapOrbitInfo {
    /// Subject name ("terrin", "vessel 3").
    pub name: String,
    /// Central body these numbers are relative to.
    pub parent_name: String,
    /// Apoapsis radius from the parent's center (m) — `None` on an
    /// open conic (hyperbolic/parabolic, no apoapsis exists).
    pub apo_r_m: Option<f64>,
    /// Periapsis radius from the parent's center (m).
    pub pe_r_m: f64,
    /// The parent's surface radius, when the model carries it —
    /// `Some` ⇒ the panel prints altitudes, `None` ⇒ radii.
    pub parent_radius_m: Option<f64>,
    /// Orbital period (s) — `None` on an open conic.
    pub period_s: Option<f64>,
}

/// The map-view overlay (§5.1): a `Background` layer painting name
/// labels at projected body/vessel/apsis positions, the focused
/// orbit's readout panel, and a hint bar — the egui half of the map;
/// the wgpu half (orbit lines, markers, starfield) is
/// `csp_render::map`. Called by `csp-view`'s `UiBuild` system only
/// while `MapMode::Map` is active.
///
/// NDC → screen: `x_px = (ndc.x + 1)/2 · w`, `y_px = (1 − ndc.y)/2 · h`
/// — anchors off-screen (|x|,|y| > 1.2) are skipped. Labels that would
/// land on an already-painted label nudge down until clear (or drop
/// after a few tries). `keys` is the *live* (applied) keymap — the
/// hint bar prints bindings, not hardcoded names, so a rebind never
/// leaves the overlay lying about which key does what.
#[allow(clippy::too_many_arguments)] // one overlay = one data bag
pub fn map_overlay(
    ctx: &egui::Context,
    focus_name: &str,
    camera_distance_m: f64,
    labels: &[MapLabel],
    orbit: Option<&MapOrbitInfo>,
    keys: &settings::Keymap,
    key_labels: &std::collections::BTreeMap<String, String>,
) {
    let rect = ctx.content_rect();
    egui::Area::new(egui::Id::unique("map_overlay"))
        .order(egui::Order::Background)
        .interactable(false)
        .fixed_pos(egui::pos2(0.0, 0.0))
        .show(ctx, |ui| {
            let painter = ui.painter();
            let shadow = egui::Color32::from_rgba_unmultiplied(0, 0, 0, 170);
            let mut placed: Vec<egui::Rect> = Vec::with_capacity(labels.len());
            for label in labels {
                let [nx, ny, _z] = label.ndc;
                if nx.abs() > 1.2 || ny.abs() > 1.2 {
                    continue;
                }
                let anchor = egui::pos2(
                    rect.left() + (nx * 0.5 + 0.5) * rect.width(),
                    rect.top() + (0.5 - ny * 0.5) * rect.height(),
                );
                paint_map_label(painter, anchor, label, shadow, &mut placed);
            }

            // The focused orbit's readout — top-right, out of the
            // hint bar's way. Radii print as surface altitudes when the
            // parent radius is known, else raw radii (star-center
            // orbits have no "surface" to subtract).
            if let Some(o) = orbit {
                let alt = |r: f64| o.parent_radius_m.map_or(r, |rad| r - rad);
                let lines = [
                    (format!("{}  orbiting  {}", o.name, o.parent_name), 190),
                    (
                        match o.apo_r_m {
                            Some(r) => format!("Ap   {}", fmt_distance(alt(r))),
                            None => "Ap   — escape".to_string(),
                        },
                        170,
                    ),
                    (format!("Pe   {}", fmt_distance(alt(o.pe_r_m))), 170),
                    (
                        match o.period_s {
                            Some(t) => format!("T    {}", fmt_period(t)),
                            None => "T    —".to_string(),
                        },
                        150,
                    ),
                ];
                let mono = egui::FontId::monospace(12.0);
                let mut y = rect.top() + 34.0;
                for (line, gray) in lines {
                    painter.text(
                        egui::pos2(rect.right() - 12.0, y),
                        egui::Align2::RIGHT_TOP,
                        &line,
                        mono.clone(),
                        egui::Color32::from_gray(gray),
                    );
                    y += 15.0;
                }
            }

            let pan = |a: settings::KeyAction| {
                keys.first_binding_label(a, key_labels)
                    .unwrap_or_else(|| "—".to_string())
            };
            painter.text(
                egui::pos2(rect.left() + 10.0, rect.bottom() - 8.0),
                egui::Align2::LEFT_BOTTOM,
                format!(
                    "MAP  ·  focus: {focus_name}  ·  camera {camera_distance_m:.3e} m  \
                     —  [{}] flight  [{}] focus  ·  click a marker to focus  \
                     ·  drag: orbit  ·  RMB/MMB drag: pan  ·  wheel: zoom to cursor  \
                     ·  [{}/{}/{}/{}] pan",
                    keys.hint_labeled(settings::KeyAction::ToggleMap, key_labels),
                    keys.hint_labeled(settings::KeyAction::CycleFocus, key_labels),
                    pan(settings::KeyAction::PanUp),
                    pan(settings::KeyAction::PanDown),
                    pan(settings::KeyAction::PanLeft),
                    pan(settings::KeyAction::PanRight),
                ),
                egui::FontId::proportional(13.0),
                egui::Color32::from_gray(180),
            );
        });
}

/// One map label: font by kind, `label.color` baked into the galley,
/// then a dark offset copy under the text — a real shadow, which needs
/// [`egui::Painter::galley_with_override_text_color`]. `galley`'s own
/// color arg is only a fallback for `PLACEHOLDER` pixels and
/// `layout_no_wrap` already colored every glyph, so plain `galley`
/// would repaint in the label's own color (a smear, not a shadow).
/// Extracted from [`map_overlay`] under the line budget — de-overlap
/// nudges the label down past anything already drawn (markers cluster
/// when the camera zooms out) and quietly drops one that can't fit
/// rather than stacking into an unreadable pile.
fn paint_map_label(
    painter: &egui::Painter,
    anchor: egui::Pos2,
    label: &MapLabel,
    shadow: egui::Color32,
    placed: &mut Vec<egui::Rect>,
) {
    let (font, lift) = match label.kind {
        MapLabelKind::Apsis => (egui::FontId::proportional(10.5), 5.0),
        _ => (egui::FontId::proportional(12.0), 8.0),
    };
    let c = egui::Color32::from_rgba_unmultiplied(
        (label.color[0] * 255.0) as u8,
        (label.color[1] * 255.0) as u8,
        (label.color[2] * 255.0) as u8,
        (label.color[3] * 255.0) as u8,
    );
    let galley = painter.layout_no_wrap(label.text.clone(), font, c);
    let mut pos = anchor + egui::vec2(8.0, -lift);
    let mut rect_at = egui::Rect::from_min_size(pos, galley.size());
    let mut tries = 0;
    while placed.iter().any(|r| r.intersects(rect_at)) && tries < 8 {
        pos.y += galley.size().y + 2.0;
        rect_at = egui::Rect::from_min_size(pos, galley.size());
        tries += 1;
    }
    if tries >= 8 {
        return;
    }
    placed.push(rect_at);
    painter.galley_with_override_text_color(pos + egui::vec2(1.2, 1.2), galley.clone(), shadow);
    painter.galley(pos, galley, c);
}

/// Mirrors for the M4c flight HUD — populated by `csp-view`'s
/// `flight_present` system (`FlightUiData`); `csp-ui` sees only
/// this plain data (§2.1's dependency rule). No `Default` — the
/// borrowed keymap has no empty value to default to.
#[derive(Clone, Debug)]
pub struct FlightHudInput<'a> {
    /// A focused vessel exists (else the HUD shows "no vessel").
    pub has_vessel: bool,
    /// The focused vessel is surface-latched (parked).
    pub latched: bool,
    /// Altitude above surface (m).
    pub alt_m: f64,
    /// Surface-relative CoM speed (m/s) — `‖v − ω × r‖`.
    pub surface_speed_mps: f64,
    /// Inertial CoM speed (m/s) — the orbital readout.
    pub orbital_speed_mps: f64,
    /// Vertical speed (m/s).
    pub vspeed_mps: f64,
    /// Compass heading of the nose (deg; NaN = undefined).
    pub heading_deg: f64,
    /// Nose pitch above the horizon (deg; NaN = no reference).
    pub pitch_deg: f64,
    /// Roll about the nose axis (deg; NaN = no reference).
    pub roll_deg: f64,
    /// Throttle [0,1].
    pub throttle: f64,
    /// Live part count of the focused vessel.
    pub parts: usize,
    /// The navball's staged body-frame directions.
    pub navball: navball::NavballInput,
    /// Stored warp multiplier — the warp readout moved out of the
    /// (now dev-gated) debug panel.
    pub warp: u32,
    /// Total vessels tracked.
    pub vessel_count: usize,
    /// Debris-flagged vessels.
    pub debris_count: usize,
    /// Osculating apoapsis/periapsis altitude, times-to-apsis, period.
    pub apo_alt_m: Option<f64>,
    /// Periapsis altitude (m).
    pub pe_alt_m: Option<f64>,
    /// Time to apoapsis (s).
    pub t_apo_s: Option<f64>,
    /// Time to periapsis (s).
    pub t_pe_s: Option<f64>,
    /// Orbital period (s).
    pub period_s: Option<f64>,
    /// The focused vessel's propellant — one gauge per resource
    /// still carried (empty for tank-less vessels).
    pub propellant: &'a [PropellantGauge],
    /// Stage firings left in the launched craft's stage plan —
    /// `0` = nothing left. `stages_total` distinguishes an expended
    /// multi-stage craft from a genuinely single-stage one.
    pub stages_left: usize,
    /// Firings the launched craft's stage plan captured at launch —
    /// `0` = single-stage (or no plan); `> 0` with `stages_left == 0`
    /// = expended.
    pub stages_total: usize,
    /// The *live* (applied) keymap — the hint lines print the real
    /// bindings, never hardcoded names (`help_panel`'s rule).
    pub keys: &'a settings::Keymap,
    /// Observed physical-token → glyph map — hints print the keys as
    /// they read on this machine's layout.
    pub key_labels: &'a std::collections::BTreeMap<String, String>,
}

/// Number formatting for the telemetry column: KSP-style SI switch —
/// metres under 10 km, km above.
fn fmt_alt(m: f64) -> String {
    if m.abs() < 10_000.0 {
        format!("{:>7.1} m", m)
    } else {
        format!("{:>7.1} km", m / 1e3)
    }
}

/// SI distance formatting for map readouts — the same m→km switch as
/// the flight column but unpadded and extended to Gm for
/// interplanetary ranges.
pub fn fmt_distance(m: f64) -> String {
    let a = m.abs();
    if a < 10_000.0 {
        format!("{m:.0} m")
    } else if a < 1.0e9 {
        format!("{:.1} km", m / 1e3)
    } else {
        format!("{:.2} Gm", m / 1e9)
    }
}

/// Orbital-period formatting: seconds under a kilosecond, then
/// minutes/hours/days so a year-long transfer doesn't print
/// "31000000 s".
pub fn fmt_period(s: f64) -> String {
    let a = s.abs();
    if a < 1000.0 {
        format!("{s:.0} s")
    } else if a < 100_000.0 {
        format!("{:.1} min", s / 60.0)
    } else if a < 10_000_000.0 {
        format!("{:.1} h", s / 3600.0)
    } else {
        format!("{:.1} d", s / 86_400.0)
    }
}

/// The §3.7 visual-SFX channel as a GLOBAL overlay — the cue stack
/// (`FlightSfx`: sim-transition edges AND `View::flight_notice`
/// refusals) drawn top-center in EVERY view mode, not just Flight.
/// Refusals that fire outside the flight view — `RecoverControl` in
/// Map, LAUNCH in the VAB, throttle/stage/lease rejections anywhere —
/// push onto this same channel; gating it behind `mode == Flight`
/// made every one of them silent (P02-F1/P04-F1/P10-F12/P11-F2).
/// Drawn by `csp-view`'s `UiBuild` unconditionally; TTL aging lives in
/// `flight_present`, which runs in every mode, so the stack fades on
/// schedule wherever it is read.
pub fn notice_overlay(ctx: &egui::Context, lines: &[String]) {
    if lines.is_empty() {
        return;
    }
    let rect = ctx.content_rect();
    egui::Area::new(egui::Id::unique("flight_sfx"))
        .order(egui::Order::Foreground)
        .interactable(false)
        .fixed_pos(egui::pos2(rect.center().x, rect.top() + 48.0))
        .pivot(egui::Align2::CENTER_TOP)
        .show(ctx, |ui| {
            for line in lines {
                ui.label(
                    egui::RichText::new(format!("« {line} »"))
                        .font(egui::FontId::proportional(17.0))
                        .color(egui::Color32::from_rgb(255, 196, 64)),
                );
            }
        });
}

/// The M4c flight HUD (§3.7's presentation floor, upgraded): the
/// navball attitude instrument bottom-center (ball + heading tape),
/// a telemetry column at its left (altitude, surface/orbital speed,
/// vertical speed, heading/pitch/roll, throttle, parts, warp), and
/// the controls hint. Drawn by `csp-view`'s `UiBuild` while
/// `ViewMode::Flight` is active. The §3.7 cue flashes moved out to
/// [`notice_overlay`] — they render in every mode, not just here.
#[allow(clippy::too_many_lines)] // one HUD = one pass
pub fn flight_hud(ctx: &egui::Context, input: &FlightHudInput<'_>) {
    let rect = ctx.content_rect();
    const BALL_R: f32 = 64.0;
    let ball_center = egui::pos2(rect.center().x, rect.bottom() - BALL_R - 30.0);
    egui::Area::new(egui::Id::unique("flight_hud"))
        .order(egui::Order::Foreground)
        .interactable(false)
        .fixed_pos(egui::pos2(10.0, rect.bottom() - 170.0))
        .show(ctx, |ui| {
            if !input.has_vessel {
                ui.label(
                    egui::RichText::new("FLIGHT — no vessel in the bubble")
                        .color(egui::Color32::from_gray(200)),
                );
                ui.label(
                    egui::RichText::new(format!(
                        "[{}] VAB  ·  [{}] map  ·  [{}] controls",
                        input.keys.hint(settings::KeyAction::ToggleVab),
                        input.keys.hint(settings::KeyAction::ToggleMap),
                        input.keys.hint(settings::KeyAction::ToggleHelp),
                    ))
                    .color(egui::Color32::from_gray(150)),
                );
                return;
            }
            let mono = egui::FontId::monospace(13.0);
            let c = egui::Color32::from_gray(220);
            let dim = egui::Color32::from_gray(160);
            let row = |ui: &mut egui::Ui, tag: &str, val: String, col: egui::Color32| {
                ui.label(
                    egui::RichText::new(format!("{tag}  {val}"))
                        .font(mono.clone())
                        .color(col),
                );
            };
            if input.latched {
                ui.label(
                    egui::RichText::new(format!(
                        "ON PAD — [{}] to launch",
                        input.keys.hint(settings::KeyAction::ThrottleFull)
                    ))
                    .font(mono.clone())
                    .color(egui::Color32::from_rgb(140, 220, 140)),
                );
            }
            row(ui, "ALT", fmt_alt(input.alt_m), c);
            row(
                ui,
                "SPD",
                format!("{:>7.1} m/s", input.surface_speed_mps),
                c,
            );
            row(
                ui,
                "ORB",
                format!("{:>7.1} m/s", input.orbital_speed_mps),
                dim,
            );
            row(ui, "V/S", format!("{:>+7.1} m/s", input.vspeed_mps), c);
            // Pass-1 orbit readout — Ap/Pe altitudes + times-to-apsis
            // + period, "—" where the conic is open or unset.
            let opt = |v: Option<f64>| v.map_or("     —  ".to_string(), fmt_alt);
            row(ui, " AP", opt(input.apo_alt_m), c);
            row(ui, " PE", opt(input.pe_alt_m), c);
            let ts = |v: Option<f64>| {
                v.map_or("     —  ".to_string(), |s| format!("{:>6.0} s", s))
            };
            row(
                ui,
                "T-A/P",
                format!("{} /{}", ts(input.t_apo_s), ts(input.t_pe_s)),
                dim,
            );
            row(ui, "  T", ts(input.period_s), dim);
            let hdg = if input.heading_deg.is_finite() {
                format!("{:>7.1}°", input.heading_deg)
            } else {
                "    —  ".to_string()
            };
            row(ui, "HDG", hdg, c);
            let pr = if input.pitch_deg.is_finite() && input.roll_deg.is_finite() {
                format!("{:>+5.1}°/{:>+5.1}°", input.pitch_deg, input.roll_deg)
            } else {
                "    —  /    —  ".to_string()
            };
            row(ui, "P/R", pr, dim);
            ui.add(
                egui::ProgressBar::new(input.throttle as f32)
                    .text(format!("THR  {:>5.1}%", input.throttle * 100.0))
                    .desired_width(170.0),
            );
            row(
                ui,
                "PWR",
                format!("parts {}  ·  warp ×{}", input.parts, input.warp),
                dim,
            );
            // Staging — the launch-captured plan's remaining firings.
            // "single stage" is only honest when the craft never had
            // a firing to spend (`stages_total == 0`); an expended
            // multi-stage craft reads "0 firing(s) left" instead.
            row(
                ui,
                "STG",
                if input.stages_total == 0 {
                    "single stage".to_string()
                } else {
                    format!("{} firing(s) left", input.stages_left)
                },
                dim,
            );
            // Propellant gauges — one bar per resource still aboard
            // (tanks the staging drops leave with their nodes).
            for t in input.propellant {
                let frac = if t.capacity > 0.0 {
                    (t.units / t.capacity) as f32
                } else {
                    0.0
                };
                ui.add(
                    egui::ProgressBar::new(frac)
                        .text(format!(
                            "{}  {:.0}/{:.0}",
                            t.resource, t.units, t.capacity
                        ))
                        .desired_width(170.0),
                );
            }
            ui.label(
                egui::RichText::new(format!(
                    "[{}] controls  ·  [{}]/[{}] throttle [{}]/[{}]  ·  [{}] stage  ·  [{}] map  [{}] VAB",
                    input.keys.hint_labeled(settings::KeyAction::ToggleHelp, input.key_labels),
                    input
                        .keys
                        .hint_labeled(settings::KeyAction::ThrottleFull, input.key_labels),
                    input
                        .keys
                        .hint_labeled(settings::KeyAction::ThrottleCut, input.key_labels),
                    input
                        .keys
                        .hint_labeled(settings::KeyAction::ThrottleUp, input.key_labels),
                    input
                        .keys
                        .hint_labeled(settings::KeyAction::ThrottleDown, input.key_labels),
                    input
                        .keys
                        .hint_labeled(settings::KeyAction::Stage, input.key_labels),
                    input
                        .keys
                        .hint_labeled(settings::KeyAction::ToggleMap, input.key_labels),
                    input
                        .keys
                        .hint_labeled(settings::KeyAction::ToggleVab, input.key_labels),
                ))
                .color(egui::Color32::from_gray(140)),
            );
        });
    // The navball — a raw painter draw beside the Area so it can sit
    // dead-center-bottom like the flight instrument it is.
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::unique("flight_navball"),
    ));
    if input.has_vessel {
        navball::navball(&painter, ball_center, BALL_R, &input.navball);
        navball::heading_tape(
            &painter,
            ball_center + egui::vec2(0.0, BALL_R + 4.0),
            BALL_R * 2.0,
            input.heading_deg,
        );
    }
}

/// The controls-help card — a toggleable window listing every bound
/// action *from the live [`settings::Keymap`]* (so rebinding keeps it
/// truthful) plus the fixed mouse controls. `open` mirrors the window's
/// close button back into the session flag.
pub fn help_panel(
    ctx: &egui::Context,
    keymap: &settings::Keymap,
    key_labels: &std::collections::BTreeMap<String, String>,
    open: &mut bool,
) {
    let mut o = *open;
    egui::Window::new("Controls")
        .open(&mut o)
        .anchor(egui::Align2::RIGHT_CENTER, egui::vec2(-12.0, 0.0))
        .resizable(false)
        .collapsible(false)
        .show(ctx, |ui| {
            ui.label("Keyboard (rebindable in Settings → Key bindings)");
            egui::Grid::new("help_keys").num_columns(2).show(ui, |ui| {
                for action in settings::KeyAction::ALL {
                    let keys = keymap
                        .bindings
                        .get(action)
                        .map(|v| {
                            v.iter()
                                .map(|k| k.display_label(key_labels))
                                .collect::<Vec<_>>()
                                .join(" / ")
                        })
                        .unwrap_or_default();
                    ui.label(
                        egui::RichText::new(if keys.is_empty() {
                            "—".to_string()
                        } else {
                            keys
                        })
                        .monospace(),
                    );
                    ui.label(action.label());
                    ui.end_row();
                }
            });
            ui.separator();
            ui.label("Mouse");
            egui::Grid::new("help_mouse").num_columns(2).show(ui, |ui| {
                for (act, desc) in [
                    ("drag", "orbit camera"),
                    ("wheel", "zoom (to cursor in map)"),
                    ("RMB / MMB drag", "pan (map)"),
                    ("click (map)", "focus body / vessel"),
                    ("click (VAB)", "select part"),
                ] {
                    ui.label(egui::RichText::new(act).monospace());
                    ui.label(desc);
                    ui.end_row();
                }
            });
        });
    *open = o;
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
///
/// `tick_hz` converts between sim ticks and the seconds the fields
/// speak — the caller feeds `csp_core::TICK_HZ` (§2.1 keeps this crate
/// off csp-core, so the rate arrives as a parameter rather than a
/// hardcoded constant that could drift from the sim's real one).
pub fn maneuver_panel(
    ctx: &egui::Context,
    tick: u64,
    tick_hz: f64,
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
            let eta = n.0.saturating_sub(tick) as f64 / tick_hz;
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
                let ticks = (state.t_plus_s.max(0.0) * tick_hz) as u64;
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
