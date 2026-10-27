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

/// The Game menu — save slots, load, revert-to-launch (with the
/// PLAN §3.4 "not a reroll" disclosure), new game.
pub mod game;

/// The M10 first-flight tutorial (PLAN M10 exit row): the
/// [`tutorial::TutorialState`] session model — the step table and the
/// real-progress predicates — plus the [`tutorial::tutorial_overlay`]
/// checklist panel.
pub mod tutorial;

/// The flight attitude instrument — the navball ball + heading tape,
/// pure egui over body-frame staged vectors.
pub mod navball;

/// The M7/M9b progression surface (PLAN §3.2–§3.6): the Program
/// window — economy readout, contract board, tech tree, the focused
/// vessel's experiment/data actions, and per-part reliability — plus
/// the always-on funds/science status strip.
pub mod program;

pub use settings::{
    AudioSettings, DifficultySettings, GameplaySettings, GraphicsQuality, GraphicsSettings,
    KeyAction, KeyBinding, Keymap, Settings, SettingsPanelOutput, SettingsPanelState,
};
pub use tutorial::{STEP_LAUNCH, STEPS, TutorialInput, TutorialState, TutorialStep};
pub use vab::{ModeSelect, VabSession};

// ---------------------------------------------------------------------------
// Shared HUD chrome (the readable-overlay rule): every floating strip
// of HUD text — notice cues, the telemetry column, status strips, hint
// bars — wears the same dark translucent chip, so a readout's contrast
// is decided by the panel and never by whatever sky/terrain/scene
// happens to be behind it.
// ---------------------------------------------------------------------------

/// The chip fill — near-opaque dark navy.
pub(crate) const HUD_CHIP_FILL: egui::Color32 =
    egui::Color32::from_rgba_unmultiplied_const(9, 13, 19, 210);

/// The chip's 1-px hairline — a faint cool tint that separates the
/// panel from the scene without reading as a window border.
pub(crate) const HUD_CHIP_EDGE: egui::Color32 =
    egui::Color32::from_rgba_unmultiplied_const(140, 185, 220, 60);

/// Text→chip-edge padding for painter-drawn chips.
pub(crate) const HUD_CHIP_PAD: egui::Vec2 = egui::Vec2::new(9.0, 5.0);

/// The notice-cue amber — kept on the dark chip, where it holds its
/// contrast; raw over the scene it washed out.
pub(crate) const NOTICE_AMBER: egui::Color32 = egui::Color32::from_rgb(255, 202, 84);

/// The shared HUD panel frame — the `Ui`-side of the chip rule
/// ([`paint_hud_chip`] is the `Painter`-side). Wraps a floating
/// readout's contents in the dark translucent backing + hairline.
pub(crate) fn hud_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(HUD_CHIP_FILL)
        .stroke(egui::Stroke::new(1.0, HUD_CHIP_EDGE))
        .corner_radius(egui::CornerRadius::same(6))
        .inner_margin(egui::Margin::symmetric(10, 7))
}

/// The `Painter`-side of the chip rule — draws the dark backing +
/// hairline behind `text_rect` (expanded by [`HUD_CHIP_PAD`]) and
/// returns the chip rect. Draw the text *after* this call.
pub(crate) fn paint_hud_chip(painter: &egui::Painter, text_rect: egui::Rect) -> egui::Rect {
    let chip = text_rect.expand2(HUD_CHIP_PAD);
    painter.rect_filled(chip, egui::CornerRadius::same(6), HUD_CHIP_FILL);
    painter.rect_stroke(
        chip,
        egui::CornerRadius::same(6),
        egui::Stroke::new(1.0, HUD_CHIP_EDGE),
        egui::StrokeKind::Inside,
    );
    chip
}

/// `painter.text` wearing the shared chip: lays `text` out no-wrap
/// (single line — the chip widens, the text never folds), backs it
/// with the dark backing, then draws it. `pos`/`align` anchor exactly
/// like `painter.text` did, so call sites keep their coordinates.
pub(crate) fn paint_chip_text(
    painter: &egui::Painter,
    pos: egui::Pos2,
    align: egui::Align2,
    text: impl Into<String>,
    font: egui::FontId,
    color: egui::Color32,
) {
    let galley = painter.layout_no_wrap(text.into(), font, color);
    let text_rect = align.anchor_size(pos, galley.size());
    paint_hud_chip(painter, text_rect);
    painter.galley(text_rect.min, galley, color);
}

/// One SOI-scan candidate's reported stride — the §2.6.6-item-4 `h`,
/// flattened to plain data by `csp-view` (`csp-ui` never sees sim
/// types, so `csp_sim::orbit::SoiScan` arrives as this row).
#[derive(Clone, Debug)]
pub struct SoiScanRow {
    /// The candidate body's data id ("terrin", "rask").
    pub body: String,
    /// `h` in whole sim ticks — a residence shorter than this may be
    /// missed by the scan (that IS the guarantee, stated plainly).
    pub stride_ticks: u64,
    /// The relative-speed bound `h` was computed from (m/s) — carried
    /// so the report shows *why* `h` is what it is ("increasing
    /// `v_rel_max` fidelity reduces `h`").
    pub v_rel_max_mps: f64,
}

/// The debug panel's §2.6.6-item-4 section — the resolved-focus
/// vessel's SOI scan report, mirrored plain. `unavailable` exists so
/// a missing report always says *why* ("not on rails" reads very
/// differently from "no SOIs exist"): SOI events are only ever
/// scheduled while on rails, so a bubble/latched focus has no `h`.
#[derive(Clone, Debug)]
pub struct SoiScanReport {
    /// The vessel the report covers (`None` = nothing focused).
    pub vessel: Option<u64>,
    /// Per-candidate `h` rows — the scan's own values.
    pub rows: Vec<SoiScanRow>,
    /// Why `rows` is empty; `None` while the report is live.
    pub unavailable: Option<String>,
}

impl Default for SoiScanReport {
    /// The pre-staging state: nothing focused, no live report — shown
    /// as "unavailable", never as an empty candidate set.
    fn default() -> Self {
        Self {
            vessel: None,
            rows: Vec::new(),
            unavailable: Some("no focused vessel".to_string()),
        }
    }
}

/// Everything the M0 debug panel needs — mirrors and session values only.
///
/// Populated per frame by the `UiBuild` system in `csp-view`. Every field is
/// either a presentation mirror of sim state (`sim_seconds`, `tick`,
/// `warp_level`, `ticks_last_frame`, `soi_scan`) or session/presentation
/// data that never reaches the sim (`frame_dt_ms`, adapter strings,
/// `warp_requested`).
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
    /// The §2.6.6-item-4 SOI scan report for the focused vessel —
    /// `SoiScanMirror` staged each frame by `csp-view` from
    /// `World::soi_scan_strides`.
    pub soi_scan: &'a SoiScanReport,
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
        ui.separator();
        // §2.6.6 item 4: the SOI scan's stride guarantee is only
        // honest while `h` is observable — a residence shorter than
        // `h` may be missed, so the panel prints the scan's own
        // strides (mirrored, never recomputed here). An absent report
        // always says why — "not on rails" is a different fact from
        // "no SOI candidates".
        let scan = input.soi_scan;
        match &scan.unavailable {
            Some(reason) => {
                ui.label(format!("soi scan: {reason}"));
            }
            None => {
                let subject = scan
                    .vessel
                    .map_or_else(|| "focused".to_string(), |v| format!("vessel {v}"));
                ui.label(format!(
                    "soi scan ({subject}) — may miss a residence shorter than h:"
                ));
                for row in &scan.rows {
                    ui.label(format!(
                        "  {}: h = {} ticks ({:.2} s) @ v_rel_max {:.1} m/s",
                        row.body,
                        row.stride_ticks,
                        row.stride_ticks as f64 / csp_core::TICK_HZ as f64,
                        row.v_rel_max_mps,
                    ));
                }
            }
        }
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
/// the app recorded. Returns the drawn window rect — the host masks the
/// VAB's pointer input against it (a click on the dialog must not reach
/// the craft behind it).
pub fn crash_dialog(ctx: &egui::Context, reason: &str, bundle: Option<&str>) -> egui::Rect {
    let screen = ctx.content_rect();
    egui::Window::new("Simulation halted")
        .collapsible(false)
        .resizable(false)
        .default_pos(egui::pos2(
            screen.center().x - 230.0,
            screen.center().y - 90.0,
        ))
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
        })
        .map(|r| r.response.rect)
        .unwrap_or(egui::Rect::NOTHING)
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
    /// A surface site (launch complex / recovery field) — styled like
    /// an apsis mark: small and secondary to object names.
    Site,
    /// A predicted SOI encounter (`predict_encounter`) — styled like
    /// an apsis mark; the text carries the Enter/Exit + approach.
    Encounter,
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
#[allow(clippy::too_many_lines)] // labels + readout + hint bar in one pass
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
            // hint bar's way.
            if let Some(o) = orbit {
                paint_orbit_readout(painter, rect, o);
            }

            let pan = |a: settings::KeyAction| {
                keys.first_binding_label(a, key_labels)
                    .unwrap_or_else(|| "—".to_string())
            };
            // The hint bar gets the chip too — a long gray line raw on
            // the scene illegible-mixed into whatever passed under it.
            paint_chip_text(
                painter,
                egui::pos2(
                    rect.left() + 10.0 + HUD_CHIP_PAD.x,
                    rect.bottom() - 8.0 - HUD_CHIP_PAD.y,
                ),
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
                egui::Color32::from_gray(215),
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
        MapLabelKind::Apsis | MapLabelKind::Site | MapLabelKind::Encounter => {
            (egui::FontId::proportional(10.5), 5.0)
        }
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

/// The map's top-right orbit readout — the focused conic's apsides +
/// period. Radii print as surface altitudes when the parent radius is
/// known, else raw radii (star-center orbits have no "surface" to
/// subtract). Extracted from [`map_overlay`] under the line budget;
/// the block wears the shared chip — it used to sit raw on the scene.
fn paint_orbit_readout(painter: &egui::Painter, rect: egui::Rect, o: &MapOrbitInfo) {
    let alt = |r: f64| o.parent_radius_m.map_or(r, |rad| r - rad);
    let lines = [
        (format!("{}  orbiting  {}", o.name, o.parent_name), 215),
        (
            match o.apo_r_m {
                Some(r) => format!("Ap   {}", fmt_distance(alt(r))),
                None => "Ap   — escape".to_string(),
            },
            200,
        ),
        (format!("Pe   {}", fmt_distance(alt(o.pe_r_m))), 200),
        (
            match o.period_s {
                Some(t) => format!("T    {}", fmt_period(t)),
                None => "T    —".to_string(),
            },
            180,
        ),
    ];
    let mono = egui::FontId::monospace(12.5);
    let right = rect.right() - 12.0;
    let line_h = 15.0;
    let galleys: Vec<std::sync::Arc<egui::Galley>> = lines
        .iter()
        .map(|(line, gray)| {
            painter.layout_no_wrap(line.clone(), mono.clone(), egui::Color32::from_gray(*gray))
        })
        .collect();
    let line_rect = |i: usize, g: &egui::Galley| {
        egui::Rect::from_min_size(
            egui::pos2(right - g.size().x, rect.top() + 34.0 + i as f32 * line_h),
            g.size(),
        )
    };
    let text_rect = galleys
        .iter()
        .enumerate()
        .fold(None::<egui::Rect>, |acc, (i, g)| {
            let r = line_rect(i, g);
            Some(acc.map_or(r, |u: egui::Rect| u.union(r)))
        });
    if let Some(tr) = text_rect {
        paint_hud_chip(painter, tr);
    }
    for (i, g) in galleys.iter().enumerate() {
        painter.galley(line_rect(i, g).min, g.clone(), egui::Color32::WHITE);
    }
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
    /// The vessel the displayed stage plan belongs to when it ISN'T
    /// the focused one — the view's plan readout falls back to the
    /// newest live plan when the focus holds none, and the stage key
    /// fires that plan's vessel. `Some(id)` names the owner in the
    /// STG row so the readout can't pass another vessel's firings off
    /// as this craft's (C19-F6).
    pub stages_plan_vessel: Option<u64>,
    /// Live wet mass of the focused vessel (kg).
    pub mass_kg: f64,
    /// Thrust delivered at the commanded throttle (N) —
    /// coverage·throttle over the live engines.
    pub thrust_n: f64,
    /// Full-throttle deliverable thrust (N).
    pub thrust_full_n: f64,
    /// Thrust-to-weight at the commanded throttle vs local gravity.
    pub twr: Option<f64>,
    /// Full-throttle TWR vs body-surface gravity — the pad figure.
    pub twr_surface: Option<f64>,
    /// Thrust acceleration at the commanded throttle (m/s²).
    pub accel_mps2: Option<f64>,
    /// `accel_mps2` in g's (standard gravity — view-side conversion).
    pub accel_gee: Option<f64>,
    /// Mass-flow-weighted vacuum Isp over the metered engines (s).
    pub isp_s: Option<f64>,
    /// Propellant mass flow at the commanded throttle (kg/s).
    pub mass_flow_kg_s: Option<f64>,
    /// Seconds until the limiting propellant empties at the commanded
    /// throttle — `None` closed-throttle, unmetered, or no engines.
    pub fuel_depletion_s: Option<f64>,
    /// The same at full throttle (s) — the burn-time figure.
    pub burn_full_s: Option<f64>,
    /// Δv still aboard — the attached set's burn plus the remaining
    /// stage drops' (m/s).
    pub delta_v_mps: Option<f64>,
    /// The staged-drops portion of `delta_v_mps` (m/s).
    pub delta_v_stages_mps: f64,
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

/// Countdown-clock formatting for the HUD's time readouts (T-to-apsis,
/// fuel depletion, burn time): `M:SS` while the wait is under ten
/// hours, `fmt_period`'s coarse steps beyond.
fn fmt_clock(s: f64) -> String {
    let a = s.abs();
    if a < 36_000.0 {
        let m = (a / 60.0).floor();
        // Floor the seconds — `{sec:02.0}` rounds a raw remainder, so
        // 119.7 s printed "1:60" and 3599.9 s printed "59:60" (C23-F4).
        let sec = (a - m * 60.0).floor();
        format!("{m:.0}:{sec:02.0}")
    } else {
        fmt_period(s)
    }
}

/// Mass formatting for the vessel column — kg under ten tonnes, then t.
fn fmt_mass(kg: f64) -> String {
    if kg.abs() < 10_000.0 {
        format!("{kg:.0} kg")
    } else {
        format!("{:.2} t", kg / 1e3)
    }
}

/// Thrust formatting — N under a kilonewton, then kN.
fn fmt_force(n: f64) -> String {
    if n.abs() < 1_000.0 {
        format!("{n:.0} N")
    } else {
        format!("{:.1} kN", n / 1e3)
    }
}

/// Speed formatting for Δv — m/s under 10 km/s, then km/s.
fn fmt_speed(v: f64) -> String {
    if v.abs() < 10_000.0 {
        format!("{v:.0} m/s")
    } else {
        format!("{:.2} km/s", v / 1e3)
    }
}

/// The honest blank — one em-dash for every unavailable readout
/// (parked, no engine, open orbit, closed throttle).
fn fmt_opt(v: Option<f64>, f: impl Fn(f64) -> String) -> String {
    v.map_or("—".to_string(), f)
}

/// The "STG" row's text — factored out for the test. "single stage"
/// is only honest when the craft never had a firing to spend
/// (`stages_total == 0`); an expended multi-stage craft reads
/// "0 firing(s) left". `stages_plan_vessel` is the C19-F6 owner
/// label: the view's plan readout can borrow ANOTHER vessel's plan
/// when the focus holds none, and the stage key fires that vessel —
/// the row names the owner so the borrowed count isn't mistaken for
/// this craft's.
fn stages_label(input: &FlightHudInput<'_>) -> String {
    let base = if input.stages_total == 0 {
        "single stage".to_string()
    } else {
        format!("{} firing(s) left", input.stages_left)
    };
    match input.stages_plan_vessel {
        Some(v) => format!("{base} — vessel {v}'s plan"),
        None => base,
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
///
/// Readability: the cues wear the shared dark chip ([`hud_frame`]) —
/// bare amber text washed out over bright terrain/sky — and every line
/// is [`egui::TextWrapMode::Extend`], i.e. single-line: the bar widens
/// to fit the longest cue instead of wrapping it to two rows.
///
/// Returns the chip's rect (`None` when there is nothing to draw) so
/// the host can mask it: the Area is `interactable(false)`, so egui
/// never consumes a click on a cue line — an unmasked right-click on
/// the chip would resolve as a viewport gesture and delete the VAB
/// part behind it (C17-F7, the same class C14-F2's window masking
/// closed).
pub fn notice_overlay(ctx: &egui::Context, lines: &[String]) -> Option<egui::Rect> {
    if lines.is_empty() {
        return None;
    }
    let rect = ctx.content_rect();
    let shown = egui::Area::new(egui::Id::unique("flight_sfx"))
        .order(egui::Order::Foreground)
        .interactable(false)
        .fixed_pos(egui::pos2(rect.center().x, rect.top() + 44.0))
        .pivot(egui::Align2::CENTER_TOP)
        .show(ctx, |ui| {
            hud_frame().show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    for line in lines {
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(format!("« {line} »"))
                                    .font(egui::FontId::proportional(17.0))
                                    .color(NOTICE_AMBER),
                            )
                            .wrap_mode(egui::TextWrapMode::Extend)
                            .selectable(false),
                        );
                    }
                });
            });
        });
    Some(shown.response.rect)
}

/// The M4c flight HUD (§3.7's presentation floor, upgraded): the
/// navball attitude instrument bottom-center (ball + heading tape),
/// a grouped telemetry grid at its left — ORBIT (Ap/Pe altitudes,
/// times-to-apsis, period), SURFACE (altitude, surface/orbital speed,
/// vertical speed, heading, pitch/roll), VESSEL (mass, current +
/// full-throttle thrust, TWR at local and surface gravity, thrust
/// acceleration in m/s² and g's, Δv remaining incl. the staged
/// drops' share), and PROP (throttle + tank gauges, mass flow,
/// time-to-fuel-depletion, burn time, Isp) — then staging, parts,
/// warp, and the controls hint. Every propulsion figure derives from
/// live sim state each frame; unavailable readouts print "—" (parked
/// conic, closed throttle, no metered engine, open orbit). Drawn by
/// `csp-view`'s `UiBuild` while `ViewMode::Flight` is active. The
/// §3.7 cue flashes moved out to [`notice_overlay`] — they render in
/// every mode, not just here.
#[allow(clippy::too_many_lines)] // one HUD = one pass
pub fn flight_hud(ctx: &egui::Context, input: &FlightHudInput<'_>) {
    let rect = ctx.content_rect();
    const BALL_R: f32 = 64.0;
    let ball_center = egui::pos2(rect.center().x, rect.bottom() - BALL_R - 30.0);
    // `movable` — the readout block is a real overlay now: grab it
    // anywhere on its surface and drag it out of the way (KSP-style).
    egui::Area::new(egui::Id::unique("flight_hud"))
        .order(egui::Order::Foreground)
        .movable(true)
        .default_pos(egui::pos2(rect.left() + 10.0, rect.bottom() - 170.0))
        .show(ctx, |ui| {
            // The shared chip: telemetry used to float gray text on the
            // scene — unreadable over bright terrain. The panel backs
            // every readout now (the navball is its own opaque draw).
            hud_frame().show(ui, |ui| {
            // Every label in this Area is `.selectable(false)`: egui
            // 0.37's `selectable_labels` default gives `ui.label` a
            // `click_and_drag` sense, and labels created after the
            // Area's whole-rect move widget are hit-tested on top of
            // it — a selectable label claims the drag press, so the
            // panel cannot be grabbed by its own text (C23-F1).
            if !input.has_vessel {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new("FLIGHT — no vessel in the bubble")
                            .color(egui::Color32::from_gray(225)),
                    )
                    .selectable(false),
                );
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(format!(
                            "[{}] VAB  ·  [{}] map  ·  [{}] controls",
                            input.keys.hint(settings::KeyAction::ToggleVab),
                            input.keys.hint(settings::KeyAction::ToggleMap),
                            input.keys.hint(settings::KeyAction::ToggleHelp),
                        ))
                        .color(egui::Color32::from_gray(185)),
                    )
                    .selectable(false),
                );
                return;
            }
            let mono = egui::FontId::monospace(14.0);
            let c = egui::Color32::from_gray(225);
            let dim = egui::Color32::from_gray(185);
            let head = egui::Color32::from_rgb(150, 195, 225);
            // The readout grid: two `TAG value` cells per row — left
            // cell a 4-char tag + 12-wide value, right cell a 6-char
            // tag + 12-wide value (35 columns of mono).
            let pair = |ui: &mut egui::Ui,
                        l: &str,
                        lv: String,
                        r: &str,
                        rv: String,
                        col: egui::Color32| {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(format!("{l:<4}{lv:>12}  {r:<6}{rv:>12}"))
                            .font(mono.clone())
                            .color(col),
                    )
                    .selectable(false),
                );
            };
            let row = |ui: &mut egui::Ui, tag: &str, val: String, col: egui::Color32| {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(format!("{tag:<4}{val:>31}"))
                            .font(mono.clone())
                            .color(col),
                    )
                    .selectable(false),
                );
            };
            let sect = |ui: &mut egui::Ui, name: &str| {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(format!("── {name} ─────────────────────────────"))
                            .font(mono.clone())
                            .color(head),
                    )
                    .selectable(false),
                );
            };
            if input.latched {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(format!(
                            "ON PAD — [{}] to launch",
                            input.keys.hint(settings::KeyAction::ThrottleFull)
                        ))
                        .font(mono.clone())
                        .color(egui::Color32::from_rgb(140, 220, 140)),
                    )
                    .selectable(false),
                );
            }
            // Orbit — the osculating conic at the CoM: apsides,
            // times-to-apsis, period. "—" where the conic is open or
            // the vessel rides surface rails.
            sect(ui, "ORBIT");
            pair(
                ui,
                "AP",
                fmt_opt(input.apo_alt_m, fmt_alt),
                "PE",
                fmt_opt(input.pe_alt_m, fmt_alt),
                c,
            );
            pair(
                ui,
                "T-AP",
                fmt_opt(input.t_apo_s, fmt_clock),
                "T-PE",
                fmt_opt(input.t_pe_s, fmt_clock),
                dim,
            );
            row(ui, "T", fmt_opt(input.period_s, fmt_period), dim);
            // Surface frame — altitude, surface/orbital speed, vertical
            // speed, and the attitude readouts.
            sect(ui, "SURFACE");
            pair(
                ui,
                "ALT",
                fmt_alt(input.alt_m),
                "V/S",
                format!("{:>+6.1} m/s", input.vspeed_mps),
                c,
            );
            pair(
                ui,
                "SPD",
                format!("{:>6.1} m/s", input.surface_speed_mps),
                "ORB",
                format!("{:>6.1} m/s", input.orbital_speed_mps),
                c,
            );
            let hdg = if input.heading_deg.is_finite() {
                format!("{:>6.1}°", input.heading_deg)
            } else {
                "—".to_string()
            };
            let pr = if input.pitch_deg.is_finite() && input.roll_deg.is_finite() {
                format!("{:>+4.0}°/{:>+4.0}°", input.pitch_deg, input.roll_deg)
            } else {
                "—/—".to_string()
            };
            pair(ui, "HDG", hdg, "P/R", pr, dim);
            // Vessel — mass, the live thrust/TWR/acceleration readouts
            // off the staged propulsion walk, and Δv remaining (the
            // rocket equation over live tanks + isp + mass, including
            // the stage plan's unfired drops).
            sect(ui, "VESSEL");
            pair(
                ui,
                "MASS",
                fmt_mass(input.mass_kg),
                "FN",
                fmt_force(input.thrust_n),
                c,
            );
            pair(
                ui,
                "TWR",
                fmt_opt(input.twr, |v| format!("{v:.2}")),
                "MAXF",
                fmt_force(input.thrust_full_n),
                c,
            );
            pair(
                ui,
                "TWR·SL",
                fmt_opt(input.twr_surface, |v| format!("{v:.2}")),
                "ACC",
                fmt_opt(input.accel_mps2, |v| format!("{v:.1} m/s²")),
                c,
            );
            pair(
                ui,
                "G",
                fmt_opt(input.accel_gee, |v| format!("{v:.2}")),
                "ΔV",
                fmt_opt(input.delta_v_mps, fmt_speed),
                c,
            );
            if input.delta_v_stages_mps > 0.0 {
                row(
                    ui,
                    "STG+",
                    format!("{} m/s in stages", input.delta_v_stages_mps as u64),
                    dim,
                );
            }
            // Propellant — the tank gauges plus the flow/depletion
            // readouts at the commanded throttle (and at full for the
            // burn-time figure).
            sect(ui, "PROP");
            ui.add(
                egui::ProgressBar::new(input.throttle as f32)
                    .text(format!("THR  {:>5.1}%", input.throttle * 100.0))
                    .desired_width(280.0),
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
                        .desired_width(280.0),
                );
            }
            pair(
                ui,
                "FLOW",
                fmt_opt(input.mass_flow_kg_s, |v| format!("{v:.1} kg/s")),
                "DEP",
                fmt_opt(input.fuel_depletion_s, fmt_clock),
                c,
            );
            pair(
                ui,
                "BURN",
                fmt_opt(input.burn_full_s, fmt_clock),
                "ISP",
                fmt_opt(input.isp_s, |v| format!("{v:.0} s")),
                dim,
            );
            // Staging — the launch-captured plan's remaining firings.
            row(ui, "STG", stages_label(input), dim);
            row(
                ui,
                "",
                format!("parts {}  ·  warp ×{}", input.parts, input.warp),
                dim,
            );
            ui.add(
                egui::Label::new(
                    egui::RichText::new(format!(
                        "[{}] controls  ·  [{}]/[{}] throttle [{}]/[{}]  ·  [{}] stage  ·  [{}] map  [{}] VAB  ·  [{}] program  ·  RMB on a part: actions",
                        input
                            .keys
                            .hint_labeled(settings::KeyAction::ToggleHelp, input.key_labels),
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
                        input
                            .keys
                            .hint_labeled(settings::KeyAction::OpenProgram, input.key_labels),
                    ))
                    .color(egui::Color32::from_gray(180)),
                )
                .selectable(false),
            );
            });
        });
    // The navball — its own movable instrument Area (ball + heading
    // tape), drag-anywhere like the readout block. The allocated box
    // leaves the tape's 16 px strip under the ball.
    if input.has_vessel {
        const TAPE_H: f32 = 20.0;
        let nav_size = egui::vec2(BALL_R * 2.0 + 8.0, BALL_R * 2.0 + TAPE_H + 4.0);
        egui::Area::new(egui::Id::unique("flight_navball"))
            .order(egui::Order::Foreground)
            .movable(true)
            // `default_size` must be the real content size: without it
            // the first-open sizing pass clamps `default_pos` against
            // egui's nominal 600×400 `default_area_size`, parking the
            // ball ~242 px above the bottom edge forever on narrow
            // windows (the real 136×152 rect fits under the clamp, so
            // it never self-corrects — C23-F2).
            .default_size(nav_size)
            .default_pos(egui::pos2(
                ball_center.x - nav_size.x * 0.5,
                rect.bottom() - nav_size.y - 6.0,
            ))
            .show(ctx, |ui| {
                let (box_rect, _resp) = ui.allocate_exact_size(nav_size, egui::Sense::hover());
                let center = egui::pos2(box_rect.center().x, box_rect.top() + 4.0 + BALL_R);
                navball::navball(ui.painter(), center, BALL_R, &input.navball);
                navball::heading_tape(
                    ui.painter(),
                    center + egui::vec2(0.0, BALL_R + 4.0),
                    BALL_R * 2.0,
                    input.heading_deg,
                );
            });
    }
}

/// The controls-help card — a toggleable window listing every bound
/// action *from the live [`settings::Keymap`]* (so rebinding keeps it
/// truthful) plus the fixed mouse controls. `open` mirrors the window's
/// close button back into the session flag. Returns the drawn window
/// rect — the host masks the VAB's pointer input against it.
pub fn help_panel(
    ctx: &egui::Context,
    keymap: &settings::Keymap,
    key_labels: &std::collections::BTreeMap<String, String>,
    open: &mut bool,
) -> egui::Rect {
    let mut o = *open;
    // Parks right-center on first open; the title bar drags it
    // anywhere after that.
    let screen = ctx.content_rect();
    let rect = egui::Window::new("Controls")
        .open(&mut o)
        .default_pos(egui::pos2(
            screen.right() - 402.0,
            screen.center().y - 210.0,
        ))
        .resizable(false)
        .collapsible(false)
        .show(ctx, |ui| {
            ui.label("Keyboard (rebindable in Settings → Key bindings)");
            egui::Grid::new("help_keys").num_columns(2).show(ui, |ui| {
                for action in settings::KeyAction::ALL {
                    ui.label(
                        egui::RichText::new(keymap.bindings_label(*action, key_labels, " / "))
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
                    (
                        "RMB (flight)",
                        "part actions — run science, deploy chute, recover",
                    ),
                ] {
                    ui.label(egui::RichText::new(act).monospace());
                    ui.label(desc);
                    ui.end_row();
                }
            });
        })
        .map(|r| r.response.rect)
        .unwrap_or(egui::Rect::NOTHING);
    *open = o;
    rect
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

// ---------------------------------------------------------------------------
// W2.4 planning panel — the KAC-equivalent surface (PLAN §4 v1/M5):
// player alarms + the transfer-window planner, shown in map mode.
// ---------------------------------------------------------------------------

/// Where one staged alarm sits in its lifecycle — plain data for the
/// panel's row styling (`csp-ui` never sees sim types).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlarmState {
    /// Scheduled and counting down.
    Armed,
    /// The event fired — surfaced, awaiting dismissal.
    Fired,
    /// Its tick passed with no fired event observed — the command
    /// never landed (a drained rejection, or a past-tick request the
    /// sim clamped differently). Reported honestly, not as a fire.
    Lapsed,
}

/// One alarm/watch row — tick + label resolved into a countdown by
/// `csp-view` against the mirrored clock.
#[derive(Clone, Debug)]
pub struct AlarmRow {
    /// The label the player gave (or the panel generated — "Pe",
    /// "terrin → duva window").
    pub label: String,
    /// Seconds until it fires; negative once past.
    pub eta_s: f64,
    /// Armed / fired / lapsed.
    pub state: AlarmState,
    /// Index into the view's alarm book — what `DismissAlarm` names.
    pub index: usize,
}

/// One vessel row in the departure picker — eligibility staged by
/// `csp-view` so a click on an unservable vessel is never offered.
#[derive(Clone, Debug)]
pub struct TransferVessel {
    /// The `VesselId`.
    pub id: u64,
    /// The body its conic orbits.
    pub parent: String,
    /// `true` = on rails, orbiting the selected origin, node-eligible.
    pub eligible: bool,
    /// The honesty line for ineligible rows ("in physics bubble —
    /// coast to rails first", "orbits rask, not terrin").
    pub note: String,
}

/// The staged `departure_node` result as plain data.
#[derive(Clone, Debug)]
pub struct PlanNodeRow {
    /// The vessel the node was resolved for.
    pub vessel: u64,
    /// Seconds until the burn tick.
    pub in_s: f64,
    /// Total Δv of the departure burn (m/s).
    pub dv_mps: f64,
    /// The composed solve's verified residual — how far the burn's
    /// achieved patched trajectory lands from the destination's
    /// centre at arrival (m). `Some` is an arrival claim the panel
    /// may show; `None` (the legacy path) means no arrival was
    /// verified — the encounter line must not read as this burn's
    /// outcome.
    pub arrival_miss_m: Option<f64>,
}

/// The staged `predict_encounter` result as plain data.
#[derive(Clone, Debug)]
pub struct PlanEncounterRow {
    /// The body whose SOI is crossed.
    pub body: String,
    /// `true` = `Enter` (an approach), `false` = `Exit`.
    pub enter: bool,
    /// Seconds until the crossing.
    pub in_s: f64,
    /// Approach periapsis radius (m) — `Enter` only.
    pub periapsis_m: Option<f64>,
    /// Approach v∞ (m/s) — `Enter` on a hyperbolic approach only.
    pub v_inf_mps: Option<f64>,
    /// `periapsis < surface radius` — `None` when the model carries
    /// no radius for the body.
    pub impacts: Option<bool>,
}

/// The staged transfer plan, plain data — the panel's display copy of
/// `csp-view`'s `PlannedTransfer` mirror.
#[derive(Clone, Debug)]
pub struct PlanRow {
    /// Bodies carried through from the query.
    pub origin: String,
    /// See [`Self::origin`].
    pub destination: String,
    /// Seconds until the optimal departure tick.
    pub depart_in_s: f64,
    /// The transfer's duration (s).
    pub duration_s: f64,
    /// Departure burn cost under the query's park-orbit model (m/s).
    pub dv_depart_mps: f64,
    /// Arrival cost — v∞ at the destination (m/s).
    pub dv_arrive_mps: f64,
    /// The resolved departure burn for the picked vessel, when the
    /// ejection geometry solved (`departure_node`).
    pub node: Option<PlanNodeRow>,
    /// Why no node resolved — the ejection's own refusal ("parking
    /// orbit not closed", "burn precedes now").
    pub node_error: Option<String>,
    /// The conic's predicted first encounter (`predict_encounter`
    /// with the origin's egress excluded).
    pub encounter: Option<PlanEncounterRow>,
}

/// Everything the planning panel needs — mirrors and session values
/// only, populated by `csp-view`'s `UiBuild`.
#[derive(Clone, Debug)]
pub struct PlanningPanelInput<'a> {
    /// The mirrored sim tick — countdowns and `SetAlarm` tick math.
    pub tick: u64,
    /// `csp_core::TICK_HZ` as f64 (§2.1 keeps this crate off csp-core).
    pub tick_hz: f64,
    /// Body ids carrying orbits — the transfer endpoint pickers.
    pub bodies: &'a [String],
    /// Vessels for the departure picker (eligibility staged).
    pub vessels: &'a [TransferVessel],
    /// Armed/fired/lapsed alarm rows.
    pub alarms: &'a [AlarmRow],
    /// The last computed plan's display row.
    pub plan: Option<&'a PlanRow>,
    /// The last planner failure's reason.
    pub plan_error: Option<&'a str>,
    /// The map's focused vessel (id) — the apsis/node alarm targets.
    pub focus_vessel: Option<u64>,
    /// Whether the focused vessel carries a scheduled node — gates
    /// the "at node" quick-alarm.
    pub focus_has_node: bool,
}

/// What the panel asked `csp-view` to do — resolved into `SimCommand`s
/// or planner calls by the view (the §2.1 rule: `csp-ui` cannot name
/// sim types, and the scan stays off the command path).
#[derive(Clone, Debug)]
pub enum PlanningCommand {
    /// Run `plan_transfer` origin → destination and stage the result.
    FindWindow {
        /// The departure body.
        origin: String,
        /// The arrival body.
        destination: String,
    },
    /// `SetAlarm` at `tick + in_s` with `label`.
    ArmAlarm {
        /// Free-form label (the panel's text field).
        label: String,
        /// Seconds from now.
        in_s: f64,
    },
    /// `SetAlarm` at the focused vessel's next apsis (`true` = Ap).
    ArmApsis {
        /// Apoapsis vs periapsis.
        apoapsis: bool,
    },
    /// `SetAlarm` at the focused vessel's scheduled node tick.
    ArmNodeAlarm,
    /// `WatchTransferWindow` at the staged plan's departure tick.
    WatchWindow,
    /// `SetAlarm` at the staged plan's departure tick.
    ArmWindowAlarm,
    /// Resolve the staged plan into a `SetManeuver` for `vessel`
    /// (`departure_node` runs in the view).
    CreateNode {
        /// The vessel the burn belongs to.
        vessel: u64,
    },
    /// Remove a fired/lapsed row from the alarm book (alarms cannot
    /// be cancelled once scheduled — armed rows show no dismiss).
    DismissAlarm {
        /// The row's book index.
        index: usize,
    },
}

/// The panel's session state — owned by `csp-view` (§2.2 C), mutated
/// by [`planning_panel`].
#[derive(Clone, Debug)]
pub struct PlanningPanelState {
    /// The departure picker (body id).
    pub origin: String,
    /// The arrival picker (body id).
    pub destination: String,
    /// The alarm label text field.
    pub alarm_label: String,
    /// "In N seconds" field for a manual alarm.
    pub alarm_in_s: f64,
    /// The picked departure vessel.
    pub vessel: u64,
}

impl Default for PlanningPanelState {
    fn default() -> Self {
        Self {
            origin: "terrin".to_string(),
            destination: "duva".to_string(),
            alarm_label: "alarm".to_string(),
            alarm_in_s: 3600.0,
            vessel: 0,
        }
    }
}

/// The W2.4 planning panel — alarms + the transfer-window planner,
/// drawn in map mode beside the orbit lines it reasons about.
///
/// ALARMS: the armed list with countdowns, fired rows awaiting
/// dismissal, the manual label/T+ form, and the focused vessel's
/// quick alarms (at Pe / at Ap / at node). TRANSFER: origin and
/// destination pickers, the departure-vessel picker (ineligibility
/// staged honestly — never a dead click), "Find window" running the
/// planner, the staged plan's Δv/encounter readout, and the verbs the
/// plan implies: create the departure node, watch the window, or arm
/// an alarm at it.
///
/// Emits [`PlanningCommand`]s — the view resolves every one (body
/// names → ids, seconds → ticks, the staged plan → `SetManeuver`),
/// so a stale panel can never smuggle a command the mirrors stopped
/// justifying.
#[allow(clippy::too_many_lines)] // one window = one pass
pub fn planning_panel(
    ctx: &egui::Context,
    input: &PlanningPanelInput<'_>,
    state: &mut PlanningPanelState,
    out: &mut Vec<PlanningCommand>,
) {
    if !input.bodies.contains(&state.origin) {
        state.origin = input.bodies.first().cloned().unwrap_or_default();
    }
    if !input.bodies.contains(&state.destination) {
        state.destination = input
            .bodies
            .iter()
            .find(|b| **b != state.origin)
            .cloned()
            .unwrap_or_default();
    }
    if !input
        .vessels
        .iter()
        .any(|v| v.id == state.vessel && v.eligible)
    {
        state.vessel = input
            .vessels
            .iter()
            .find(|v| v.eligible)
            .map_or(0, |v| v.id);
    }
    egui::Window::new("Planning")
        .default_pos(egui::pos2(12.0, 60.0))
        .show(ctx, |ui| {
            // ---- ALARMS ---------------------------------------------------
            ui.label(
                egui::RichText::new("ALARMS")
                    .font(egui::FontId::monospace(12.0))
                    .color(egui::Color32::from_gray(160)),
            );
            if input.alarms.is_empty() {
                ui.label(
                    egui::RichText::new("none armed — set one below")
                        .color(egui::Color32::from_gray(120)),
                );
            }
            for row in input.alarms {
                ui.horizontal(|ui| {
                    let (text, color) = match row.state {
                        AlarmState::Armed => (
                            format!("{} — T−{}", row.label, fmt_clock(row.eta_s.max(0.0))),
                            egui::Color32::from_rgb(255, 196, 64),
                        ),
                        AlarmState::Fired => (
                            format!("{} — FIRED", row.label),
                            egui::Color32::from_rgb(120, 230, 120),
                        ),
                        AlarmState::Lapsed => (
                            format!("{} — lapsed (never fired)", row.label),
                            egui::Color32::from_gray(140),
                        ),
                    };
                    ui.colored_label(color, text);
                    if row.state != AlarmState::Armed
                        && ui.small_button("×").on_hover_text("dismiss").clicked()
                    {
                        out.push(PlanningCommand::DismissAlarm { index: row.index });
                    }
                });
            }
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut state.alarm_label).desired_width(110.0));
                ui.label("in");
                ui.add(
                    egui::DragValue::new(&mut state.alarm_in_s)
                        .suffix(" s")
                        .speed(1.0)
                        .range(0.0..=f64::MAX),
                );
                if ui.button("Set").clicked() && !state.alarm_label.is_empty() {
                    out.push(PlanningCommand::ArmAlarm {
                        label: state.alarm_label.clone(),
                        in_s: state.alarm_in_s,
                    });
                }
            });
            if input.focus_vessel.is_some() {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("at:").color(egui::Color32::from_gray(140)));
                    if ui.small_button("Pe").clicked() {
                        out.push(PlanningCommand::ArmApsis { apoapsis: false });
                    }
                    if ui.small_button("Ap").clicked() {
                        out.push(PlanningCommand::ArmApsis { apoapsis: true });
                    }
                    if ui
                        .add_enabled(input.focus_has_node, egui::Button::new("node").small())
                        .clicked()
                    {
                        out.push(PlanningCommand::ArmNodeAlarm);
                    }
                });
            }
            ui.separator();
            // ---- TRANSFER -------------------------------------------------
            ui.label(
                egui::RichText::new("TRANSFER")
                    .font(egui::FontId::monospace(12.0))
                    .color(egui::Color32::from_gray(160)),
            );
            egui::Grid::new("planning_transfer_grid")
                .num_columns(2)
                .show(ui, |ui| {
                    ui.label("origin");
                    egui::ComboBox::from_id_salt("plan_origin")
                        .selected_text(state.origin.clone())
                        .show_ui(ui, |ui| {
                            for b in input.bodies {
                                ui.selectable_value(&mut state.origin, b.clone(), b);
                            }
                        });
                    ui.end_row();
                    ui.label("destination");
                    egui::ComboBox::from_id_salt("plan_dest")
                        .selected_text(state.destination.clone())
                        .show_ui(ui, |ui| {
                            for b in input.bodies {
                                ui.selectable_value(&mut state.destination, b.clone(), b);
                            }
                        });
                    ui.end_row();
                    ui.label("vessel");
                    egui::ComboBox::from_id_salt("plan_vessel")
                        .selected_text(if input.vessels.iter().any(|v| v.id == state.vessel) {
                            format!("vessel {}", state.vessel)
                        } else {
                            "— none —".to_string()
                        })
                        .show_ui(ui, |ui| {
                            for v in input.vessels {
                                let text = if v.eligible {
                                    format!("vessel {}", v.id)
                                } else {
                                    format!("vessel {} — {}", v.id, v.note)
                                };
                                ui.add_enabled_ui(v.eligible, |ui| {
                                    ui.selectable_value(&mut state.vessel, v.id, text);
                                });
                            }
                        });
                    ui.end_row();
                });
            if input.vessels.iter().all(|v| !v.eligible) && !input.vessels.is_empty() {
                ui.label(
                    egui::RichText::new(format!(
                        "no vessel can take the burn — {}",
                        input
                            .vessels
                            .first()
                            .map(|v| v.note.as_str())
                            .unwrap_or("none on rails")
                    ))
                    .color(egui::Color32::from_gray(150)),
                );
            }
            if ui.button("Find window").clicked() {
                out.push(PlanningCommand::FindWindow {
                    origin: state.origin.clone(),
                    destination: state.destination.clone(),
                });
            }
            if let Some(err) = input.plan_error {
                ui.colored_label(egui::Color32::from_rgb(230, 120, 110), err);
            }
            if let Some(p) = input.plan {
                let mono = egui::FontId::monospace(12.0);
                let dim = egui::Color32::from_gray(190);
                ui.label(
                    egui::RichText::new(format!("{} → {}", p.origin, p.destination))
                        .font(mono.clone())
                        .color(egui::Color32::from_rgb(140, 220, 140)),
                );
                for line in [
                    format!(
                        "departs T−{} · transit {}",
                        fmt_period(p.depart_in_s),
                        fmt_period(p.duration_s)
                    ),
                    format!(
                        "Δv depart {:.0} m/s · arrive {:.0} m/s · total {:.0} m/s",
                        p.dv_depart_mps,
                        p.dv_arrive_mps,
                        p.dv_depart_mps + p.dv_arrive_mps
                    ),
                ] {
                    ui.label(egui::RichText::new(line).font(mono.clone()).color(dim));
                }
                if let Some(e) = &p.encounter {
                    let mut line = format!(
                        "{} {} T−{}",
                        if e.enter { "enters" } else { "leaves" },
                        e.body,
                        fmt_period(e.in_s)
                    );
                    // Honesty (C07-F1): the encounter is computed on
                    // the plan's own conic. It is the burn's outcome
                    // only while a verified node backs it — without
                    // one, the row describes the ideal, not anything
                    // the player can fly.
                    if p.node.is_none() {
                        line.push_str(" · plan conic — no burn staged");
                    }
                    if let Some(pe) = e.periapsis_m {
                        line.push_str(&format!(" · Pe {}", fmt_distance(pe)));
                    }
                    if let Some(v) = e.v_inf_mps {
                        line.push_str(&format!(" · v∞ {:.1} km/s", v / 1e3));
                    }
                    let warn = e.impacts == Some(true);
                    ui.label(
                        egui::RichText::new(if warn {
                            format!("{line} — IMPACTS")
                        } else {
                            line
                        })
                        .font(mono.clone())
                        .color(if warn {
                            egui::Color32::from_rgb(240, 90, 80)
                        } else {
                            dim
                        }),
                    );
                }
                match (&p.node, &p.node_error) {
                    (Some(n), _) => {
                        let outcome = match n.arrival_miss_m {
                            Some(m) => format!(" · arrives ±{}", fmt_distance(m)),
                            None => String::new(),
                        };
                        ui.label(
                            egui::RichText::new(format!(
                                "node: vessel {} T−{} · Δv {:.0} m/s{outcome}",
                                n.vessel,
                                fmt_period(n.in_s),
                                n.dv_mps
                            ))
                            .font(mono.clone())
                            .color(dim),
                        );
                        if ui.button("Set maneuver node").clicked() {
                            out.push(PlanningCommand::CreateNode { vessel: n.vessel });
                        }
                    }
                    (None, Some(err)) => {
                        ui.colored_label(
                            egui::Color32::from_rgb(230, 160, 110),
                            format!("no node — {err}"),
                        );
                    }
                    (None, None) => {}
                }
                ui.horizontal(|ui| {
                    if ui.button("Watch window").clicked() {
                        out.push(PlanningCommand::WatchWindow);
                    }
                    if ui.button("Alarm at window").clicked() {
                        out.push(PlanningCommand::ArmWindowAlarm);
                    }
                });
            }
        });
}

#[cfg(test)]
// Tests signal failure by panicking — unwrap/expect stays legal here
// (§2.13's no-panic rule covers shipping fallible paths).
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// A live-vessel HUD input — enough for the readout block + the
    /// navball Area to draw in a headless pass.
    fn hud_input<'a>(
        keys: &'a settings::Keymap,
        key_labels: &'a BTreeMap<String, String>,
        propellant: &'a [PropellantGauge],
    ) -> FlightHudInput<'a> {
        FlightHudInput {
            has_vessel: true,
            latched: false,
            alt_m: 12_480.0,
            surface_speed_mps: 812.4,
            orbital_speed_mps: 1_204.6,
            vspeed_mps: 138.2,
            heading_deg: 87.5,
            pitch_deg: 42.0,
            roll_deg: -8.0,
            throttle: 0.68,
            parts: 7,
            navball: navball::NavballInput::default(),
            warp: 4,
            vessel_count: 1,
            debris_count: 0,
            apo_alt_m: Some(84_210.0),
            pe_alt_m: Some(-6_400.0),
            t_apo_s: Some(214.0),
            t_pe_s: None,
            period_s: None,
            propellant,
            stages_left: 2,
            stages_total: 3,
            stages_plan_vessel: None,
            mass_kg: 12_840.0,
            thrust_n: 148_300.0,
            thrust_full_n: 218_000.0,
            twr: Some(1.18),
            twr_surface: Some(1.72),
            accel_mps2: Some(11.55),
            accel_gee: Some(1.18),
            isp_s: Some(310.0),
            mass_flow_kg_s: Some(48.6),
            fuel_depletion_s: Some(94.0),
            burn_full_s: Some(64.0),
            delta_v_mps: Some(1_940.0),
            delta_v_stages_mps: 420.0,
            keys,
            key_labels,
        }
    }

    /// A headless egui pass-runner (the C23 verifier's method): a
    /// `Context` + fixed-size `RawInput` the tests feed pointer-event
    /// scripts into, one `run_pass` per frame.
    fn hud_rig(w: f32, h: f32) -> (egui::Context, egui::RawInput) {
        (
            egui::Context::default(),
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::pos2(0.0, 0.0),
                    egui::vec2(w, h),
                )),
                ..Default::default()
            },
        )
    }

    fn press(pos: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        }
    }

    /// C19-F6: a borrowed plan's owner is named in the STG row — the
    /// readout can't pass another vessel's firings off as this
    /// craft's, and an unowned readout stays clean.
    #[test]
    fn stg_label_names_a_borrowed_plan_owner() {
        let keys = settings::Keymap::default();
        let key_labels = BTreeMap::new();
        let prop: [PropellantGauge; 0] = [];
        let mut input = hud_input(&keys, &key_labels, &prop);
        assert_eq!(stages_label(&input), "2 firing(s) left");
        input.stages_plan_vessel = Some(9);
        assert_eq!(stages_label(&input), "2 firing(s) left — vessel 9's plan");
        // A borrowed plan with nothing left still names its owner —
        // "0 firing(s) left" is about THAT vessel's plan, not "this
        // craft is single-stage".
        input.stages_left = 0;
        assert_eq!(stages_label(&input), "0 firing(s) left — vessel 9's plan");
    }

    #[test]
    fn fmt_clock_floors_seconds() {
        // C23-F4: the seconds field used to round the remainder —
        // 119.7 s printed "1:60" and 3599.9 s printed "59:60".
        assert_eq!(fmt_clock(119.7), "1:59");
        assert_eq!(fmt_clock(3599.9), "59:59");
        assert_eq!(fmt_clock(0.0), "0:00");
        assert_eq!(fmt_clock(45.0), "0:45");
        assert_eq!(fmt_clock(60.0), "1:00");
        // Past ten hours the coarse period formatter takes over.
        assert_eq!(fmt_clock(36_000.0), fmt_period(36_000.0));
    }

    /// One headless frame. `FullOutput::textures_delta` is
    /// drop-guarded (`#[must_use]`-style: unapplied deltas panic on
    /// drop) — the tests assert on memory state, not pixels, so the
    /// font-atlas upload is simply acknowledged and dropped.
    fn run_frame(ctx: &egui::Context, raw: egui::RawInput, input: &FlightHudInput<'_>) {
        let mut out = ctx.run_pass(raw, |ctx| flight_hud(ctx, input));
        out.textures_delta.clear();
    }

    /// C23-F2: the navball's first-open parking spot. Without
    /// `.default_size(nav_size)` egui's sizing pass clamps
    /// `default_pos` against the nominal 600×400 `default_area_size`
    /// and the ball parks ~242 px high (`[360,200]` on 960×600) for
    /// every subsequent frame. With the real size it must land at
    /// `(W/2 − 68, H − 152 − 6)` = `(412, 442)` and stay there.
    #[test]
    fn navball_first_open_parks_bottom_center() {
        let keys = settings::Keymap::default();
        let key_labels = BTreeMap::new();
        let prop: [PropellantGauge; 0] = [];
        let input = hud_input(&keys, &key_labels, &prop);
        let (ctx, raw) = hud_rig(960.0, 600.0);
        let navball_id = egui::Id::unique("flight_navball");
        let mut min = None;
        for _ in 0..3 {
            run_frame(&ctx, raw.clone(), &input);
            min = ctx.memory(|m| m.area_rect(navball_id)).map(|r| r.min);
        }
        let min = min.expect("navball area registered");
        let expected = egui::pos2(480.0 - 68.0, 600.0 - 152.0 - 6.0);
        assert!(
            (min - expected).length() < 4.0,
            "navball first-open parked at {min:?}, expected ≈{expected:?}"
        );
    }

    /// C23-F1: the readout block must drag when grabbed ON a text row —
    /// egui 0.37's default-selectable labels used to claim the press
    /// over the Area's whole-rect move widget, so the panel never
    /// moved. The assertion checks both that the *move* widget is the
    /// dragged id mid-gesture and that the panel actually moved.
    #[test]
    fn readout_block_drags_from_its_text() {
        let keys = settings::Keymap::default();
        let key_labels = BTreeMap::new();
        let prop: [PropellantGauge; 0] = [];
        let input = hud_input(&keys, &key_labels, &prop);
        let (ctx, mut raw) = hud_rig(960.0, 600.0);
        let hud_id = egui::Id::unique("flight_hud");
        // egui hit-tests each pass against the PREVIOUS pass's widget
        // rects, and the Area registers its move widget before the
        // off-screen correction is applied — so the first real pass
        // records a stale rect. Three warm-up frames leave the parked
        // position stable before the press.
        for _ in 0..3 {
            run_frame(&ctx, raw.clone(), &input);
        }
        let before = ctx
            .memory(|m| m.area_rect(hud_id))
            .expect("hud area registered");
        // A point inside the first label row (the "── ORBIT ──"
        // section line): `hud_frame`'s inner margin puts the first
        // mono row ~7 px below the chip's top.
        let grab = egui::pos2(before.left() + 80.0, before.top() + 15.0);
        let move_id = hud_id.with("move");
        let mut saw_move_drag = false;
        for events in [
            vec![press(grab, true)],
            vec![egui::Event::PointerMoved(grab + egui::vec2(16.0, -8.0))],
            vec![egui::Event::PointerMoved(grab + egui::vec2(140.0, -60.0))],
        ] {
            raw.events = events;
            run_frame(&ctx, raw.clone(), &input);
            saw_move_drag |= ctx.interaction_snapshot(|s| s.dragged) == Some(move_id);
        }
        assert!(
            saw_move_drag,
            "drag over the readout's text must reach the Area's move widget"
        );
        raw.events = vec![press(grab + egui::vec2(140.0, -60.0), false)];
        run_frame(&ctx, raw, &input);
        let after = ctx
            .memory(|m| m.area_rect(hud_id))
            .expect("hud area registered");
        let delta = after.min - before.min;
        assert!(
            delta.length() > 60.0,
            "readout block should have followed the drag (moved {delta:?})"
        );
    }
}
