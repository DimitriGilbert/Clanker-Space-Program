//! M1 map-view rendering (PLAN §5.1 R0): an unlit colored-vertex pipeline
//! for orbit polylines and screen-space markers.
//!
//! This module is deliberately dumb about the world: it consumes a
//! [`MapPacket`] of plain vertex data built by `csp-view` — f32 positions
//! already projected to clip space relative to the map camera (§2.3's
//! camera-relative rule: the f64 subtraction happens CPU-side, the GPU
//! never sees world coordinates). `csp-render` has no `csp-sim` edge
//! (§2.1) — everything sim-derived is baked into the packet upstream.

/// One colored vertex — position is clip-space NDC (wgpu: x/y in
/// [-1,1] with y up, z in [0,1] with 1 = near under reverse-Z),
/// `color` is linear RGBA.
///
/// `uv` + `kind` drive the fragment-side shaping: `kind = 0` is a
/// solid quad (orbit-line segments — `uv` ignored), `1` a soft-edged
/// disc, `2` a thin ring, `3` a radial glow — evaluated against the
/// quad-interpolated `uv` in `[-1,1]` (0 = quad center).
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct MapVertex {
    /// Clip-space position (z is the depth value).
    pub pos: [f32; 3],
    /// Linear RGBA.
    pub color: [f32; 4],
    /// Quad-local coordinate, `[-1,1]` across the quad for shaped
    /// markers; ignored for `kind = 0` line segments.
    pub uv: [f32; 2],
    /// Shape code: 0 solid, 1 soft disc, 2 ring, 3 radial glow.
    pub kind: f32,
}

impl MapVertex {
    /// Shape code for unshaded quads (orbit-line segments).
    pub const KIND_SOLID: f32 = 0.0;
    /// Shape code for a soft-edged filled disc.
    pub const KIND_DISC: f32 = 1.0;
    /// Shape code for a thin ring outline.
    pub const KIND_RING: f32 = 2.0;
    /// Shape code for a radial-falloff glow.
    pub const KIND_GLOW: f32 = 3.0;

    /// The vertex-buffer layout for [`MapRenderer`]'s pipeline.
    pub(crate) const LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<MapVertex>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x4, 2 => Float32x2, 3 => Float32],
    };
}

/// One draw call's worth of map geometry.
#[derive(Clone, Debug, Default)]
pub struct MapMesh {
    /// Triangle-list vertices (`lines` are emitted as camera-facing quad
    /// strips by the extractor — line primitives have no width control).
    pub vertices: Vec<MapVertex>,
}

/// The immutable render packet `csp-view`'s RenderExtract stage produces
/// for one frame — the §2.5 contract: everything the map pass draws,
/// pre-projected, with no route back to sim state.
#[derive(Clone, Debug, Default)]
pub struct MapPacket {
    /// The backdrop — the world-anchored starfield, drawn first so the
    /// alpha-blended scene layers on top of it.
    pub bg: MapMesh,
    /// Orbit polylines (one shared mesh; per-segment colors baked in).
    pub lines: MapMesh,
    /// Screen-space markers (bodies + vessels), quads in clip space.
    pub markers: MapMesh,
}

use crate::GpuBuffer;

/// Owns the map pipeline and the three per-frame vertex buffers.
///
/// Constructed once at GPU init; [`Self::draw`] uploads the packet's
/// vertices and records three draws into an open render pass (the same
/// pass egui paints into — map first, UI on top).
pub struct MapRenderer {
    pipeline: wgpu::RenderPipeline,
    bg: GpuBuffer,
    lines: GpuBuffer,
    markers: GpuBuffer,
}

impl MapRenderer {
    /// Build the unlit pipeline against the surface format.
    pub fn new(device: &wgpu::Device, surface_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("map"),
            source: wgpu::ShaderSource::Wgsl(include_str!("map.wgsl").into()),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("map"),
            layout: None, // no bind groups — pure passthrough
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[Some(MapVertex::LAYOUT)],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
                    // Alpha-blended lines/markers over the clear color.
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let initial = (std::mem::size_of::<MapVertex>() * 4096) as u64;
        Self {
            pipeline,
            bg: GpuBuffer::new(device, "map-bg", initial),
            lines: GpuBuffer::new(device, "map-lines", initial),
            markers: GpuBuffer::new(device, "map-markers", initial),
        }
    }

    /// Upload `packet` and record its draws into `pass`. Cheap path
    /// (§5.3's map row): three buffer writes + three draws per frame —
    /// backdrop first, then orbit lines, then markers on top.
    pub fn draw(
        &mut self,
        pass: &mut wgpu::RenderPass<'_>,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        packet: &MapPacket,
    ) {
        let n_bg = self.bg.upload(device, queue, &packet.bg.vertices);
        let n_lines = self.lines.upload(device, queue, &packet.lines.vertices);
        let n_markers = self.markers.upload(device, queue, &packet.markers.vertices);
        pass.set_pipeline(&self.pipeline);
        if n_bg > 0 {
            pass.set_vertex_buffer(0, self.bg.buffer.slice(..));
            pass.draw(0..n_bg, 0..1);
        }
        if n_lines > 0 {
            pass.set_vertex_buffer(0, self.lines.buffer.slice(..));
            pass.draw(0..n_lines, 0..1);
        }
        if n_markers > 0 {
            pass.set_vertex_buffer(0, self.markers.buffer.slice(..));
            pass.draw(0..n_markers, 0..1);
        }
    }
}
