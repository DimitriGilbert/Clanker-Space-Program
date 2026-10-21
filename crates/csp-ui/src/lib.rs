#![forbid(unsafe_code)]

//! egui panels, VAB session model, input bindings, settings (PLAN §2.1).
//!
//! Pure egui functions only: this crate never sees `bevy_ecs`, `csp-sim`,
//! `wgpu`, or `winit` (§2.1 dependency rules). Panels take plain-data input
//! structs populated by `csp-view`'s `ui_build` system from presentation
//! mirrors — the sim is never read directly — and return plain-data output
//! structs the same system stores for next frame's `Input` (§2.5). Painting
//! the resulting `egui::FullOutput` stays in `csp-app`/`egui-wgpu`.

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
