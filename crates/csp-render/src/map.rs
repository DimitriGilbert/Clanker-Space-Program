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
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct MapVertex {
    /// Clip-space position (z is the depth value).
    pub pos: [f32; 3],
    /// Linear RGBA.
    pub color: [f32; 4],
}

impl MapVertex {
    /// The vertex-buffer layout for [`MapRenderer`]'s pipeline.
    pub(crate) const LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<MapVertex>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x4],
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
    /// Orbit polylines (one shared mesh; per-segment colors baked in).
    pub lines: MapMesh,
    /// Screen-space markers (bodies + vessels), quads in clip space.
    pub markers: MapMesh,
}

/// Owns the map pipeline and the two per-frame vertex buffers.
///
/// Constructed once at GPU init; [`Self::draw`] uploads the packet's
/// vertices and records two draws into an open render pass (the same
/// pass egui paints into — map first, UI on top).
pub struct MapRenderer {
    pipeline: wgpu::RenderPipeline,
    lines: GpuBuffer,
    markers: GpuBuffer,
}

/// A grow-only vertex buffer + the count of vertices currently in it.
struct GpuBuffer {
    buffer: wgpu::Buffer,
    capacity: u64,
}

impl GpuBuffer {
    fn new(device: &wgpu::Device, label: &'static str, capacity: u64) -> Self {
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

    /// Upload `mesh`'s vertices, growing the buffer when needed. Returns
    /// the vertex count to draw (0 ⇒ skip the draw call).
    fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, mesh: &MapMesh) -> u32 {
        let bytes = bytemuck::cast_slice(&mesh.vertices);
        let needed = bytes.len() as u64;
        if needed > self.capacity {
            // Grow with headroom — resampling per frame would reallocate
            // every frame at the exact-size policy.
            *self = Self::new(device, "map-vtx", needed.next_power_of_two());
        }
        queue.write_buffer(&self.buffer, 0, bytes);
        mesh.vertices.len() as u32
    }
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
            lines: GpuBuffer::new(device, "map-lines", initial),
            markers: GpuBuffer::new(device, "map-markers", initial),
        }
    }

    /// Upload `packet` and record its draws into `pass`. Cheap path
    /// (§5.3's map row): two buffer writes + two draws per frame.
    pub fn draw(
        &mut self,
        pass: &mut wgpu::RenderPass<'_>,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        packet: &MapPacket,
    ) {
        let n_lines = self.lines.upload(device, queue, &packet.lines);
        let n_markers = self.markers.upload(device, queue, &packet.markers);
        pass.set_pipeline(&self.pipeline);
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
