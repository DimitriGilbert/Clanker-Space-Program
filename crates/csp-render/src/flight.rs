//! M4c near-flight view (PLAN §3.7's presentation floor): the actual
//! InBubble craft as hull boxes, a ground disc and horizon band, an
//! engine plume — the flat-colored triangle stream `csp-view`'s
//! `extract_flight` builds (clip-space f32, §2.3; this crate never
//! sees the f64 bubble frame). Same reverse-Z depth convention as
//! the map and VAB renderers.

use crate::GpuBuffer;

/// One vertex of the flight scene — clip-space NDC (z: 1 = near,
/// 0 = far) + flat RGBA. Identical layout to `VabVertex` minus the
/// pick channel (flight has no picking at M4c).
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct FlightVertex {
    /// Clip-space position (z is the depth value).
    pub pos: [f32; 3],
    /// Linear RGBA.
    pub color: [f32; 4],
}

impl FlightVertex {
    const LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<FlightVertex>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x4],
    };
}

/// The RenderExtract product — plain vertex data, no route back to
/// sim state (§2.1/§2.5).
#[derive(Clone, Debug, Default)]
pub struct FlightPacket {
    /// Triangle-list scene: fallback sky gradient, craft boxes, plumes.
    pub vertices: Vec<FlightVertex>,
    /// The R2 surface pass (§5.2): quadtree terrain + ocean instances
    /// and the LUT-scattered sky, drawn by
    /// [`crate::terrain::TerrainRenderer`] *before* the vertex stream
    /// (the sky is the backdrop; terrain is opaque over it).
    /// `None` without a physics environment or a solved camera — the
    /// gradient in `vertices` is the retained fallback.
    pub terrain: Option<crate::terrain::TerrainPacket>,
}

/// GPU owner for the flight pass — one pipeline + one depth target.
pub struct FlightRenderer {
    pipeline: wgpu::RenderPipeline,
    verts: GpuBuffer,
    depth: wgpu::Texture,
    depth_view: wgpu::TextureView,
    size: (u32, u32),
}

impl FlightRenderer {
    /// Build the pipeline/depth target; `size` is the initial surface.
    pub fn new(
        device: &wgpu::Device,
        surface_format: wgpu::TextureFormat,
        size: (u32, u32),
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("flight"),
            source: wgpu::ShaderSource::Wgsl(include_str!("flight.wgsl").into()),
        });
        let depth_state = wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth32Float,
            depth_write_enabled: Some(true),
            depth_compare: Some(wgpu::CompareFunction::Greater),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        };
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("flight"),
            layout: None,
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[Some(FlightVertex::LAYOUT)],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(depth_state),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let size = (size.0.max(1), size.1.max(1));
        let depth = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("flight-depth"),
            size: wgpu::Extent3d {
                width: size.0,
                height: size.1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let depth_view = depth.create_view(&wgpu::TextureViewDescriptor::default());
        Self {
            pipeline,
            verts: GpuBuffer::new(
                device,
                "flight-verts",
                (std::mem::size_of::<FlightVertex>() * 16384) as u64,
            ),
            depth,
            depth_view,
            size,
        }
    }

    /// Recreate the depth target after a resize.
    pub fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        let size = (width.max(1), height.max(1));
        if size == self.size {
            return;
        }
        self.depth = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("flight-depth"),
            size: wgpu::Extent3d {
                width: size.0,
                height: size.1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Depth32Float,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        self.depth_view = self
            .depth
            .create_view(&wgpu::TextureViewDescriptor::default());
        self.size = size;
    }

    /// The depth view the app's main pass should attach — the shared
    /// surface-sized depth buffer (reverse-Z, clear 0.0).
    pub fn depth_view(&self) -> &wgpu::TextureView {
        &self.depth_view
    }

    /// Upload the packet and draw.
    pub fn draw(
        &mut self,
        pass: &mut wgpu::RenderPass<'_>,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        packet: &FlightPacket,
    ) {
        let n = self.verts.upload(device, queue, &packet.vertices);
        if n == 0 {
            return;
        }
        pass.set_pipeline(&self.pipeline);
        pass.set_vertex_buffer(0, self.verts.buffer.slice(..));
        pass.draw(0..n, 0..1);
    }
}
