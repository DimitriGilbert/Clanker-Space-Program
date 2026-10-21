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
//! schedule's [`csp_ui`] panel system, and render consumes the stored
//! `FullOutput`. Native and wasm share this code; the entry points differ
//! (`main.rs` vs `wasm_start` below).

mod warp;

use std::sync::Arc;

use csp_core::SimError;
use csp_render::Gpu;
use csp_view::{SimCommand, SourceClass, View};
use warp::WarpController;
use web_time::Instant;
use winit::{
    application::ApplicationHandler,
    event::{ElementState, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoop},
    keyboard::Key,
    window::{Window, WindowId},
};

/// Max wall seconds a frame may contribute to sim time — clamps the
/// spiral-of-death case when the app stalls (§2.5 `min(dt, 100ms)`).
const MAX_FRAME_DT: f64 = 0.1;

/// `source_id` for the warp controller's commands (§2.2.1). The controller
/// is the only `Ui`-class producer at M0; `SetWarp` is enqueued before
/// `AdvanceTo` each frame so per-source seq order applies the level first.
const WARP_SOURCE_ID: u32 = 0;

struct App {
    window: Option<Arc<Window>>,
    gpu: Option<Gpu>,
    egui_state: Option<egui_winit::State>,
    egui_renderer: Option<egui_wgpu::Renderer>,
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
    /// Set when the sim reports a §2.13 `Fatal`: ticking halts and the error
    /// is surfaced in the debug panel. (The full policy — dialog, read-only
    /// session, bug-report bundle — is a later-milestone concern; M0 stops
    /// advancing and shows the error rather than panicking or dropping it.)
    fatal: Option<SimError>,
}

impl App {
    fn new() -> Self {
        Self {
            window: None,
            gpu: None,
            egui_state: None,
            egui_renderer: None,
            view: View::new(),
            warp: WarpController::new(),
            last_frame: Instant::now(),
            last_tick_ms: 0.0,
            fatal: None,
        }
    }

    // §2.13's no-panic rule covers the determinism domain; window/GPU init
    // is unrecoverable presentation setup — there is nothing to fall back
    // to, so `expect` is the honest failure mode (allowed at fn scope).
    #[allow(clippy::expect_used)]
    fn init_graphics(&mut self, event_loop: &ActiveEventLoop) {
        let attrs = Window::default_attributes()
            .with_title("Clanker Space Program")
            .with_inner_size(winit::dpi::LogicalSize::new(1280u32, 720u32));

        #[cfg(target_arch = "wasm32")]
        let attrs = {
            use winit::platform::web::WindowAttributesExtWebSys;
            attrs.with_append(true)
        };

        let window = Arc::new(
            event_loop
                .create_window(attrs)
                .expect("failed to create window"),
        );

        let size = window.inner_size();
        let gpu = pollster::block_on(Gpu::new(window.clone(), (size.width, size.height)))
            .expect("failed to init GPU");

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
        let info = gpu.adapter.get_info();
        self.view
            .set_gpu_info(info.name.clone(), format!("{:?}", info.backend));

        self.window = Some(window);
        self.gpu = Some(gpu);
        self.egui_state = Some(egui_state);
        self.egui_renderer = Some(egui_renderer);
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
    fn run_frame(&mut self, window: &Window, dt_wall: f64) {
        // UiBuild → next frame's Input (§2.5's accepted one-frame latency):
        // fold last frame's panel requests into the controller.
        let outbox = self.view.take_ui_outbox();
        if outbox.warp_step != 0 {
            self.warp.request_step(outbox.warp_step);
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

        // §2.5 order: Input → SimStep → Sync → Presentation → UiBuild →
        // RenderExtract. One sim_step call per frame, always.
        self.view.begin_frame();
        if let Some(target) = target {
            let step_start = Instant::now();
            if let Err(e) = self.view.sim_step(target) {
                self.fatal = Some(e);
                // Refresh the halt mirror so UiBuild sees it this frame.
                self.view.set_fatal(self.fatal);
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
    }

    /// The paint half of the frame: consume the `UiBuild`-produced
    /// `egui::FullOutput` (platform output, tessellation, textures delta)
    /// and draw it — no egui pass logic here (§2.5).
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
        let (gpu, egui_renderer) = match (&mut self.gpu, &mut self.egui_renderer) {
            (Some(g), Some(r)) => (g, r),
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

        let mut frame = match gpu.begin_frame() {
            csp_render::FrameResult::Ok(f) => f,
            csp_render::FrameResult::Reconfigure => {
                // Surface lost (e.g. resize race) — reconfigure and skip.
                let (w, h) = (gpu.config.width, gpu.config.height);
                gpu.resize(w, h);
                return;
            }
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
            let pass = frame
                .encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("main"),
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
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
            egui_renderer.render(&mut pass.forget_lifetime(), &tris, &screen);
            drop(cmds); // user command buffers currently unused
        }

        gpu.submit(*frame);
        window.request_redraw();
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

        if let Some(state) = &mut self.egui_state {
            let response = state.on_window_event(&window, &event);
            if response.consumed {
                return;
            }
        }

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(gpu) = &mut self.gpu {
                    gpu.resize(size.width, size.height);
                }
            }
            // Manual warp ladder steps (§2.6.3): `,` down / `.` up — the
            // request lands in the controller and becomes a logged
            // `SetWarp` on the next decide().
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed && !event.repeat =>
            {
                match &event.logical_key {
                    Key::Character(c) if c.as_str() == "," => self.warp.request_step(-1),
                    Key::Character(c) if c.as_str() == "." => self.warp.request_step(1),
                    _ => {}
                }
            }
            WindowEvent::RedrawRequested => self.render(),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }
}

/// Native entry point.
// Event-loop creation/run failure is unrecoverable startup — outside the
// determinism domain, `expect` is the honest failure mode (§2.13).
#[cfg(not(target_arch = "wasm32"))]
#[allow(clippy::expect_used)]
pub fn run() {
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
