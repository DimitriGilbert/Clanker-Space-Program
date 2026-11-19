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
//! §2.3's far-field pass (what the row now certifies): since the
//! two-pass depth partition landed, the shipped pipeline routes the
//! scene's two surfaces the way it routes any flight frame — the
//! SURFACE is terrain-class geometry at 10⁷ m, drawn by the far pass
//! with `z = FAR_SPLIT/d` (0.1 at 10⁷ m), and the reference OBJECT is
//! craft-class geometry, drawn by the near pass (`z = near/d`, near
//! 0.1) — the far pass records first with its own cleared depth and
//! the near pass clears again and draws on top, so no submission
//! order can decide a winner across the partition. The probe mirrors
//! that composition exactly (see `render_probe_frame`) and the
//! measurement certifies it: **zero** order-dependent pixels for the
//! §5.4 scene. The within-pass information floor this rides on, for
//! the record: an f32 depth value carries 2²⁴ steps total, so a
//! frame-width 1 m swing at 10⁷ m needs ~2³²·⁴ steps to resolve at
//! every pixel — impossible in ANY single f32 depth mapping (the
//! resolvable difference at range d is ~d·2⁻²⁴ regardless of the
//! projection's near; see `csp_render::flight::FAR_SPLIT_M`). The
//! partition is the §2.3 answer: determinism by pass layering, with
//! each pass's own precision profile. The controls keep the differ
//! honest — at 1 km both quads route NEAR (below the split), the
//! classic one-stream order swap must resolve to zero, and a coplanar
//! pair must count ~the whole quad.
//!
//! Native-only: it rasterizes on a caller-supplied headless device
//! (`csp-perf`'s `render.depth-fight-1e7` row and the perf test both
//! drive it; the row is recorded red or green by what this returns —
//! over the §5.4 "1 pixel at 1080p" bound, §2.3's second far-field
//! pass is owed).

use csp_render::flight::{FAR_SPLIT_M, FlightPacket, FlightRenderer, FlightVertex};

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
/// depth `d` under a projection with near `n`: f64 math, f32 cast —
/// the same quantization the real vertex path applies. `n` is
/// `NEAR_M` for near-pass geometry, [`FAR_SPLIT_M`] for far-pass.
fn ndc_z(d: f64, n: f64) -> f32 {
    (n / d) as f32
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

/// The probe scene's two quads, split into the far/near streams the
/// shipped pipeline routes them through (§2.3): the SURFACE is
/// terrain-class — beyond [`FAR_SPLIT_M`] it draws in the far pass
/// (`z = FAR_SPLIT/d`), below it in the near pass with the craft; the
/// OBJECT is the craft-class "reference object" — always the near
/// stream (a craft at 10⁷ m is the map regime; the near pass draws it
/// over far geometry unconditionally, the documented stable
/// approximation). `object_first` swaps the two quads' order inside
/// each stream — the classic single-stream swap wherever both land
/// together (the 1 km controls), and the composition's fixed pass
/// order wherever they do not (the 10⁷ m scene: order can no longer
/// decide, which is precisely what the row measures).
fn scene(
    scene_depth_m: f64,
    spread_m: f64,
    object_first: bool,
) -> (Vec<FlightVertex>, Vec<FlightVertex>) {
    // The surface: a flat quad at `scene_depth_m` covering the frame's
    // centre band.
    const X: f32 = 0.6;
    const Y: f32 = 0.5;
    let surface_far = scene_depth_m >= FAR_SPLIT_M;
    let n_surf = if surface_far { FAR_SPLIT_M } else { NEAR_M };
    let zs = ndc_z(scene_depth_m, n_surf);
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
    let zl = ndc_z(scene_depth_m - spread_m, NEAR_M);
    let zr = ndc_z(scene_depth_m + spread_m, NEAR_M);
    let object = [
        v(-X, -Y, zl, [0.05, 0.90, 0.25, 1.0]),
        v(X, -Y, zr, [0.05, 0.90, 0.25, 1.0]),
        v(X, Y, zr, [0.05, 0.90, 0.25, 1.0]),
        v(-X, -Y, zl, [0.05, 0.90, 0.25, 1.0]),
        v(X, Y, zr, [0.05, 0.90, 0.25, 1.0]),
        v(-X, Y, zl, [0.05, 0.90, 0.25, 1.0]),
    ];
    let mut far = Vec::new();
    let mut near = Vec::new();
    if surface_far {
        // The §5.4 composition: surface in the far pass, object in
        // the near pass — the pass structure owns the order, so
        // `object_first` has no swap to make (that IS the fix).
        far.extend_from_slice(&surface);
        near.extend_from_slice(&object);
    } else {
        // Both quads in one stream (the harness controls): the
        // classic single-stream submission swap.
        if object_first {
            near.extend_from_slice(&object);
            near.extend_from_slice(&surface);
        } else {
            near.extend_from_slice(&surface);
            near.extend_from_slice(&object);
        }
    }
    (far, near)
}

/// Render the scene's two streams through the flight pipeline the way
/// the app's §2.3 frame does: a FAR pass (color clear, shared reverse-Z
/// depth cleared 0.0, no resolve) drawing `far`, then the NEAR pass
/// (color load, depth cleared AGAIN, resolve into the readback)
/// drawing `near` — 4×MSAA throughout — and return the resolved
/// pixels.
#[allow(clippy::too_many_lines)] // one render-pass recipe for the probe
fn render_probe_frame(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    far: &[FlightVertex],
    near: &[FlightVertex],
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
    // Terrain first — the flight renderer borrows the cascade
    // resources (R1/3b) from it, mirroring the app's construction.
    // The probe's packets are disarmed (`shadow` defaults to the
    // zeroed row), so the cascade path stays dead in these frames.
    let terrain = csp_render::terrain::TerrainRenderer::new(device, FORMAT);
    let mut flight = FlightRenderer::new(device, FORMAT, (W, H), &terrain);
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("depth-probe"),
    });
    {
        // §2.3 far pass: own cleared depth, far stream only.
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("depth-probe-far"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &msaa,
                resolve_target: None,
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
                    // The near pass below clears again before reading —
                    // the far depths are write-only (store discarded,
                    // mirroring the app's far pass).
                    store: wgpu::StoreOp::Discard,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        flight.draw_far(
            &mut pass,
            device,
            queue,
            &FlightPacket {
                far_vertices: far.to_vec(),
                ..Default::default()
            },
        );
    }
    {
        // §2.3 near pass: depth cleared again (the partition — near
        // never tests against far depths), color loaded, resolved.
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("depth-probe"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &msaa,
                resolve_target: Some(&target),
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Load,
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
                vertices: near.to_vec(),
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

/// The far-CONTENT control (the row's fourth guard): the order diff
/// above reads 0 both when the composition is order-independent AND
/// when the far pass stopped drawing anything (both orders render the
/// same empty background) — and the §5.4 quads share one screen
/// footprint by design, so the composed frame has no
/// outside-the-object band to sample for surviving far pixels. This
/// control closes that hole directly: it renders the far surface
/// ALONE through the far pass (`scene`'s surface quad, no near
/// stream) and returns how many pixels actually rasterized — a
/// healthy run is the full centre band (~6.2·10⁵ px at 1920×1080);
/// near-zero means the far pass lost its content and the row's 0 px
/// is vacuous. Only meaningful at depths ≥ `FAR_SPLIT_M` (below it
/// the surface routes near and the far stream is empty by design).
pub fn far_surface_rasterizes(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    scene_depth_m: f64,
) -> Result<u32, DepthProbeError> {
    let (far, _near) = scene(scene_depth_m, 1.0, false);
    let img = render_probe_frame(device, queue, &far, &[])?;
    Ok(img
        .chunks_exact(4)
        .filter(|p| (0..3).any(|c| p[c] > 8))
        .count() as u32)
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
    let (fa, na) = scene(scene_depth_m, spread_m, false);
    let (fb, nb) = scene(scene_depth_m, spread_m, true);
    let a = render_probe_frame(device, queue, &fa, &na)?;
    let b = render_probe_frame(device, queue, &fb, &nb)?;
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
