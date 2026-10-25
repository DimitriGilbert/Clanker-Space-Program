//! M3 VAB craft view + GPU picking (PLAN M3 exit row "GPU picking",
//! §9 "Editor viewport is `csp-render` + GPU picking").
//!
//! Two pipelines over one vertex stream built by `csp-view` (clip-space
//! f32, §2.3 — this crate never sees the f64 craft frame):
//!
//! - **Color pass** — flat-shaded kitbash part meshes (§3.7's
//!   "silhouette-correct kitbashed parts" — the M4c floor that
//!   replaced the through-M3 placeholder boxes) drawn into the app's
//!   main pass. Depth-tested against a `Depth32Float` buffer this renderer
//!   owns and the app attaches to its render pass
//!   ([`VabRenderer::depth_view`]); reverse-Z convention like the map's
//!   projection (z: 1 = near, 0 = far, clear 0.0, `Greater` wins).
//! - **Pick pass** — the same vertices re-rasterized into an offscreen
//!   `R32Uint` texture (with its own depth), each part's fragment
//!   writing its 1-based index into [`VabPacket::pick_ids`]; 0 is
//!   background. [`VabRenderer::encode_pick`] copies the pixel under
//!   the cursor to a 4-byte readback buffer inside the frame's
//!   submission; [`VabRenderer::map_pick`] (call after submit) hands
//!   the result to a channel — selection lands via `csp-view`'s input
//!   events a frame or two later, inside §5.4's pointer→response
//!   budget.

use crate::GpuBuffer;

/// One vertex of a part mesh — clip-space NDC (z: 1 = near, 0 = far —
/// the map projection's reverse-Z convention) plus the per-pixel-
/// lighting channels shared with `FlightVertex` (camera-relative world
/// position, world normal, linear albedo, material terms; ~zero normal
/// marks the unlit stream), and the part's 1-based pick index
/// (0 = none/background).
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct VabVertex {
    /// Clip-space position (z is the depth value).
    pub pos: [f32; 3],
    /// Camera-relative world position (f32 — §2.3 renders relative to
    /// the eye, so the view direction is `-normalize(world)`).
    pub world: [f32; 3],
    /// World-space unit normal; ~0 → unlit passthrough.
    pub nrm: [f32; 3],
    /// Linear RGBA albedo (unlit color when `nrm` ≈ 0).
    pub color: [f32; 4],
    /// `[specular strength, specular exponent, occlusion]` — baked
    /// per-material terms the fragment lighting reads.
    pub mat: [f32; 3],
    /// 1-based index into [`VabPacket::pick_ids`].
    pub pick: u32,
}

impl VabVertex {
    const LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<VabVertex>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &wgpu::vertex_attr_array![
            0 => Float32x3,
            1 => Float32x3,
            2 => Float32x3,
            3 => Float32x4,
            4 => Float32x3,
            5 => Uint32
        ],
    };
}

/// The RenderExtract product for the VAB craft view — plain vertex
/// data, no route back to session state (§2.1/§2.5).
#[derive(Clone, Debug, Default)]
pub struct VabPacket {
    /// Triangle-list part-mesh faces for every placed part.
    pub vertices: Vec<VabVertex>,
    /// `pick_ids[i]` is the `PartNodeId` (as `u64`) drawn with pick
    /// index `i + 1` — the pick readback decodes through this table.
    /// `u64`, not `PartNodeId`, so `csp-app` needs no `csp-phys` edge.
    pub pick_ids: Vec<u64>,
    /// The `VabSession::craft_epoch` this packet's `pick_ids` table
    /// was generated from — the stale-pick stamp (P10-F3): a readback
    /// decoded through a snapshotted table is only meaningful while
    /// the session still reports the SAME epoch (the app's
    /// `View::with_vab_session(|v| v.craft_epoch())` read). A craft
    /// edit between encode and decode either dead-ends the id
    /// (collapses a parent-pick into "background click", disarming a
    /// pending placement) or, worse, reuses the id for a DIFFERENT
    /// node — an epoch check drops the stale decode instead.
    pub craft_epoch: u64,
    /// The frame's part lighting — the editor's fixed studio key +
    /// hemisphere fill, written to the uniform buffer each draw.
    pub light: crate::PartLight,
}

/// The offscreen targets the pick pass draws into.
struct PickTargets {
    /// `R32Uint` object-id texture — the fragment's pick index is the
    /// pixel value; 0 is background.
    color: wgpu::Texture,
    /// Its view.
    color_view: wgpu::TextureView,
    /// The pick pass's own depth (the color pass's depth texture is
    /// bound to the app-owned main pass, so this pass needs its own).
    depth_view: wgpu::TextureView,
}

/// Owns the VAB pipelines, the depth texture the app's main pass
/// attaches, and the offscreen object-id target + readback buffer.
pub struct VabRenderer {
    /// Part meshes → the surface (depth-tested).
    pipeline: wgpu::RenderPipeline,
    /// Object-id rasterization → `pick.color`.
    pick_pipeline: wgpu::RenderPipeline,
    verts: GpuBuffer,
    /// `PartLight` uniform — rewritten per draw from the packet.
    light_buf: wgpu::Buffer,
    /// group(0) on the color pipeline — the `light` uniform binding.
    light_bind: wgpu::BindGroup,
    /// Depth for the app's main render pass (reverse-Z convention) —
    /// texture + view kept so `depth_view` can return a reference.
    depth: wgpu::Texture,
    depth_view: wgpu::TextureView,
    /// Offscreen pick targets, rebuilt with the surface.
    pick: PickTargets,
    /// Single-pixel readback target (`MAP_READ | COPY_DST`).
    read_buf: wgpu::Buffer,
    /// Surface size the offscreen targets were built at.
    size: (u32, u32),
}

impl VabRenderer {
    /// Build both pipelines and the offscreen targets for `size`.
    #[allow(clippy::too_many_lines)] // two pipelines + targets is one init block
    pub fn new(
        device: &wgpu::Device,
        surface_format: wgpu::TextureFormat,
        size: (u32, u32),
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("vab"),
            source: wgpu::ShaderSource::Wgsl(include_str!("vab.wgsl").into()),
        });
        // Reverse-Z depth convention (the map projection's): 1 = near,
        // 0 = far; clears to 0.0 and `Greater` wins.
        let depth_state = wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth32Float,
            depth_write_enabled: Some(true),
            depth_compare: Some(wgpu::CompareFunction::Greater),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        };
        let vertex = wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[Some(VabVertex::LAYOUT)],
        };
        let primitive = wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            cull_mode: None, // kitbash meshes are not guaranteed closed
            ..Default::default()
        };
        let mk_pipeline = |label, samples: u32, fragment: wgpu::FragmentState<'_>| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: None,
                vertex: vertex.clone(),
                fragment: Some(fragment),
                primitive,
                depth_stencil: Some(depth_state.clone()),
                multisample: wgpu::MultisampleState {
                    count: samples,
                    ..wgpu::MultisampleState::default()
                },
                multiview_mask: None,
                cache: None,
            })
        };
        let pipeline = mk_pipeline(
            "vab",
            crate::MSAA_SAMPLES,
            wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            },
        );
        // The pick pass rasterizes into its own single-sampled
        // object-id target — MSAA there would be pure cost (and the
        // id values are not blendable anyway).
        let pick_pipeline = mk_pipeline(
            "vab-pick",
            1,
            wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_pick"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: wgpu::TextureFormat::R32Uint,
                    blend: None, // integer target
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            },
        );
        let depth = render_target(
            device,
            size,
            wgpu::TextureFormat::Depth32Float,
            crate::MSAA_SAMPLES,
            "vab-depth",
        );
        let depth_view = depth.create_view(&wgpu::TextureViewDescriptor::default());
        // The color pipeline's auto layout carries group(0) (`light`);
        // the pick pipeline shares the shader module but never
        // references the uniform, so its own layout stays empty and no
        // bind group is needed for it.
        let light_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vab-light"),
            size: std::mem::size_of::<crate::PartLight>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let light_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vab-light"),
            layout: &pipeline.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: light_buf.as_entire_binding(),
            }],
        });
        Self {
            pipeline,
            pick_pipeline,
            light_buf,
            light_bind,
            verts: GpuBuffer::new(
                device,
                "vab-verts",
                (std::mem::size_of::<VabVertex>() * 8192) as u64,
            ),
            depth,
            depth_view,
            pick: pick_targets(device, size),
            read_buf: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("vab-pick-read"),
                size: 4,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            size: (size.0.max(1), size.1.max(1)),
        }
    }

    /// Recreate the surface-sized targets after a resize.
    pub fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        let size = (width.max(1), height.max(1));
        if size != self.size {
            self.depth = render_target(
                device,
                size,
                wgpu::TextureFormat::Depth32Float,
                crate::MSAA_SAMPLES,
                "vab-depth",
            );
            self.depth_view = self
                .depth
                .create_view(&wgpu::TextureViewDescriptor::default());
            self.pick = pick_targets(device, size);
            self.size = size;
        }
    }

    /// The depth view the app's main render pass should attach —
    /// reverse-Z: clear to `0.0`, `Greater` wins.
    pub fn depth_view(&self) -> &wgpu::TextureView {
        &self.depth_view
    }

    /// Upload the packet's vertices and draw the part pass into `pass`.
    pub fn draw(
        &mut self,
        pass: &mut wgpu::RenderPass<'_>,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        packet: &VabPacket,
    ) {
        let n = self.verts.upload(device, queue, &packet.vertices);
        if n == 0 {
            return;
        }
        queue.write_buffer(&self.light_buf, 0, bytemuck::bytes_of(&packet.light));
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.light_bind, &[]);
        pass.set_vertex_buffer(0, self.verts.buffer.slice(..));
        pass.draw(0..n, 0..1);
    }

    /// Record the object-id pass into `encoder` (offscreen, so it may
    /// sit beside the main pass in the same submission) plus the
    /// single-pixel copy at (`x_px`, `y_px`) — physical surface pixels.
    /// Returns `false` (request dropped) when the pixel is outside the
    /// pick target or the packet is empty; `map_pick` should only run
    /// when this returns `true`.
    pub fn encode_pick(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        packet: &VabPacket,
        x_px: u32,
        y_px: u32,
    ) -> bool {
        if x_px >= self.size.0 || y_px >= self.size.1 || packet.vertices.is_empty() {
            return false;
        }
        // Re-upload is cheap and keeps the pick pass independent of
        // whether `draw` ran this frame; `write_buffer` orders before
        // this submission's commands either way.
        let n = self.verts.upload(device, queue, &packet.vertices);
        if n == 0 {
            return false;
        }
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("vab-pick"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.pick.color_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        // Integer target — TRANSPARENT clears to u32 0,
                        // the background id.
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.pick.depth_view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(0.0), // reverse-Z: 0 = far
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pick_pipeline);
            pass.set_vertex_buffer(0, self.verts.buffer.slice(..));
            pass.draw(0..n, 0..1);
        }
        self.pick.copy_pixel(encoder, &self.read_buf, x_px, y_px);
        true
    }

    /// After the submission containing [`Self::encode_pick`]'s commands:
    /// map the readback buffer; on completion the pick index (0 =
    /// background) is sent on `tx`. The callback fires on
    /// `device.poll(PollType::Poll)` — the app polls once per frame —
    /// and on wasm on the event loop.
    pub fn map_pick(&self, tx: std::sync::mpsc::Sender<u32>) {
        let buf = self.read_buf.clone();
        self.read_buf
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |res| {
                let v = if res.is_ok() {
                    let v = buf
                        .slice(..)
                        .get_mapped_range()
                        .ok()
                        .map(|view| u32::from_le_bytes([view[0], view[1], view[2], view[3]]));
                    buf.unmap();
                    v.unwrap_or(0)
                } else {
                    0
                };
                let _ = tx.send(v);
            });
    }
}

impl PickTargets {
    /// Copy the single pixel at (`x`, `y`) of `color` into `buf`.
    fn copy_pixel(&self, encoder: &mut wgpu::CommandEncoder, buf: &wgpu::Buffer, x: u32, y: u32) {
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.color,
                mip_level: 0,
                origin: wgpu::Origin3d { x, y, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: None,
                    rows_per_image: None,
                },
            },
            wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
    }
}

/// A surface-sized render-target texture at `samples` multisamples.
fn render_target(
    device: &wgpu::Device,
    size: (u32, u32),
    format: wgpu::TextureFormat,
    samples: u32,
    label: &'static str,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: size.0.max(1),
            height: size.1.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: samples,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

/// Fresh pick targets at `size` — single-sampled (object ids don't
/// multisample).
fn pick_targets(device: &wgpu::Device, size: (u32, u32)) -> PickTargets {
    let color = render_target(device, size, wgpu::TextureFormat::R32Uint, 1, "vab-pick");
    let depth = render_target(
        device,
        size,
        wgpu::TextureFormat::Depth32Float,
        1,
        "vab-pick-depth",
    );
    PickTargets {
        color_view: color.create_view(&wgpu::TextureViewDescriptor::default()),
        color,
        depth_view: depth.create_view(&wgpu::TextureViewDescriptor::default()),
    }
}
