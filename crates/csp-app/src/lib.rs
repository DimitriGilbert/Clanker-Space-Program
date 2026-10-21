//! Application shell: winit window + wgpu surface + egui UI driving the
//! `csp-view` presentation world (PLAN §2.5 frame loop).
//!
//! Wall-clock time is read *only here*, converted to integer sim ticks, and
//! never reaches the sim. Native and wasm share this code; the entry points
//! differ (`main.rs` vs `wasm_start` below).

use std::sync::Arc;

use csp_core::TICK_HZ;
use csp_render::Gpu;
use csp_view::{FrameStats, View};
use web_time::Instant;
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, EventLoop},
    window::{Window, WindowId},
};

/// Max wall seconds a frame may contribute to sim time — clamps the
/// spiral-of-death case when the app stalls.
const MAX_FRAME_DT: f64 = 0.1;

struct App {
    window: Option<Arc<Window>>,
    gpu: Option<Gpu>,
    egui_ctx: egui::Context,
    egui_state: Option<egui_winit::State>,
    egui_renderer: Option<egui_wgpu::Renderer>,
    view: View,
    last_frame: Instant,
    /// Fractional tick carry-over; wall-clock side only, never sim state.
    tick_remainder: f64,
}

impl App {
    fn new() -> Self {
        Self {
            window: None,
            gpu: None,
            egui_ctx: egui::Context::default(),
            egui_state: None,
            egui_renderer: None,
            view: View::new(),
            last_frame: Instant::now(),
            tick_remainder: 0.0,
        }
    }

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

        let egui_state = egui_winit::State::new(
            self.egui_ctx.clone(),
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

        self.window = Some(window);
        self.gpu = Some(gpu);
        self.egui_state = Some(egui_state);
        self.egui_renderer = Some(egui_renderer);
    }

    /// Convert wall dt into integer sim ticks at the immutable 64 Hz rate,
    /// carrying the fractional remainder (PLAN §2.5 frame loop).
    fn wall_to_ticks(&mut self, dt_wall: f64) -> u64 {
        let raw = dt_wall.min(MAX_FRAME_DT) * TICK_HZ as f64 + self.tick_remainder;
        let ticks = raw.floor() as u64;
        self.tick_remainder = raw - ticks as f64;
        ticks
    }

    fn render(&mut self) {
        if self.window.is_none() || self.gpu.is_none() {
            return;
        }

        // --- frame loop (PLAN §2.5): Input -> N× SimStep -> Sync ->
        //     Presentation -> UiBuild -> RenderExtract -> render ---
        let now = Instant::now();
        let dt_wall = now.duration_since(self.last_frame).as_secs_f64();
        self.last_frame = now;

        // PLAN §2.5 ordering: Input -> Nx SimStep -> Sync -> Presentation ->
        // UiBuild (egui, below) -> RenderExtract -> render.
        let ticks = self.wall_to_ticks(dt_wall);
        self.view.begin_frame();
        self.view.sim_ticks(ticks);

        let (window, gpu, egui_state, egui_renderer) = match (
            &self.window,
            &self.gpu,
            &mut self.egui_state,
            &mut self.egui_renderer,
        ) {
            (Some(w), Some(g), Some(s), Some(r)) => (w, g, s, r),
            _ => return,
        };

        // --- egui (UiBuild) ---
        let input = egui_state.take_egui_input(window);
        let full_output = self.egui_ctx.run_ui(input, |ctx| {
            egui::Window::new("CSP — M0 scaffold").show(ctx, |ui| {
                let stats = self.view.world.resource::<FrameStats>();
                let clock = self.view.sim.clock();
                ui.label(format!("adapter: {}", gpu.adapter.get_info().name));
                ui.label(format!("backend: {:?}", gpu.adapter.get_info().backend));
                ui.separator();
                ui.label(format!("sim time: {:.2} s", clock.seconds()));
                ui.label(format!("tick: {}", clock.ticks()));
                ui.label(format!("ticks last frame: {}", stats.ticks_last_frame));
                ui.label(format!("frames: {}", stats.frames));
                ui.label(format!("frame dt: {:.2} ms", dt_wall * 1000.0));
            });
        });
        egui_state.handle_platform_output(window, full_output.platform_output);

        let tris = self
            .egui_ctx
            .tessellate(full_output.shapes, full_output.pixels_per_point);

        self.view.end_frame();

        let mut frame = match gpu.begin_frame() {
            csp_render::FrameResult::Ok(f) => f,
            csp_render::FrameResult::Reconfigure => {
                // Surface lost (e.g. resize race) — reconfigure and skip.
                let (w, h) = (gpu.config.width, gpu.config.height);
                self.gpu.as_mut().unwrap().resize(w, h);
                return;
            }
            csp_render::FrameResult::Skip => return,
        };

        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [gpu.config.width, gpu.config.height],
            pixels_per_point: full_output.pixels_per_point,
        };
        for (id, deltas) in &full_output.textures_delta.set {
            for delta in deltas {
                egui_renderer.update_texture(&gpu.device, &gpu.queue, *id, delta);
            }
        }
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

        for id in &full_output.textures_delta.free {
            egui_renderer.free_texture(id);
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
#[cfg(not(target_arch = "wasm32"))]
pub fn run() {
    let event_loop = EventLoop::new().expect("failed to create event loop");
    let mut app = App::new();
    event_loop.run_app(&mut app).expect("event loop error");
}

/// wasm entry point — called automatically by wasm-bindgen.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(start)]
pub fn wasm_start() {
    console_error_panic_hook::set_once();
    let _ = console_log::init_with_level(log::Level::Info);

    let event_loop = EventLoop::new().expect("failed to create event loop");
    let app = App::new();
    winit::platform::web::EventLoopExtWebSys::spawn_app(event_loop, app);
}
