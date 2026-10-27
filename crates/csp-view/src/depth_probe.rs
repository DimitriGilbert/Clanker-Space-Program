//! PLAN §5.4's depth-strategy measurement (R1, "Recorded in the perf
//! log" — C16-F5): renders the 10⁷ m test scene — a 1 m-depth
//! reference object crossing a surface at the probe distance —
//! through the **shipped** flight pipeline (reverse-Z f32 clip,
//! `Depth32Float`, `GreaterEqual`, 4×MSAA), twice with the submission
//! order swapped, and counts the pixels whose winner depends on that
//! order. Those are the depth-fighting pixels: where the depth buffer
//! cannot separate the two surfaces, draw order — and in an animated
//! scene, frame-to-frame noise — decides the winner.
//!
//! Why "1 m at 10⁷ m" is a coverage problem: a literal 1 m object
//! subtends ~10⁻⁴ px at 10⁷ m, so the scene keeps the 1 m SCALE (the
//! object's depth extent) and drops the 1 m footprint — two quads
//! spanning the frame's centre band, the "reference object" tilted so
//! it crosses the surface plane mid-frame with a peak separation of
//! 1 m. NDC z is the exact value `extract` emits for a point at that
//! depth (`clip.z = near`, `w = −z_view` ⇒ `z = near/d`; see
//! `map::perspective_revz`), computed in f64 and cast to f32 per
//! vertex — the same quantization the real packet path applies.
//!
//! Native-only: it rasterizes on a caller-supplied headless device
//! (`csp-perf`'s `render.depth-fight-1e7` row and the perf test both
//! drive it; the row is recorded red or green by what this returns —
//! over the §5.4 "1 pixel at 1080p" bound, §2.3's second far-field
//! pass is owed).

use csp_render::flight::{FlightPacket, FlightRenderer, FlightVertex};

/// The measurement's render size — §5.4's bound is written at 1080p.
/// `W * 4` = 7680 is a multiple of wgpu's 256-byte copy-row alignment.
const W: u32 = 1920;
const H: u32 = 1080;
/// The flight scene's reverse-Z near plane (`csp-view`'s flight camera
/// builds `perspective_revz(FLIGHT_FOV_Y, aspect, 0.1)`): NDC depth is
/// `near / view_depth`, 1 at the near plane, 0 at infinity.
const NEAR_M: f64 = 0.1;
/// The shared colour target format — sRGB like the surface path.
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// What one probe run measured.
#[derive(Clone, Copy, Debug)]
pub struct DepthFight {
    /// Pixels whose rendered colour differs between the two submission
    /// orders — the depth-unresolved set (the §5.4 "depth-fighting
    /// artefacts" count).
    pub order_dependent_px: u32,
    /// Render size the count was taken at.
    pub width: u32,
    /// Render size the count was taken at.
    pub height: u32,
}

/// The probe's failure modes — the measurement is a diagnostic, never
/// fatal (§2.13's presentation-fault class).
#[derive(Debug)]
pub enum DepthProbeError {
    /// `map_async` / the readback channel / the buffer map failed.
    Readback(String),
}

impl std::fmt::Display for DepthProbeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Readback(e) => write!(f, "depth probe readback: {e}"),
        }
    }
}

impl std::error::Error for DepthProbeError {}

/// The reverse-Z NDC depth `extract` would emit for a point at view
/// depth `d`: f64 math, f32 cast — the same quantization the real
/// vertex path applies.
fn ndc_z(d: f64) -> f32 {
    (NEAR_M / d) as f32
}

/// One unlit-stream vertex (zero normal ⇒ fragment passthrough).
fn v(x: f32, y: f32, z: f32, color: [f32; 4]) -> FlightVertex {
    FlightVertex {
        pos: [x, y, z],
        world: [0.0; 3],
        nrm: [0.0; 3],
        color,
        mat: [0.0; 3],
    }
}

/// The probe scene's two quads as a vertex stream. `object_first`
/// swaps submission order — the artefact count is the pixel diff
/// between the two orders.
fn scene(scene_depth_m: f64, spread_m: f64, object_first: bool) -> Vec<FlightVertex> {
    // The surface: a flat quad at `scene_depth_m` covering the frame's
    // centre band.
    const X: f32 = 0.6;
    const Y: f32 = 0.5;
    let zs = ndc_z(scene_depth_m);
    let surface = [
        v(-X, -Y, zs, [0.05, 0.25, 0.90, 1.0]),
        v(X, -Y, zs, [0.05, 0.25, 0.90, 1.0]),
        v(X, Y, zs, [0.05, 0.25, 0.90, 1.0]),
        v(-X, -Y, zs, [0.05, 0.25, 0.90, 1.0]),
        v(X, Y, zs, [0.05, 0.25, 0.90, 1.0]),
        v(-X, Y, zs, [0.05, 0.25, 0.90, 1.0]),
    ];
    // The 1 m reference object: the same footprint, tilted in depth —
    // `spread_m` nearer than the surface at the left edge, that far
    // behind it at the right, crossing mid-frame. `spread_m = 1.0` is
    // the §5.4 scene's 1 m reference scale.
    let zl = ndc_z(scene_depth_m - spread_m);
    let zr = ndc_z(scene_depth_m + spread_m);
    let object = [
        v(-X, -Y, zl, [0.05, 0.90, 0.25, 1.0]),
        v(X, -Y, zr, [0.05, 0.90, 0.25, 1.0]),
        v(X, Y, zr, [0.05, 0.90, 0.25, 1.0]),
        v(-X, -Y, zl, [0.05, 0.90, 0.25, 1.0]),
        v(X, Y, zr, [0.05, 0.90, 0.25, 1.0]),
        v(-X, Y, zl, [0.05, 0.90, 0.25, 1.0]),
    ];
    let mut verts = Vec::with_capacity(12);
    if object_first {
        verts.extend_from_slice(&object);
        verts.extend_from_slice(&surface);
    } else {
        verts.extend_from_slice(&surface);
        verts.extend_from_slice(&object);
    }
    verts
}

/// Render `verts` through the flight pipeline the way the app's scene
/// pass does (4×MSAA colour into a resolved readback, shared reverse-Z
/// depth cleared to 0.0) and return the resolved pixels.
#[allow(clippy::too_many_lines)] // one render-pass recipe for the probe
fn render_probe_frame(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    verts: &[FlightVertex],
) -> Result<Vec<u8>, DepthProbeError> {
    let msaa_tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("depth-probe-msaa"),
        size: wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: csp_render::MSAA_SAMPLES,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let msaa = msaa_tex.create_view(&wgpu::TextureViewDescriptor::default());
    let color_tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("depth-probe-target"),
        size: wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let target = color_tex.create_view(&wgpu::TextureViewDescriptor::default());
    let mut flight = FlightRenderer::new(device, FORMAT, (W, H));
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("depth-probe"),
    });
    {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("depth-probe"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &msaa,
                resolve_target: Some(&target),
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: flight.depth_view(),
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
        flight.draw(
            &mut pass,
            device,
            queue,
            &FlightPacket {
                vertices: verts.to_vec(),
                ..Default::default()
            },
        );
    }
    let row_bytes = W as usize * 4;
    let buf = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("depth-probe-readback"),
        size: (row_bytes * H as usize) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: &color_tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buf,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row_bytes as u32),
                rows_per_image: Some(H),
            },
        },
        wgpu::Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([encoder.finish()]);
    let (tx, rx) = std::sync::mpsc::channel();
    buf.map_async(wgpu::MapMode::Read, .., move |r| {
        let _ = tx.send(r);
    });
    let mapped = device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|e| DepthProbeError::Readback(format!("device poll: {e:?}")))
        .and_then(|_| {
            rx.recv()
                .map_err(|e| DepthProbeError::Readback(format!("map channel: {e}")))
        })?;
    mapped.map_err(|e| DepthProbeError::Readback(format!("map_async: {e}")))?;
    let img = buf
        .get_mapped_range(..)
        .map_err(|e| DepthProbeError::Readback(format!("map range: {e}")))?
        .to_vec();
    buf.unmap();
    Ok(img)
}

/// The §5.4 measurement: the probe scene at `scene_depth_m` rendered
/// both submission orders; returns the count of pixels whose winner
/// changed with the order — the depth-fighting artefact count.
/// `spread_m` is the reference object's peak depth separation (the
/// §5.4 row uses `1.0` at `1e7`; a `0.0` spread is a fully coplanar
/// pair — every covered pixel order-dependent — the harness control).
pub fn depth_fight_pixels(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    scene_depth_m: f64,
    spread_m: f64,
) -> Result<DepthFight, DepthProbeError> {
    let a = render_probe_frame(device, queue, &scene(scene_depth_m, spread_m, false))?;
    let b = render_probe_frame(device, queue, &scene(scene_depth_m, spread_m, true))?;
    // A pixel is a depth-fight artefact when the two orders disagree
    // — where f32 depth resolves the pair, the winner is the same in
    // both. >8 tolerates resolve-path rounding on shared coverage
    // edges; the two colours differ by ≥ ~150.
    let order_dependent_px = a
        .chunks_exact(4)
        .zip(b.chunks_exact(4))
        .filter(|(pa, pb)| (0..3).any(|c| pa[c].abs_diff(pb[c]) > 8))
        .count() as u32;
    Ok(DepthFight {
        order_dependent_px,
        width: W,
        height: H,
    })
}
