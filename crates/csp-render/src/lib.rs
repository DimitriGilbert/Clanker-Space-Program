#![forbid(unsafe_code)]

//! wgpu device/surface ownership and frame lifecycle.
//!
//! Rendering happens in f32 relative to the active camera — the sim keeps
//! f64 world positions, this crate never sees them.

/// The M1 map-view pass (PLAN §5.1 R0): unlit orbit polylines and markers
/// drawn from a `csp-view`-built [`map::MapPacket`].
pub mod map;

/// The M3 VAB craft view + GPU picking (PLAN M3 exit row "GPU picking",
/// §9 "Editor viewport is `csp-render` + GPU picking"): a depth-tested
/// placeholder-box pass over a `csp-view`-built [`vab::VabPacket`], plus
/// an offscreen object-id pass whose single-pixel readback resolves the
/// part under the cursor.
pub mod vab;

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

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);

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

        Ok(Self {
            instance,
            surface,
            adapter,
            device,
            queue,
            config,
        })
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width > 0 && height > 0 {
            self.config.width = width;
            self.config.height = height;
            self.surface.configure(&self.device, &self.config);
        }
    }

    pub fn surface_format(&self) -> wgpu::TextureFormat {
        self.config.format
    }

    /// Acquire the next surface texture and an encoder for it.
    pub fn begin_frame(&self) -> FrameResult {
        let texture = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t)
            | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            wgpu::CurrentSurfaceTexture::Lost | wgpu::CurrentSurfaceTexture::Outdated => {
                return FrameResult::Reconfigure;
            }
            // Timeout / Occluded / Validation — nothing to draw into.
            _ => return FrameResult::Skip,
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
    /// Surface lost/outdated — call [`Gpu::resize`] with current size, skip the frame.
    Reconfigure,
    /// Nothing renderable this frame (timeout, occluded, validation error).
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
