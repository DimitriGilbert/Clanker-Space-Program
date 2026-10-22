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
use std::collections::BTreeMap;

use csp_render::Gpu;
use csp_render::flight::FlightRenderer;
use csp_render::map::MapRenderer;
use csp_render::vab::VabRenderer;
use csp_view::flight::FlightEvent;
use csp_view::map::MapKeyEvent;
use csp_view::vab::VabEvent;
use csp_view::{EventClass, ModeSelect, SimCommand, SourceClass, View, ViewMode};
use warp::WarpController;
use web_time::Instant;
use winit::{
    application::ApplicationHandler,
    event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoop},
    keyboard::{Key, NamedKey},
    window::{Window, WindowId},
};

/// Max wall seconds a frame may contribute to sim time — clamps the
/// spiral-of-death case when the app stalls (§2.5 `min(dt, 100ms)`).
const MAX_FRAME_DT: f64 = 0.1;

/// `source_id` for the warp controller's commands (§2.2.1). The controller
/// is the only `Ui`-class producer at M0; `SetWarp` is enqueued before
/// `AdvanceTo` each frame so per-source seq order applies the level first.
const WARP_SOURCE_ID: u32 = 0;

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
    /// Craft-file store: name → `CSPCRAFT` bytes. Memory-first so the
    /// wasm build keeps a working save/load; on native the store is
    /// mirrored to `crafts/*.cspcraft` files.
    craft_store: BTreeMap<String, Vec<u8>>,
    /// Left-drag state for map camera orbit — pointer position at the
    /// last `CursorMoved`, and whether the left button is held.
    last_cursor: Option<(f64, f64)>,
    map_dragging: bool,
    /// Cursor position at the last unconsumed LMB press — a candidate
    /// VAB click. A release within `CLICK_MAX_DRAG_PX` of it becomes a
    /// pick request; a release past it was an orbit drag and does not
    /// select.
    lmb_press: Option<(f64, f64)>,
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
    // §8.3/§2.13: `View::new` loads `data/` (balance.ron + the home
    // system) — a missing or invalid file is a startup failure, surfaced
    // loudly at construction. Presentation init is the app's one allowed
    // `expect` site per the workspace lint doc.
    #[allow(clippy::expect_used)]
    fn new() -> Self {
        let (pick_tx, pick_rx) = std::sync::mpsc::channel();
        Self {
            window: None,
            gpu: None,
            egui_state: None,
            egui_renderer: None,
            flight_renderer: None,
            map_renderer: None,
            vab_renderer: None,
            last_vab_packet: None,
            pending_pick: None,
            pick_tx,
            pick_rx,
            pick_in_flight: false,
            pick_decode_ids: None,
            craft_store: load_craft_store(),
            last_cursor: None,
            map_dragging: false,
            lmb_press: None,
            view: View::new().expect("data/ startup load failed"),
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
        let info = gpu.adapter.get_info();
        self.view
            .set_gpu_info(info.name.clone(), format!("{:?}", info.backend));

        self.window = Some(window);
        self.gpu = Some(gpu);
        self.egui_state = Some(egui_state);
        self.egui_renderer = Some(egui_renderer);
        self.flight_renderer = Some(flight_renderer);
        self.map_renderer = Some(map_renderer);
        self.vab_renderer = Some(vab_renderer);
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
        if outbox.warp_step != 0 {
            self.warp.request_step(outbox.warp_step);
        }
        if let Some(mode) = outbox.mode {
            self.view.set_mode(match mode {
                ModeSelect::Flight => ViewMode::Flight,
                ModeSelect::Map => ViewMode::Map,
                ModeSelect::Vab => ViewMode::Vab,
            });
        }
        if let Some((name, bytes)) = outbox.craft_save {
            self.craft_store.insert(name.clone(), bytes.clone());
            save_craft_file(&name, &bytes);
            self.view
                .set_craft_files(self.craft_store.keys().cloned().collect());
            self.view
                .vab_event(VabEvent::Notice(format!("saved {name}")));
        }
        if outbox.launch {
            self.view.launch_craft();
        }
        if let Some(name) = outbox.craft_load {
            match self.craft_store.get(&name) {
                Some(b) => self.view.vab_event(VabEvent::LoadCraft(b.clone())),
                None => self
                    .view
                    .vab_event(VabEvent::Notice(format!("no craft file '{name}'"))),
            }
        }
        // GPU pick readback: a completed map_async delivers the object
        // id — decode through the id table snapshotted at encode time
        // (`last_vab_packet` may have been replaced between rasterize
        // and this landing) and route the selection through the Input
        // schedule.
        while let Ok(pid) = self.pick_rx.try_recv() {
            self.pick_in_flight = false;
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
        // The map camera's projection needs the live framebuffer size.
        if let Some(gpu) = &self.gpu {
            self.view
                .set_map_viewport(f64::from(gpu.config.width), f64::from(gpu.config.height));
            self.view
                .set_vab_viewport(f64::from(gpu.config.width), f64::from(gpu.config.height));
        }

        // §2.5 order: Input → SimStep → Sync → Presentation → UiBuild →
        // RenderExtract. One sim_step call per frame, always.
        self.view.begin_frame();
        if let Some(target) = target {
            let step_start = Instant::now();
            match self.view.sim_step(target) {
                Ok(report) => {
                    // §2.6.5: a fired Clamping event demotes warp — now,
                    // before the next frame's step — so the player cannot
                    // overshoot (PlayerAlarm, tracked-vessel SoiTransition).
                    // The demotion itself is a logged SetWarp; re-promotion
                    // still runs the §2.6.3 hysteresis.
                    let clamping = report
                        .fired_events
                        .iter()
                        .any(|e| e.class == EventClass::Clamping);
                    // §2.2.1/§2.13's honest channels: rejected commands and
                    // degraded events surface as log lines — they are the
                    // app's view of Recoverable, never a halt.
                    for (key, e) in &report.rejected {
                        log::warn!("command rejected on originating channel {key:?}: {e}");
                    }
                    for e in &report.degraded {
                        log::warn!("event degraded (fallback applied): {e}");
                    }
                    // §2.6.2's rails-warp refusal seam: the sim-side
                    // predicate, checked here every frame on post-step
                    // state — the only place it can be. Honest about the
                    // one-frame lag: the predicate reads state THIS
                    // frame's step produced, and the demote is a queued
                    // SetWarp that lands at the next frame's boundary —
                    // the frame the refusal appeared in already ran at
                    // the old level. Bounded, not compounding: the
                    // refusal conditions (thrust, contact, atmosphere)
                    // persist until commanded away, so the predicate
                    // re-reports every frame until the state changes.
                    let refusal = self.view.sim.rails_warp_refusal();
                    if (clamping || refusal.is_some())
                        && let Some(level) = self.warp.demote(0)
                    {
                        // §2.6.2: the refusal names the offending vessel.
                        if let Some(reason) = &refusal {
                            log::warn!("rails warp refused ({reason}) — demoting to ×{level}");
                        }
                        self.view.enqueue_command(
                            SourceClass::Ui,
                            WARP_SOURCE_ID,
                            SimCommand::SetWarp(level),
                        );
                    }
                }
                Err(e @ SimError::Fatal(_)) => {
                    // §2.13: only Fatal halts — step_frame keeps
                    // Recoverable inside its report channels.
                    self.fatal = Some(e);
                    // Refresh the halt mirror so UiBuild sees it this frame.
                    self.view.set_fatal(self.fatal);
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
            let mut pass = frame
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
                    // The VAB renderer owns the depth target (reverse-Z:
                    // clear 0.0 = far, `Greater` wins). Map/egui
                    // pipelines carry no depth state and ignore it.
                    depth_stencil_attachment: (if self.view.mode() == ViewMode::Flight {
                        self.flight_renderer.as_ref().map(|v| v.depth_view())
                    } else {
                        self.vab_renderer.as_ref().map(|v| v.depth_view())
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
                f.draw(&mut pass, &gpu.device, &gpu.queue, packet);
            }
            egui_renderer.render(&mut pass.forget_lifetime(), &tris, &screen);
            drop(cmds); // user command buffers currently unused
        }

        // A queued pick: rasterize the same packet into the object-id
        // target and copy the clicked pixel to the readback buffer —
        // same encoder submission as the frame.
        let mut encode_pick = false;
        let mut encoded_ids = None;
        if let (Some((x, y)), Some(v), Some(packet)) = (
            self.pending_pick.take(),
            &mut self.vab_renderer,
            &self.last_vab_packet,
        ) {
            if self.pick_in_flight {
                // One outstanding pick — drop the request; the next
                // click retriggers.
            } else {
                encode_pick =
                    v.encode_pick(&mut frame.encoder, &gpu.device, &gpu.queue, packet, x, y);
                if encode_pick {
                    // Snapshot the decode table alongside the rasterized
                    // pixels — the readback decodes a frame or two later
                    // and `last_vab_packet` may have moved on by then.
                    encoded_ids = Some(packet.pick_ids.clone());
                }
            }
        }
        if let Some(ids) = encoded_ids {
            self.pick_decode_ids = Some(ids);
        }
        gpu.submit(*frame);
        if encode_pick {
            if let Some(v) = &self.vab_renderer {
                v.map_pick(self.pick_tx.clone());
            }
            self.pick_in_flight = true;
        }
        // Service map_async callbacks (native; wasm callbacks fire on
        // the event loop).
        if let Err(e) = gpu.device.poll(wgpu::PollType::Poll) {
            log::warn!("device poll failed: {e}");
        }
        window.request_redraw();
    }

    /// One key-press event: manual warp ladder steps (§2.6.3 — `,` down /
    /// `.` up land in the controller and become a logged `SetWarp` on the
    /// next decide()) and the map camera keys. Map input rides
    /// `View::map_event` into the Input schedule; pan keys and Tab accept
    /// key-repeat so they track a held key, while `M`/`,`/`.` are
    /// non-repeat single shots.
    #[allow(clippy::too_many_lines)] // M4c's flight arms lengthened it
    fn key_press(&mut self, event: &winit::event::KeyEvent) {
        let mode = self.view.mode();
        let in_map = mode == ViewMode::Map;
        let in_vab = mode == ViewMode::Vab;
        let pan = |dx_px: f64, dy_px: f64| MapKeyEvent::Pan { dx_px, dy_px };
        // VAB mode keys: V toggles the editor; M/WASD/arrows get their
        // own routing below. `,`/`.` stay warp keys in every mode.
        if let Key::Character(c) = &event.logical_key
            && c.eq_ignore_ascii_case("v")
            && !event.repeat
        {
            self.view.set_mode(if in_vab {
                ViewMode::Flight
            } else {
                ViewMode::Vab
            });
            return;
        }
        if in_vab {
            let vab_event = match &event.logical_key {
                Key::Character(c) if c.eq_ignore_ascii_case("m") && !event.repeat => {
                    self.view.set_mode(ViewMode::Map);
                    return;
                }
                Key::Named(NamedKey::ArrowUp) => Some(VabEvent::Pan {
                    dx_px: 0.0,
                    dy_px: -PAN_STEP_PX,
                }),
                Key::Named(NamedKey::ArrowDown) => Some(VabEvent::Pan {
                    dx_px: 0.0,
                    dy_px: PAN_STEP_PX,
                }),
                Key::Named(NamedKey::ArrowLeft) => Some(VabEvent::Pan {
                    dx_px: -PAN_STEP_PX,
                    dy_px: 0.0,
                }),
                Key::Named(NamedKey::ArrowRight) => Some(VabEvent::Pan {
                    dx_px: PAN_STEP_PX,
                    dy_px: 0.0,
                }),
                Key::Character(c) => match c.as_str() {
                    "w" | "W" => Some(VabEvent::Pan {
                        dx_px: 0.0,
                        dy_px: -PAN_STEP_PX,
                    }),
                    "s" | "S" => Some(VabEvent::Pan {
                        dx_px: 0.0,
                        dy_px: PAN_STEP_PX,
                    }),
                    "a" | "A" => Some(VabEvent::Pan {
                        dx_px: -PAN_STEP_PX,
                        dy_px: 0.0,
                    }),
                    "d" | "D" => Some(VabEvent::Pan {
                        dx_px: PAN_STEP_PX,
                        dy_px: 0.0,
                    }),
                    _ => None,
                },
                _ => None,
            };
            if let Some(ev) = vab_event {
                self.view.vab_event(ev);
            }
            return;
        }
        let map_event = match &event.logical_key {
            Key::Character(c) if matches!(c.as_str(), "z" | "Z" | "x" | "X") && !event.repeat => {
                // M4c flight throttle — the §7.6 command channel like
                // every other mutation (logged, deterministic).
                if let Some(vessel) = self.view.flight_focus() {
                    self.view.enqueue_command(
                        SourceClass::Ui,
                        1,
                        SimCommand::SetThrottle {
                            vessel,
                            throttle: if c.eq_ignore_ascii_case("z") {
                                1.0
                            } else {
                                0.0
                            },
                        },
                    );
                }
                None
            }
            Key::Character(c) if c.as_str() == "," && !event.repeat => {
                self.warp.request_step(-1);
                return;
            }
            Key::Character(c) if c.as_str() == "." && !event.repeat => {
                self.warp.request_step(1);
                return;
            }
            Key::Character(c) if c.eq_ignore_ascii_case("m") && !event.repeat => {
                // Mode choreography lives in View::set_mode so VAB
                // suppression is handled in one place.
                self.view.set_mode(if in_map {
                    ViewMode::Flight
                } else {
                    ViewMode::Map
                });
                return;
            }
            Key::Named(NamedKey::Tab) if in_map => Some(MapKeyEvent::CycleFocus),
            Key::Named(NamedKey::ArrowUp) if in_map => Some(pan(0.0, -PAN_STEP_PX)),
            Key::Named(NamedKey::ArrowDown) if in_map => Some(pan(0.0, PAN_STEP_PX)),
            Key::Named(NamedKey::ArrowLeft) if in_map => Some(pan(-PAN_STEP_PX, 0.0)),
            Key::Named(NamedKey::ArrowRight) if in_map => Some(pan(PAN_STEP_PX, 0.0)),
            Key::Character(c) if in_map => match c.as_str() {
                "w" | "W" => Some(pan(0.0, -PAN_STEP_PX)),
                "s" | "S" => Some(pan(0.0, PAN_STEP_PX)),
                "a" | "A" => Some(pan(-PAN_STEP_PX, 0.0)),
                "d" | "D" => Some(pan(PAN_STEP_PX, 0.0)),
                _ => None,
            },
            _ => None,
        };
        if let Some(ev) = map_event {
            self.view.map_event(ev);
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
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(gpu) = &mut self.gpu {
                    gpu.resize(size.width, size.height);
                }
                if let (Some(g), Some(f)) = (&self.gpu, &mut self.flight_renderer) {
                    f.resize(&g.device, size.width, size.height);
                    self.view
                        .set_flight_viewport(size.width as f64, size.height as f64);
                }
                if let (Some(g), Some(v)) = (&self.gpu, &mut self.vab_renderer) {
                    v.resize(&g.device, size.width, size.height);
                }
            }
            // Key presses — warp ladder + map camera; see `key_press`.
            // `!consumed` keeps egui-owned keys (a focused text field)
            // out of the sim.
            WindowEvent::KeyboardInput { event, .. }
                if !consumed && event.state == ElementState::Pressed =>
            {
                self.key_press(&event);
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
                match state {
                    ElementState::Pressed => {
                        // VAB pick is decided at RELEASE, not here: an
                        // unconsumed press is only a click *candidate* —
                        // it may be the start of a camera orbit drag,
                        // and a drag must not also select.
                        self.lmb_press = if consumed { None } else { self.last_cursor };
                    }
                    ElementState::Released => {
                        // VAB: an unconsumed click that stayed inside
                        // the drag threshold requests a GPU pick at the
                        // cursor — the readback lands via the pick
                        // channel a frame or two later.
                        if let Some(press) = self.lmb_press.take() {
                            let pos = self.last_cursor.unwrap_or(press);
                            let dragged =
                                (pos.0 - press.0).hypot(pos.1 - press.1) > CLICK_MAX_DRAG_PX;
                            if !consumed && !dragged && self.view.mode() == ViewMode::Vab {
                                self.pending_pick =
                                    Some((pos.0.max(0.0) as u32, pos.1.max(0.0) as u32));
                            }
                        }
                    }
                }
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
                let factor = f64::exp(lines);
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

/// Native: `crafts/` next to the working directory — session artifacts
/// live outside `data/` (that tree is authored content + the
/// attribution lint). On wasm the store is memory-only.
#[cfg(not(target_arch = "wasm32"))]
fn load_craft_store() -> BTreeMap<String, Vec<u8>> {
    let mut store = BTreeMap::new();
    let dir = std::path::Path::new("crafts");
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "cspcraft")
                && let (Ok(bytes), Some(name)) = (
                    std::fs::read(&p),
                    p.file_stem().and_then(|s| s.to_str()).map(String::from),
                )
            {
                store.insert(name, bytes);
            }
        }
    }
    store
}

/// wasm: in-memory only — the M3 web slice has no filesystem.
#[cfg(target_arch = "wasm32")]
fn load_craft_store() -> BTreeMap<String, Vec<u8>> {
    BTreeMap::new()
}

/// Native: mirror the store entry to `crafts/<name>.cspcraft`. Failures
/// are logged, not fatal — the in-memory store already holds the craft.
#[cfg(not(target_arch = "wasm32"))]
fn save_craft_file(name: &str, bytes: &[u8]) {
    let dir = std::path::Path::new("crafts");
    if let Err(e) = std::fs::create_dir_all(dir)
        .and_then(|()| std::fs::write(dir.join(format!("{name}.cspcraft")), bytes))
    {
        log::warn!("craft save to disk failed: {e}");
    }
}

/// wasm: nothing to write — the store in `App` is the whole store.
#[cfg(target_arch = "wasm32")]
fn save_craft_file(_name: &str, _bytes: &[u8]) {}
