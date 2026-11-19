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
/// needs no eye position. `mat` on each vertex supplies specular
/// strength/exponent and baked occlusion.
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
        }
    }
}

/// The multisample count every pipeline and the scene framebuffer use
/// (C11-F1: 4×MSAA across terrain/flight/vab/map — before this there
/// was no AA anywhere in the pipeline). Kept in one constant so a
/// sample-count mismatch between a pipeline and its attachments is
/// impossible by construction.
pub const MSAA_SAMPLES: u32 = 4;

/// The `MultisampleState` every surface-destined pipeline declares.
pub(crate) fn msaa_state() -> wgpu::MultisampleState {
    wgpu::MultisampleState {
        count: MSAA_SAMPLES,
        ..wgpu::MultisampleState::default()
    }
}

/// The scene pass's multisampled color target: every surface-bound
/// pipeline renders into this 4× texture and the pass resolves into
/// the swapchain view. Recreated when the surface size or format
/// changes (resize/reconfigure).
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
    /// into [`MsaaColor::view`] and resolve into the surface view.
    msaa: MsaaColor,
    /// Set by the device-lost callback; `begin_frame` degrades to the
    /// reconfigure path instead of feeding commands to a dead device.
    device_lost: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Warn-once latches for `begin_frame`'s degraded acquire outcomes
    /// (`Cell` — `begin_frame` takes `&self`). Bit 0: skip-class
    /// (Timeout/Occluded), bit 1: Validation, bit 2: device-lost.
    surface_warned: std::cell::Cell<u8>,
}

impl Gpu {
    /// Create a GPU context bound to a window. `window` must be an owned
    /// handle (e.g. `Arc<Window>`) so the surface outlives the call.
    pub async fn new(
        window: impl Into<wgpu::SurfaceTarget<'static>>,
        size: (u32, u32),
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

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width: size.0.max(1),
            height: size.1.max(1),
            present_mode: wgpu::PresentMode::AutoVsync,
            desired_maximum_frame_latency: 2,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
        };
        surface.configure(&device, &config);
        let msaa = MsaaColor::new(&device, format, (config.width, config.height));

        Ok(Self {
            instance,
            surface,
            adapter,
            device,
            queue,
            config,
            msaa,
            device_lost,
            surface_warned: std::cell::Cell::new(0),
        })
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width > 0 && height > 0 {
            self.config.width = width;
            self.config.height = height;
            self.surface.configure(&self.device, &self.config);
            self.msaa.ensure(
                &self.device,
                self.config.format,
                (self.config.width, self.config.height),
            );
        }
    }

    pub fn surface_format(&self) -> wgpu::TextureFormat {
        self.config.format
    }

    /// The multisampled attachment the scene pass renders into —
    /// `resolve_target` is the frame's surface view.
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
}

impl std::fmt::Display for GpuError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Surface(e) => write!(f, "surface: {e}"),
            Self::Adapter(e) => write!(f, "adapter: {e}"),
            Self::Device(e) => write!(f, "device: {e}"),
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
}
