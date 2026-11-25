#![forbid(unsafe_code)]

//! wgpu device/surface ownership and frame lifecycle.
//!
//! Rendering happens in f32 relative to the active camera — the sim keeps
//! f64 world positions, this crate never sees them.

/// The M1 map-view pass (PLAN §5.1 R0): unlit orbit polylines and markers
/// drawn from a `csp-view`-built [`map::MapPacket`].
pub mod flight;
pub mod map;

/// The R2 flight-view surface pass (PLAN §5.2): CPU quadtree terrain with
/// per-patch f64 origins, precomputed-LUT atmospheric scattering, ocean.
pub mod terrain;

/// The R1 shadow-cascade core (PLAN §5.2 row R1 "shadow cascades" — the
/// row's last unshipped term): the pure per-frame cascade solve
/// ([`shadow::solve_cascades`]), the GPU cascade-map passes
/// (`ShadowCore`, terrain + craft cast), and the receiving paths'
/// uniforms. Terrain-overhaul 3a shipped the core; 3b armed it — the
/// view solves per frame and the frame loop records the passes.
pub mod shadow;

/// The M3 VAB craft view + GPU picking (PLAN M3 exit row "GPU picking",
/// §9 "Editor viewport is `csp-render` + GPU picking"): a depth-tested
/// kitbash part pass over a `csp-view`-built [`vab::VabPacket`], plus
/// an offscreen object-id pass whose single-pixel readback resolves the
/// part under the cursor.
pub mod vab;

/// Per-frame part lighting for the flight and VAB vertex passes —
/// one directional "sun" plus a two-stop hemisphere ambient (sky
/// bounce toward `hemi.xyz`, ground bounce away). All terms are in the
/// camera-relative frame the vertex streams already carry; the eye
/// sits at the origin by construction (§2.3), so the fragment shader
/// needs no eye position — save against the ocean, which is not about
/// the eye: `water` carries that one. `mat` on each vertex supplies
/// specular strength/exponent and baked occlusion.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct PartLight {
    /// Unit direction TO the light (xyz; w unused).
    pub sun_dir: [f32; 4],
    /// Sun's linear RGB intensity (w unused).
    pub sun_col: [f32; 4],
    /// Hemisphere ambient, up-side (linear RGB; w unused).
    pub amb_up: [f32; 4],
    /// Hemisphere ambient, down-side (linear RGB; w unused).
    pub amb_dn: [f32; 4],
    /// Hemisphere "up" axis (xyz; w = rim-light strength multiplier).
    pub hemi: [f32; 4],
    /// The water a fragment is seen through: `xyz` the eye relative to
    /// the body centre — the centre of the ocean, the sphere of radius
    /// `w` the surface is, 0 for none. A fragment inside that sphere is
    /// under water: `flight.wgsl`'s `water_fog` absorbs it.
    pub water: [f32; 4],
}

impl Default for PartLight {
    /// Neutral studio light — what the VAB editor uses: a warm key
    /// from the upper front, a cool sky fill, a dim ground bounce.
    fn default() -> Self {
        Self {
            sun_dir: [0.4021, 0.3016, 0.8645, 0.0], // normalized (0.4,0.3,0.86)
            sun_col: [0.92, 0.89, 0.84, 0.0],
            amb_up: [0.20, 0.24, 0.31, 0.0],
            amb_dn: [0.075, 0.070, 0.065, 0.0],
            hemi: [0.0, 0.0, 1.0, 0.1],
            water: [0.0; 4],
        }
    }
}

/// The multisample count every pipeline and the scene framebuffer use
/// (C11-F1: 4×MSAA across terrain/flight/vab/map — before this there
/// was no AA anywhere in the pipeline). Kept in one constant so a
/// sample-count mismatch between a pipeline and its attachments is
/// impossible by construction.
pub const MSAA_SAMPLES: u32 = 4;

/// The renderer-side floor for the scene render scale (P1) —
/// [`Gpu::set_scene_scale`] clamps into `SCENE_SCALE_MIN..=1.0`. The
/// player-facing ladder (`csp-ui`'s `RENDER_SCALE_MIN`) is narrower
/// policy on top of this safety bound.
pub const SCENE_SCALE_MIN: f32 = 0.25;

/// The frame-latency range [`Gpu::new`] clamps its `frame_latency`
/// argument into — the same range the app's `CSP_FRAME_LATENCY`
/// accepts. The floor is every backend's contract (wgpu-hal requires
/// the range to start at 1); 5 is policy, not a wgpu limit — its docs
/// call 1–3 typical and expect higher values to be clamped by the
/// backend anyway.
pub const FRAME_LATENCY_MIN: u32 = 1;
/// See [`FRAME_LATENCY_MIN`].
pub const FRAME_LATENCY_MAX: u32 = 5;

/// Resolve a present-mode request against the modes a surface offers.
/// Pure — the one negotiation [`Gpu::new`] and [`Gpu::set_present_mode`]
/// both run, so the mode this crate stores (and the app logs) is the
/// one wgpu configures: wgpu-core 30.0.1 (`validate_surface_configuration`,
/// `device/surface_config.rs`) takes a concrete mode the surface lists,
/// refuses one it does not, and resolves `AutoVsync` to the first of
/// `[FifoRelaxed, Fifo]` and `AutoNoVsync` to the first of
/// `[Immediate, Mailbox, Fifo]` the surface lists. The fallback lists
/// here mirror that function, in its order — change them together.
/// `None` when nothing resolves: a concrete mode the surface does not
/// offer, or an empty `offered` (an incompatible surface).
pub fn resolve_present_mode(
    requested: wgpu::PresentMode,
    offered: &[wgpu::PresentMode],
) -> Option<wgpu::PresentMode> {
    let fallbacks: &[wgpu::PresentMode] = match requested {
        wgpu::PresentMode::AutoVsync => &[wgpu::PresentMode::FifoRelaxed, wgpu::PresentMode::Fifo],
        wgpu::PresentMode::AutoNoVsync => &[
            wgpu::PresentMode::Immediate,
            wgpu::PresentMode::Mailbox,
            wgpu::PresentMode::Fifo,
        ],
        _ => return offered.contains(&requested).then_some(requested),
    };
    fallbacks
        .iter()
        .copied()
        .find(|fallback| offered.contains(fallback))
}

/// The present-mode list a re-negotiation resolves against: the one the
/// surface reports now, or — when that is empty, i.e. the surface has
/// gone incompatible with its adapter — the list stored at the last
/// successful negotiation.
pub(crate) fn effective_offered_modes<'a>(
    stored: &'a [wgpu::PresentMode],
    fresh: &'a [wgpu::PresentMode],
) -> &'a [wgpu::PresentMode] {
    if fresh.is_empty() { stored } else { fresh }
}

/// The scaled scene-target size for `surface` at `scale` (P1): each
/// axis `round(surface × scale)`, floored at 1 px, `scale` clamped
/// into `SCENE_SCALE_MIN..=1.0` (non-finite → 1.0). Pure — the
/// target-math core `Gpu` and the app's renderer resizes share, and
/// the unit tests pin. Uniform per-axis (aspect never changes).
pub fn scene_target_size(surface: (u32, u32), scale: f32) -> (u32, u32) {
    let s = if scale.is_finite() {
        scale.clamp(SCENE_SCALE_MIN, 1.0)
    } else {
        1.0
    };
    let axis = |v: u32| (f64::from(v) * f64::from(s)).round().max(1.0) as u32;
    (axis(surface.0), axis(surface.1))
}

/// The `MultisampleState` every surface-destined pipeline declares.
pub(crate) fn msaa_state() -> wgpu::MultisampleState {
    wgpu::MultisampleState {
        count: MSAA_SAMPLES,
        ..wgpu::MultisampleState::default()
    }
}

/// The scene pass's multisampled color target: every surface-bound
/// pipeline renders into this 4× texture and the pass resolves into
/// the swapchain view (scale 1.0) or the scaled resolve intermediate
/// (P1 render scale). Recreated when the surface size, format, or
/// scene scale changes (resize/reconfigure/scale change).
pub struct MsaaColor {
    /// Kept alive for `view` — a wgpu `TextureView` is an unborrowed
    /// handle, so dropping the texture would leave a dangling view.
    #[allow(dead_code)]
    tex: wgpu::Texture,
    view: wgpu::TextureView,
    size: (u32, u32),
    format: wgpu::TextureFormat,
}

impl MsaaColor {
    /// Create the multisampled target for `size`/`format`.
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat, size: (u32, u32)) -> Self {
        let size = (size.0.max(1), size.1.max(1));
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("msaa-color"),
            size: wgpu::Extent3d {
                width: size.0,
                height: size.1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: MSAA_SAMPLES,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        Self {
            tex,
            view,
            size,
            format,
        }
    }

    /// Recreate the target when `size`/`format` moved (surface resize
    /// or reconfigure); a no-op otherwise.
    pub fn ensure(&mut self, device: &wgpu::Device, format: wgpu::TextureFormat, size: (u32, u32)) {
        let size = (size.0.max(1), size.1.max(1));
        if size != self.size || format != self.format {
            *self = Self::new(device, format, size);
        }
    }

    /// The attachment view the scene pass renders into (resolve
    /// target = the frame's surface view).
    pub fn view(&self) -> &wgpu::TextureView {
        &self.view
    }
}

/// A grow-only GPU vertex buffer + the count it currently holds — shared
/// by the simple pass renderers (map, VAB). Recreated with headroom when
/// the frame's mesh outgrows it so steady-state frames never reallocate.
pub(crate) struct GpuBuffer {
    /// The wgpu buffer.
    pub buffer: wgpu::Buffer,
    /// Allocated bytes.
    pub capacity: u64,
}

impl GpuBuffer {
    pub(crate) fn new(device: &wgpu::Device, label: &'static str, capacity: u64) -> Self {
        Self {
            buffer: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: capacity,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            capacity,
        }
    }

    /// Upload `verts`, growing the buffer when needed. Returns the
    /// vertex count to draw (0 ⇒ skip the draw call).
    pub(crate) fn upload<V: bytemuck::Pod>(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        verts: &[V],
    ) -> u32 {
        let bytes = bytemuck::cast_slice(verts);
        let needed = bytes.len() as u64;
        if needed > self.capacity {
            *self = Self::new(device, "vtx-grow", needed.next_power_of_two());
        }
        if needed > 0 {
            queue.write_buffer(&self.buffer, 0, bytes);
        }
        verts.len() as u32
    }
}

/// Everything needed to render frames into a window surface.
pub struct Gpu {
    pub instance: wgpu::Instance,
    pub surface: wgpu::Surface<'static>,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub config: wgpu::SurfaceConfiguration,
    /// The scene pass's multisampled framebuffer — pipelines render
    /// into [`MsaaColor::view`] and resolve into the frame view (full
    /// scale) or [`Self::scene_resolve`] (P1 render scale).
    msaa: MsaaColor,
    /// The scene's render scale (P1) in `SCENE_SCALE_MIN..=1.0`;
    /// 1.0 = the pre-P1 resolve-straight-to-swapchain path verbatim.
    scene_scale: f32,
    /// The scaled, single-sampled intermediate the scene pass resolves
    /// into when `scene_scale < 1.0` (`None` at 1.0): RENDER_ATTACHMENT
    /// for the resolve + TEXTURE_BINDING for [`Self::blit_scene`]'s
    /// upscale pass. Recreated by `resize`/`set_scene_scale`.
    scene_resolved: Option<SceneResolved>,
    /// The upscale pipeline + sampler (P1); the per-texture bind group
    /// rides `scene_resolved` (recreated with it).
    blit: BlitPipeline,
    /// Set by the device-lost callback; `begin_frame` degrades to the
    /// reconfigure path instead of feeding commands to a dead device.
    device_lost: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Warn-once latches for `begin_frame`'s degraded acquire outcomes
    /// (`Cell` — `begin_frame` takes `&self`). Bit 0: skip-class
    /// (Timeout/Occluded), bit 1: Validation, bit 2: device-lost.
    surface_warned: std::cell::Cell<u8>,
    /// The present modes the surface offered at its last successful
    /// negotiation, in the driver's order — what [`Self::set_present_mode`]
    /// falls back to when a fresh query reports nothing.
    present_modes: Vec<wgpu::PresentMode>,
    /// The frame latency the surface is configured with.
    frame_latency: u32,
}

/// The scene-resolve intermediate (P1): the scaled texture the MSAA
/// scene pass resolves into, bound for the upscale pass that follows.
struct SceneResolved {
    #[allow(dead_code)] // kept alive for `view`/`bind`
    tex: wgpu::Texture,
    view: wgpu::TextureView,
    bind: wgpu::BindGroup,
    size: (u32, u32),
}

/// The upscale pass's device-fixed half (P1): pipeline, bind layout,
/// sampler. Built once in `Gpu::new` — cheap, and always ready so a
/// scale change never rebuilds pipeline state.
struct BlitPipeline {
    pipe: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
}

impl Gpu {
    /// Create a GPU context bound to a window. `window` must be an owned
    /// handle (e.g. `Arc<Window>`) so the surface outlives the call.
    /// `present_mode` is the vsync choice (P1 — the app maps its
    /// settings enum onto wgpu's modes), concrete or `Auto*`: it is
    /// resolved against the surface's offered list by
    /// [`resolve_present_mode`], and the resolved mode is the one
    /// stored and reported. `frame_latency` is the requested frames in
    /// flight, clamped into `FRAME_LATENCY_MIN..=FRAME_LATENCY_MAX`;
    /// wgpu-core clamps it again to the backend's own range, which the
    /// public API does not expose, so [`Self::frame_latency`] reports
    /// the request rather than that second clamp. `scene_scale` is the
    /// initial render scale (P1, 1.0 = full resolution).
    pub async fn new(
        window: impl Into<wgpu::SurfaceTarget<'static>>,
        size: (u32, u32),
        present_mode: wgpu::PresentMode,
        frame_latency: u32,
        scene_scale: f32,
    ) -> Result<Self, GpuError> {
        // PRIMARY auto-selects Vulkan/Metal/DX12 natively, WebGPU on wasm.
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });

        let surface = instance.create_surface(window)?;

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                ..Default::default()
            })
            .await?;

        let limits = if cfg!(target_arch = "wasm32") {
            wgpu::Limits::downlevel_webgl2_defaults()
        } else {
            wgpu::Limits::default()
        };

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("csp"),
                required_features: wgpu::Features::empty(),
                required_limits: limits,
                ..Default::default()
            })
            .await?;

        // PLAN §2.13 classes presentation faults as Cosmetic —
        // "log once, placeholder, continue". wgpu's DEFAULT uncaptured
        // handler panics, which contradicts that policy (a stray
        // validation error would kill the app), so install a logging
        // handler instead. Errors repeat on real faults — dedup by
        // message keeps a persistent fault one line, not per-frame
        // spam; the cap keeps an error storm bounded.
        device.on_uncaptured_error(std::sync::Arc::new(move |err: wgpu::Error| {
            use std::sync::{Mutex, OnceLock};
            static SEEN: OnceLock<Mutex<std::collections::BTreeSet<String>>> = OnceLock::new();
            let seen = SEEN.get_or_init(|| Mutex::new(std::collections::BTreeSet::new()));
            let msg = err.to_string();
            let mut guard = seen.lock().unwrap_or_else(|e| e.into_inner());
            if guard.len() < 64 && guard.insert(msg.clone()) {
                log::error!("wgpu uncaptured error: {msg}");
            }
        }));
        // A lost device cannot be re-requested from inside the
        // callback — flag it so `begin_frame` degrades to the surface
        // reconfigure path instead of panicking or hanging on dead
        // submissions.
        let device_lost = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        {
            let flag = device_lost.clone();
            device.set_device_lost_callback(move |reason, message| {
                if !flag.swap(true, std::sync::atomic::Ordering::Relaxed) {
                    log::error!("wgpu device lost ({reason:?}): {message}");
                }
            });
        }

        let caps = surface.get_capabilities(&adapter);
        let (format, present_mode) = Self::negotiate_surface(&caps, present_mode)?;

        let frame_latency = frame_latency.clamp(FRAME_LATENCY_MIN, FRAME_LATENCY_MAX);
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width: size.0.max(1),
            height: size.1.max(1),
            // Never the request: wgpu hands a configuration back as it
            // was passed, so an `Auto*` stored here would have
            // `present_mode` report a mode no frame runs under.
            present_mode,
            desired_maximum_frame_latency: frame_latency,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
        };
        surface.configure(&device, &config);
        // P1: the upscale pass's fixed half exists from the start (a
        // small pipeline; the resolved texture + bind group come and go
        // with the scale).
        let blit = BlitPipeline::new(&device, format);
        let scene_scale = if scene_scale.is_finite() {
            scene_scale.clamp(SCENE_SCALE_MIN, 1.0)
        } else {
            1.0
        };
        let scene = scene_target_size((config.width, config.height), scene_scale);
        let msaa = MsaaColor::new(&device, format, scene);
        let scene_resolved =
            (scene_scale < 1.0).then(|| SceneResolved::new(&device, format, scene, &blit));

        Ok(Self {
            instance,
            surface,
            adapter,
            device,
            queue,
            config,
            msaa,
            scene_scale,
            scene_resolved,
            blit,
            device_lost,
            surface_warned: std::cell::Cell::new(0),
            present_modes: caps.present_modes,
            frame_latency,
        })
    }

    /// The surface's format and concrete present mode, negotiated from
    /// what it offers the adapter. An incompatible surface — one that
    /// reports no format — or a request nothing resolves to is a
    /// refusal, never a panic or a failed configure.
    fn negotiate_surface(
        caps: &wgpu::SurfaceCapabilities,
        requested: wgpu::PresentMode,
    ) -> Result<(wgpu::TextureFormat, wgpu::PresentMode), GpuError> {
        // The capability query itself cannot fail: a surface that is
        // incompatible with the adapter reports empty lists instead, and
        // indexing them would panic.
        let refused = || GpuError::PresentMode {
            requested,
            offered: caps.present_modes.clone(),
        };
        if caps.formats.is_empty() {
            return Err(refused());
        }
        let Some(present_mode) = resolve_present_mode(requested, &caps.present_modes) else {
            return Err(refused());
        };
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or_else(|| {
                // Every pipeline tone-maps into the attachment expecting
                // a hardware sRGB encode — a non-sRGB fallback renders
                // ~2.2× dark. Warn loudly rather than silently shipping
                // a wrong-gamma frame (C11-F8).
                log::warn!(
                    "surface offers no sRGB format — falling back to {:?}; \
                     output will render dark",
                    caps.formats[0]
                );
                caps.formats[0]
            });
        Ok((format, present_mode))
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width > 0 && height > 0 {
            self.config.width = width;
            self.config.height = height;
            self.surface.configure(&self.device, &self.config);
            self.refresh_scene_targets();
        }
    }

    /// (Re)build the scaled scene attachments at the current surface
    /// size + scale — the shared tail of `resize` and
    /// `set_scene_scale` (P1). The MSAA target and the resolve
    /// intermediate always agree on size; at scale 1.0 the intermediate
    /// is dropped and the scene pass resolves straight into the frame
    /// view, byte-identical to the pre-P1 path.
    fn refresh_scene_targets(&mut self) {
        let scene = self.scene_size();
        self.msaa.ensure(&self.device, self.config.format, scene);
        self.scene_resolved = if self.scene_scale < 1.0 {
            let stale = self.scene_resolved.as_ref().is_none_or(|r| r.size != scene);
            if stale {
                Some(SceneResolved::new(
                    &self.device,
                    self.config.format,
                    scene,
                    &self.blit,
                ))
            } else {
                self.scene_resolved.take()
            }
        } else {
            None
        };
    }

    /// The present mode in force (P1's vsync plumbing) — the concrete
    /// mode the surface is configured with, i.e. the one frames run
    /// under, never an `Auto*` request.
    pub fn present_mode(&self) -> wgpu::PresentMode {
        self.config.present_mode
    }

    /// The present modes the surface offered at its last successful
    /// negotiation, in the driver's order.
    pub fn offered_present_modes(&self) -> &[wgpu::PresentMode] {
        &self.present_modes
    }

    /// The frame latency the surface is configured with.
    pub fn frame_latency(&self) -> u32 {
        self.frame_latency
    }

    /// Negotiate a new present mode — a surface reconfigure on the same
    /// path `resize` takes, so it lands at the next frame; returns the
    /// mode that won, which is never an `Auto*` request. The request
    /// (concrete or `Auto*`) resolves by [`resolve_present_mode`]
    /// against the list the surface offers *now* — falling back to the
    /// stored list when the fresh query reports nothing. Whether an
    /// off-vsync request takes effect is therefore the platform's and
    /// driver's call: on a surface that offers `Immediate` or `Mailbox`
    /// it uncaps the frame loop, on one that offers only `Fifo` it
    /// resolves to `Fifo`. A request that resolves to nothing is `Err`
    /// and leaves the surface untouched — a concrete mode the surface
    /// does not list cannot be configured, and wgpu would keep the old
    /// swapchain anyway. No target rebuilds: present mode never touches
    /// attachment sizes.
    pub fn set_present_mode(
        &mut self,
        mode: wgpu::PresentMode,
    ) -> Result<wgpu::PresentMode, GpuError> {
        let fresh = self.surface.get_capabilities(&self.adapter).present_modes;
        let offered = effective_offered_modes(&self.present_modes, &fresh);
        let Some(resolved) = resolve_present_mode(mode, offered) else {
            return Err(GpuError::PresentMode {
                requested: mode,
                offered: offered.to_vec(),
            });
        };
        if !fresh.is_empty() {
            self.present_modes = fresh;
        }
        if self.config.present_mode != resolved {
            self.config.present_mode = resolved;
            self.surface.configure(&self.device, &self.config);
        }
        Ok(resolved)
    }

    /// The live scene render scale (P1).
    pub fn scene_scale(&self) -> f32 {
        self.scene_scale
    }

    /// The scaled size the scene color/depth attachments render at
    /// (P1) — what the app feeds the renderers' own depth-target
    /// resizes. Aspect matches the surface (uniform scale).
    pub fn scene_size(&self) -> (u32, u32) {
        scene_target_size((self.config.width, self.config.height), self.scene_scale)
    }

    /// Set the scene render scale (P1): clamped into
    /// `SCENE_SCALE_MIN..=1.0` (non-finite → 1.0), then the scaled
    /// targets rebuild exactly as a resize rebuilds them. Only the
    /// scene attachments move — the swapchain, egui, and the pick
    /// pass stay native.
    pub fn set_scene_scale(&mut self, scale: f32) {
        let s = if scale.is_finite() {
            scale.clamp(SCENE_SCALE_MIN, 1.0)
        } else {
            1.0
        };
        if s != self.scene_scale {
            self.scene_scale = s;
            self.refresh_scene_targets();
        }
    }

    /// The resolve target the scene pass should resolve into (P1):
    /// `None` at scale 1.0 — resolve straight into the frame view (the
    /// pre-P1 path); `Some` at a reduced scale — resolve here, then
    /// [`Self::blit_scene`] upscales into the frame before the UI
    /// pass.
    pub fn scene_resolve(&self) -> Option<&wgpu::TextureView> {
        self.scene_resolved.as_ref().map(|r| &r.view)
    }

    /// Record the scene upscale pass (P1): the scaled resolved image →
    /// `dst` (the swapchain frame view), one fullscreen linear-filtered
    /// triangle. A no-op at scale 1.0 — that path resolved into the
    /// frame directly and must stay byte-identical to pre-P1 frames.
    pub fn blit_scene(&self, encoder: &mut wgpu::CommandEncoder, dst: &wgpu::TextureView) {
        if let Some(resolved) = &self.scene_resolved {
            self.blit.record(encoder, dst, &resolved.bind);
        }
    }

    pub fn surface_format(&self) -> wgpu::TextureFormat {
        self.config.format
    }

    /// The multisampled attachment the scene pass renders into — at
    /// the scene scale (P1); the resolve target is [`Self::scene_resolve`]
    /// when one exists, else the frame's surface view.
    pub fn msaa_view(&self) -> &wgpu::TextureView {
        self.msaa.view()
    }

    /// Log a degraded acquire outcome once per class — the flags make
    /// a persistent fault (a wedged swapchain, a recurring validation
    /// error) a single line instead of per-frame spam.
    fn warn_surface_once(&self, bit: u8, msg: &str) {
        let flags = self.surface_warned.get();
        if flags & bit == 0 {
            self.surface_warned.set(flags | bit);
            log::warn!("surface: {msg}");
        }
    }

    /// Acquire the next surface texture and an encoder for it.
    pub fn begin_frame(&self) -> FrameResult {
        if self.device_lost.load(std::sync::atomic::Ordering::Relaxed) {
            // The device-lost callback already logged once; degrade to
            // the reconfigure path — the only recovery a dead device
            // still allows — rather than submitting work that either
            // panics validation or silently vanishes.
            self.warn_surface_once(
                0b100,
                "device lost — attempting surface reconfigure each frame",
            );
            return FrameResult::Reconfigure;
        }
        let texture = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t)
            | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            wgpu::CurrentSurfaceTexture::Lost | wgpu::CurrentSurfaceTexture::Outdated => {
                return FrameResult::Reconfigure;
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                // A validation error is a real fault, not a transient
                // state — report it and ask the caller to reconfigure,
                // which is the only available recovery attempt.
                self.warn_surface_once(
                    0b10,
                    "validation error in get_current_texture — reconfiguring",
                );
                return FrameResult::Reconfigure;
            }
            wgpu::CurrentSurfaceTexture::Timeout => {
                self.warn_surface_once(
                    0b01,
                    "timed out acquiring a frame — skipping until it recovers",
                );
                return FrameResult::Skip;
            }
            wgpu::CurrentSurfaceTexture::Occluded => {
                self.warn_surface_once(
                    0b01,
                    "surface occluded — skipping frames until it is visible again",
                );
                return FrameResult::Skip;
            }
        };
        let view = texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame"),
            });
        FrameResult::Ok(Box::new(Frame {
            texture,
            view,
            encoder,
        }))
    }

    pub fn submit(&self, frame: Frame) {
        self.queue.submit([frame.encoder.finish()]);
        self.queue.present(frame.texture);
    }
}

impl SceneResolved {
    /// The scaled resolve intermediate: resolve target of the scene
    /// pass, sample source of the upscale pass.
    fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        size: (u32, u32),
        blit: &BlitPipeline,
    ) -> Self {
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("scene-resolved"),
            size: wgpu::Extent3d {
                width: size.0.max(1),
                height: size.1.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            // Resolve target of the MSAA scene pass + sampled by the
            // upscale pass — both usages, nothing else touches it.
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("scene-blit"),
            layout: &blit.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&blit.sampler),
                },
            ],
        });
        Self {
            tex,
            view,
            bind,
            size: (size.0.max(1), size.1.max(1)),
        }
    }
}

impl BlitPipeline {
    /// Record the upscale pass (P1) — the exact pass `Gpu::blit_scene`
    /// records, factored out so the headless scale-dump rig drives the
    /// production path verbatim: fullscreen triangle sampling `src`
    /// (the resolved scene's bind group) into `dst`.
    fn record(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        dst: &wgpu::TextureView,
        src: &wgpu::BindGroup,
    ) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("scene-blit"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: dst,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(&self.pipe);
        pass.set_bind_group(0, src, &[]);
        pass.draw(0..3, 0..1);
    }

    /// The upscale pipeline (P1): fullscreen triangle, linear sampler,
    /// surface format, no depth, single sample.
    fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("blit"),
            source: wgpu::ShaderSource::Wgsl(include_str!("blit.wgsl").into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("scene-blit"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        // The resolved scene is the surface format
                        // (sRGB) — a float, filterable sample (the
                        // linear upscale needs it); the sRGB decode
                        // happens in the sampler, the re-encode at the
                        // output merge.
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipe_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("scene-blit"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipe = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("scene-blit"),
            layout: Some(&pipe_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: None, // replace — the whole frame is the blit
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..wgpu::PrimitiveState::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("scene-blit"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..wgpu::SamplerDescriptor::default()
        });
        Self {
            pipe,
            layout,
            sampler,
        }
    }
}

/// Result of [`Gpu::begin_frame`].
pub enum FrameResult {
    Ok(Box<Frame>),
    /// Surface lost/outdated — or an acquire-time validation error,
    /// which the same reconfigure is the only recovery for. Call
    /// [`Gpu::resize`] with current size, skip the frame.
    Reconfigure,
    /// Nothing renderable this frame (timeout, occluded) — transient;
    /// retry next frame.
    Skip,
}

pub struct Frame {
    pub texture: wgpu::SurfaceTexture,
    pub view: wgpu::TextureView,
    pub encoder: wgpu::CommandEncoder,
}

#[derive(Debug)]
pub enum GpuError {
    Surface(wgpu::CreateSurfaceError),
    Adapter(wgpu::RequestAdapterError),
    Device(wgpu::RequestDeviceError),
    /// The surface cannot carry `requested` — it offers none of the
    /// modes that request resolves to. An empty `offered` is a surface
    /// incompatible with the adapter (the capability query reports
    /// empty lists rather than failing).
    PresentMode {
        requested: wgpu::PresentMode,
        offered: Vec<wgpu::PresentMode>,
    },
}

impl std::fmt::Display for GpuError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Surface(e) => write!(f, "surface: {e}"),
            Self::Adapter(e) => write!(f, "adapter: {e}"),
            Self::Device(e) => write!(f, "device: {e}"),
            Self::PresentMode { requested, offered } if offered.is_empty() => write!(
                f,
                "surface offers no present modes (incompatible with the adapter); requested {requested:?}"
            ),
            Self::PresentMode { requested, offered } => write!(
                f,
                "requested present mode {requested:?} is not in the list of supported present modes: {offered:?}"
            ),
        }
    }
}

impl std::error::Error for GpuError {}

impl From<wgpu::CreateSurfaceError> for GpuError {
    fn from(e: wgpu::CreateSurfaceError) -> Self {
        Self::Surface(e)
    }
}
impl From<wgpu::RequestAdapterError> for GpuError {
    fn from(e: wgpu::RequestAdapterError) -> Self {
        Self::Adapter(e)
    }
}
impl From<wgpu::RequestDeviceError> for GpuError {
    fn from(e: wgpu::RequestDeviceError) -> Self {
        Self::Device(e)
    }
}

#[cfg(test)]
// Tests signal failure by panicking (§2.13's no-panic rule is a shipping
// contract, not a test contract).
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::scene_target_size;
    use super::{
        BlitPipeline, Gpu, GpuError, MsaaColor, SceneResolved, effective_offered_modes, map,
        resolve_present_mode,
    };

    /// Every WGSL source this crate feeds `create_shader_module` must
    /// pass the same checks wgpu runs eagerly at GPU init: naga parse +
    /// full validation. An invalid module panics device-side and kills
    /// the app on launch (the M3-D case: an integer varying without
    /// `@interpolate(flat)` → InvalidInterpolationForInteger), so this
    /// test is the gate that keeps shader regressions inside
    /// `cargo test`. `Capabilities::all()` keeps the check permissive —
    /// it gates structural validity, not device feature usage.
    #[test]
    fn shaders_parse_and_validate() {
        for (name, src) in [
            ("map.wgsl", include_str!("map.wgsl")),
            ("vab.wgsl", include_str!("vab.wgsl")),
            ("flight.wgsl", include_str!("flight.wgsl")),
            ("flight_shadow.wgsl", include_str!("flight_shadow.wgsl")),
            ("terrain.wgsl", include_str!("terrain.wgsl")),
            ("blit.wgsl", include_str!("blit.wgsl")),
        ] {
            let module = naga::front::wgsl::parse_str(src)
                .unwrap_or_else(|e| panic!("{name}: parse failed\n{}", e.emit_to_string(src)));
            let mut validator = naga::valid::Validator::new(
                naga::valid::ValidationFlags::all(),
                naga::valid::Capabilities::all(),
            );
            validator
                .validate(&module)
                .unwrap_or_else(|e| panic!("{name}: validation failed\n{e:?}"));
        }
    }

    /// P1's scene-target math: `scale × surface` per axis (rounded to
    /// nearest, so 0.05 steps land on whole pixels where the product
    /// allows), the 1-px floor (a degenerate axis must never produce a
    /// 0-size attachment), the scale clamp, and the non-finite guard.
    #[test]
    fn scene_target_size_scales_and_floors() {
        // The settings ladder's exact steps at a common resolution.
        assert_eq!(scene_target_size((1920, 1080), 1.0), (1920, 1080));
        assert_eq!(scene_target_size((1920, 1080), 0.75), (1440, 810));
        assert_eq!(scene_target_size((1920, 1080), 0.5), (960, 540));
        assert_eq!(scene_target_size((1280, 720), 0.95), (1216, 684));
        // Rounding, not truncation: 0.85 × 1080 = 918 exactly, but odd
        // surfaces at 0.65 land on halves — nearest keeps the aspect
        // drift under half a pixel per axis.
        assert_eq!(scene_target_size((1921, 1081), 0.65), (1249, 703));
        // The 1-px floor — whatever the scale, an attachment axis is
        // never 0 (wgpu would reject the descriptor; a 1-px scene is
        // the honest degenerate).
        assert_eq!(scene_target_size((1, 1), 0.5), (1, 1));
        assert_eq!(scene_target_size((2, 3), 0.3), (1, 1));
        // The clamp: below SCENE_SCALE_MIN floors at it, above 1.0 is
        // full res (never an upscale), non-finite falls back to 1.0.
        assert_eq!(scene_target_size((1000, 1000), 0.01), (250, 250));
        assert_eq!(scene_target_size((1000, 1000), 4.0), (1000, 1000));
        assert_eq!(scene_target_size((1000, 1000), f32::NAN), (1000, 1000));
        assert_eq!(scene_target_size((1000, 1000), f32::INFINITY), (1000, 1000));
        // Aspect: the scaled size's aspect matches the surface's to
        // within half a pixel per axis (uniform scale).
        for &s in &[0.5f32, 0.65, 0.75, 0.9, 1.0] {
            let (w, h) = scene_target_size((1920, 1080), s);
            let drift = (f64::from(w) / f64::from(h) - 1920.0 / 1080.0).abs() * f64::from(h);
            assert!(drift < 1.0, "scale {s}: aspect drift {drift}px");
        }
    }

    /// The present-mode negotiation mirrors wgpu-core 30.0.1's own: an
    /// `Auto*` request takes the first of its fallback list the surface
    /// offers (so the mode this crate logs is the one wgpu configures),
    /// a concrete mode is honoured only when offered, and nothing
    /// resolves against an empty list.
    #[test]
    fn resolve_present_mode_mirrors_wgpu_core_fallbacks() {
        use wgpu::PresentMode::{AutoNoVsync, AutoVsync, Fifo, FifoRelaxed, Immediate, Mailbox};
        // AutoVsync → the first of [FifoRelaxed, Fifo]: relaxed wins
        // where offered, plain Fifo where it is not.
        assert_eq!(
            resolve_present_mode(AutoVsync, &[Fifo, FifoRelaxed, Mailbox]),
            Some(FifoRelaxed)
        );
        assert_eq!(
            resolve_present_mode(AutoVsync, &[Fifo, Mailbox, Immediate]),
            Some(Fifo)
        );
        // AutoNoVsync → the first of [Immediate, Mailbox, Fifo].
        assert_eq!(
            resolve_present_mode(AutoNoVsync, &[Fifo, Mailbox]),
            Some(Mailbox)
        );
        assert_eq!(
            resolve_present_mode(AutoNoVsync, &[Fifo, FifoRelaxed]),
            Some(Fifo)
        );
        // The offer list's order is the driver's — the fallback list's
        // order is what decides, not it.
        assert_eq!(
            resolve_present_mode(AutoNoVsync, &[Mailbox, Immediate, Fifo]),
            Some(Immediate)
        );
        // A concrete mode is honoured only when offered.
        assert_eq!(resolve_present_mode(FifoRelaxed, &[Fifo, Mailbox]), None);
        assert_eq!(
            resolve_present_mode(Mailbox, &[Fifo, Mailbox]),
            Some(Mailbox)
        );
        // An empty list — an incompatible surface — resolves nothing.
        for request in [
            AutoVsync,
            AutoNoVsync,
            Fifo,
            FifoRelaxed,
            Mailbox,
            Immediate,
        ] {
            assert_eq!(resolve_present_mode(request, &[]), None, "{request:?}");
        }
    }

    /// A re-negotiation resolves against the list the surface reports
    /// now, and against the stored list only when that fresh query is
    /// empty (a surface that has gone incompatible with its adapter).
    #[test]
    fn effective_offered_modes_prefers_the_fresh_list() {
        use wgpu::PresentMode::{Fifo, Mailbox};
        let stored = [Fifo];
        let fresh = [Fifo, Mailbox];
        assert_eq!(effective_offered_modes(&stored, &fresh), fresh.as_slice());
        assert_eq!(effective_offered_modes(&stored, &[]), stored.as_slice());
    }

    /// The refusal names both sides of the failed negotiation — the
    /// incompatible-surface form (empty offer list) and the plain
    /// unsupported-mode form alike.
    #[test]
    fn present_mode_error_names_request_and_offered_modes() {
        use wgpu::PresentMode::{AutoVsync, Fifo, FifoRelaxed, Immediate};
        let refused = GpuError::PresentMode {
            requested: Immediate,
            offered: vec![Fifo, FifoRelaxed],
        };
        assert_eq!(
            refused.to_string(),
            "requested present mode Immediate is not in the list of supported present modes: \
             [Fifo, FifoRelaxed]"
        );
        let incompatible = GpuError::PresentMode {
            requested: AutoVsync,
            offered: Vec::new(),
        };
        assert_eq!(
            incompatible.to_string(),
            "surface offers no present modes (incompatible with the adapter); requested AutoVsync"
        );
    }

    /// `Gpu::new`'s surface negotiation refuses instead of panicking: a
    /// surface with no formats (incompatible with the adapter) and a
    /// request that resolves to nothing both come back as the error
    /// naming the request and the offer list; a healthy surface yields
    /// its first sRGB format — or the first one on offer — and the
    /// resolved mode.
    #[test]
    fn negotiate_surface_refuses_instead_of_panicking() {
        use wgpu::PresentMode::{AutoNoVsync, AutoVsync, Fifo, FifoRelaxed, Immediate, Mailbox};
        use wgpu::TextureFormat::{Bgra8Unorm, Bgra8UnormSrgb, Rgba8UnormSrgb};
        use wgpu::{CompositeAlphaMode, SurfaceCapabilities};
        let caps =
            |formats: &[wgpu::TextureFormat], modes: &[wgpu::PresentMode]| SurfaceCapabilities {
                formats: formats.to_vec(),
                present_modes: modes.to_vec(),
                alpha_modes: vec![CompositeAlphaMode::Opaque],
                ..Default::default()
            };
        // An incompatible surface reports empty lists.
        let err = Gpu::negotiate_surface(&caps(&[], &[]), AutoVsync).unwrap_err();
        assert!(
            matches!(
                err,
                GpuError::PresentMode {
                    requested: AutoVsync,
                    ref offered
                } if offered.is_empty()
            ),
            "{err}"
        );
        // A concrete mode the surface does not list.
        let err = Gpu::negotiate_surface(&caps(&[Rgba8UnormSrgb], &[Fifo, Mailbox]), Immediate)
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "requested present mode Immediate is not in the list of supported present modes: \
             [Fifo, Mailbox]"
        );
        // A healthy surface: the first sRGB format, the resolved mode.
        let negotiated = Gpu::negotiate_surface(
            &caps(
                &[Bgra8Unorm, Bgra8UnormSrgb, Rgba8UnormSrgb],
                &[Fifo, FifoRelaxed, Mailbox],
            ),
            AutoVsync,
        )
        .unwrap();
        assert_eq!(negotiated, (Bgra8UnormSrgb, FifoRelaxed));
        let negotiated =
            Gpu::negotiate_surface(&caps(&[Rgba8UnormSrgb], &[Fifo, Mailbox]), AutoNoVsync)
                .unwrap();
        assert_eq!(negotiated, (Rgba8UnormSrgb, Mailbox));
        // No sRGB format on offer: the first format is used (and warned
        // about — see `negotiate_surface`).
        let negotiated = Gpu::negotiate_surface(&caps(&[Bgra8Unorm], &[Fifo]), Fifo).unwrap();
        assert_eq!(negotiated, (Bgra8Unorm, Fifo));
    }

    // ------------------------------------------------------------------
    // P1 scale evidence rig (scratch visual dump, the repo's `#[ignore]`
    // convention) — see docs/UI-AND-RENDERING.md §7.2.2.
    // ------------------------------------------------------------------

    /// The dump's frame size. 960×540 (a half-1080p 16:9 frame; 960·4
    /// is a multiple of wgpu's 256-byte copy-row alignment).
    const SCALE_DUMP_W: u32 = 960;
    const SCALE_DUMP_H: u32 = 540;
    /// Any surface-legal format works for the rig; sRGB like the real
    /// surface path.
    const SCALE_DUMP_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

    /// A headless adapter + device for the scale dumps (`None` when no
    /// driver exists — the render gate's documented skip case).
    fn scale_dump_device() -> Option<(wgpu::Device, wgpu::Queue)> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: None,
            ..Default::default()
        }))
        .ok()?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("scale-dump"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            ..wgpu::DeviceDescriptor::default()
        }))
        .ok()?;
        Some((device, queue))
    }

    /// The dump scene: fine 1–2 px line probes at three angles (the
    /// sharpness signal an upscale either keeps or smears), colored
    /// wedges, and the map pipeline's shaped markers (disc / ring /
    /// glow) — all through the REAL scene pipeline (4× MSAA, alpha
    /// blending, resolve), the same pass shape `csp-app` records.
    fn scale_dump_packet() -> map::MapPacket {
        use map::{MapMesh, MapVertex};
        let mut lines = MapMesh::default();
        let px_y = 2.0 / f64::from(SCALE_DUMP_H);
        // Horizontal stripes, 1 and 2 px, alternating hues — a
        // resolution wedge: the top third is 2-px-on/2-px-off, the
        // middle 1-px-on/1-px-off, the bottom 1-px-on/3-px-off.
        let hues = [
            [0.95, 0.30, 0.30, 1.0],
            [0.30, 0.95, 0.45, 1.0],
            [0.35, 0.55, 1.0, 1.0],
            [0.98, 0.80, 0.25, 1.0],
        ];
        let mut y: f64 = -0.95;
        let mut k = 0usize;
        while y < 0.9 {
            let (h, gap) = if y < -0.3 {
                (2.0, 2.0)
            } else if y < 0.3 {
                (1.0, 1.0)
            } else {
                (1.0, 3.0)
            };
            let c = hues[k % hues.len()];
            k += 1;
            lines.vertices.extend_from_slice(&[
                MapVertex {
                    pos: [-1.0, y as f32, 0.5],
                    color: c,
                    uv: [0.0, 0.0],
                    kind: MapVertex::KIND_SOLID,
                },
                MapVertex {
                    pos: [1.0, y as f32, 0.5],
                    color: c,
                    uv: [0.0, 0.0],
                    kind: MapVertex::KIND_SOLID,
                },
                MapVertex {
                    pos: [1.0, (y + h * px_y) as f32, 0.5],
                    color: c,
                    uv: [0.0, 0.0],
                    kind: MapVertex::KIND_SOLID,
                },
                MapVertex {
                    pos: [-1.0, y as f32, 0.5],
                    color: c,
                    uv: [0.0, 0.0],
                    kind: MapVertex::KIND_SOLID,
                },
                MapVertex {
                    pos: [1.0, (y + h * px_y) as f32, 0.5],
                    color: c,
                    uv: [0.0, 0.0],
                    kind: MapVertex::KIND_SOLID,
                },
                MapVertex {
                    pos: [-1.0, (y + h * px_y) as f32, 0.5],
                    color: c,
                    uv: [0.0, 0.0],
                    kind: MapVertex::KIND_SOLID,
                },
            ]);
            y += (h + gap) * px_y;
        }
        // The shaped markers across the bottom half.
        let mut markers = MapMesh::default();
        let quads = [
            (
                -0.72f32,
                -0.55f32,
                MapVertex::KIND_DISC,
                [0.9, 0.5, 0.15, 1.0],
            ),
            (-0.24, -0.55, MapVertex::KIND_RING, [0.4, 0.85, 0.9, 1.0]),
            (0.24, -0.55, MapVertex::KIND_GLOW, [1.0, 0.45, 0.6, 1.0]),
            (0.72, -0.55, MapVertex::KIND_DISC, [0.55, 0.75, 0.35, 1.0]),
        ];
        for (cx, cy, kind, color) in quads {
            for (dx, dy) in [
                (-1.0, -1.0),
                (1.0, -1.0),
                (1.0, 1.0),
                (-1.0, -1.0),
                (1.0, 1.0),
                (-1.0, 1.0),
            ] {
                markers.vertices.push(MapVertex {
                    pos: [cx + dx * 0.10, cy + dy * 0.16, 0.5],
                    color,
                    uv: [dx, dy],
                    kind,
                });
            }
        }
        map::MapPacket {
            lines,
            markers,
            ..Default::default()
        }
    }

    /// One P1-shaped frame at `scale`: the scene renders into the
    /// SCALED MSAA target, resolves into the scaled intermediate (or
    /// the frame directly at 1.0), and the upscale pass brings it to
    /// the full-size frame — the app's `paint` order, minus egui.
    /// Returns the readback bytes.
    #[allow(clippy::too_many_lines)] // one headless frame recipe; the arms differ only in scale
    fn scale_dump_frame(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        renderer: &mut map::MapRenderer,
        packet: &map::MapPacket,
        scale: f32,
    ) -> Vec<u8> {
        let scene = scene_target_size((SCALE_DUMP_W, SCALE_DUMP_H), scale);
        let msaa = MsaaColor::new(device, SCALE_DUMP_FORMAT, scene);
        let blit = BlitPipeline::new(device, SCALE_DUMP_FORMAT);
        let resolved =
            (scale < 1.0).then(|| SceneResolved::new(device, SCALE_DUMP_FORMAT, scene, &blit));
        let frame = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("scale-dump-frame"),
            size: wgpu::Extent3d {
                width: SCALE_DUMP_W,
                height: SCALE_DUMP_H,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: SCALE_DUMP_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let frame_view = frame.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("scale-dump"),
        });
        {
            let resolve_view = resolved.as_ref().map_or(&frame_view, |r| &r.view);
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("scale-dump-scene"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: msaa.view(),
                    resolve_target: Some(resolve_view),
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
            renderer.draw(&mut pass, device, queue, packet);
        }
        if let Some(r) = &resolved {
            blit.record(&mut encoder, &frame_view, &r.bind);
        }
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("scale-dump-readback"),
            size: u64::from(SCALE_DUMP_W * SCALE_DUMP_H * 4),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &frame,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(SCALE_DUMP_W * 4),
                    rows_per_image: Some(SCALE_DUMP_H),
                },
            },
            wgpu::Extent3d {
                width: SCALE_DUMP_W,
                height: SCALE_DUMP_H,
                depth_or_array_layers: 1,
            },
        );
        queue.submit([encoder.finish()]);
        let (tx, rx) = std::sync::mpsc::channel();
        buf.map_async(wgpu::MapMode::Read, .., move |r| {
            let _ = tx.send(r);
        });
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        rx.recv().unwrap().expect("map_async failed");
        let img = buf.get_mapped_range(..).unwrap().to_vec();
        buf.unmap();
        img
    }

    /// P1's evidence dump (`#[ignore]`d scratch visual, the repo's
    /// convention — run with
    /// `cargo test -p csp-render dump_render_scale_frames -- --ignored`):
    /// renders the probe scene through the production scene→resolve→
    /// upscale path at 1.0 / 0.75 / 0.5 into `/tmp/p1-scale/` for the
    /// vision pass, and asserts in-process that scale 1.0 is
    /// byte-identical to the pre-P1 direct resolve (no intermediate,
    /// no blit — the unchanged-path guarantee, pinned rather than
    /// asserted by eyeball).
    #[test]
    #[ignore]
    #[cfg(not(target_arch = "wasm32"))]
    fn dump_render_scale_frames() {
        let Some((device, queue)) = scale_dump_device() else {
            eprintln!("scale dump: SKIP — no headless GPU adapter");
            return;
        };
        let mut renderer = map::MapRenderer::new(&device, SCALE_DUMP_FORMAT);
        let packet = scale_dump_packet();
        let dir = std::path::Path::new("/tmp/p1-scale");
        std::fs::create_dir_all(dir).unwrap();
        let mut frames = Vec::new();
        for &scale in &[1.0f32, 0.75, 0.5] {
            let img = scale_dump_frame(&device, &queue, &mut renderer, &packet, scale);
            let name = format!("scene-{}.ppm", (scale * 100.0).round() as u32);
            let mut out = format!("P6\n{SCALE_DUMP_W} {SCALE_DUMP_H}\n255\n").into_bytes();
            for px in img.chunks_exact(4) {
                out.extend_from_slice(&[px[0], px[1], px[2]]);
            }
            std::fs::write(dir.join(&name), &out).unwrap();
            eprintln!("scale dump: wrote {} ({name})", dir.join(&name).display());
            frames.push((scale, img));
        }
        // The 1.0 determinism pin: two independent submissions of the
        // 1.0 frame reproduce byte-for-byte. The path identity to
        // pre-P1 is structural — `resolved` is `None` at 1.0, so the
        // pass resolves straight into the frame and records no blit —
        // and the render gate's (Gpu-free) dumps staying green is the
        // unchanged-content evidence that matters.
        let (s, full) = &frames[0];
        assert!(
            (*s - 1.0).abs() < f32::EPSILON,
            "the identity arm must be the 1.0 frame"
        );
        let direct = scale_dump_frame(&device, &queue, &mut renderer, &packet, 1.0);
        assert_eq!(
            full, &direct,
            "the 1.0 frame must reproduce byte-for-byte across submissions"
        );
    }
}
