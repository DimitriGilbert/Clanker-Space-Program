//! M4c near-flight view (PLAN §3.7's presentation floor): the actual
//! InBubble craft as kitbash part meshes, the terrain/sky surface
//! pass, engine plumes — the flat-colored triangle stream `csp-view`'s
//! `extract_flight` builds (clip-space f32, §2.3; this crate never
//! sees the f64 bubble frame). Same reverse-Z depth convention as
//! the map and VAB renderers — with one adjustment: this pipeline
//! compares `GreaterEqual`, not `Greater`, because the fallback sky
//! sits ON the far plane (NDC z = 0) and geometry exactly at the far
//! plane can never pass `Greater` against a 0.0-cleared depth buffer.
//! (A strict `Greater` left an empty flight scene wholly undrawn —
//! the uniform-clear-color bug the headless render gate now pins.)

use crate::GpuBuffer;

/// PLAN §2.3's far-field split (the debt the `render.depth-fight-1e7`
/// row calls in): the scene's single reverse-Z frustum (near 0.1,
/// infinite far) partitions at this view distance into a near pass
/// (today's projection, unchanged) and a far pass whose reversed-Z
/// projection takes the split as its near — `ndc.z = split/d`, so
/// 10⁷ m sits at z = 0.1 in the far pass's high-precision band
/// instead of z = 10⁻⁸ at the bottom of the near pass's last binades.
///
/// **Algebra** (`csp_view::map::perspective_revz`): the projection is
/// `[[f/a,0,0,0],[0,f,0,0],[0,0,0,n],[0,0,-1,0]]`; positions are
/// camera-relative so `V`'s bottom row is `[0,0,0,1]`, which makes
/// `(P·V)`'s clip.z row exactly `[0,0,0,n]` — clip.z is the constant
/// `n` and `clip.w = d` (the FORWARD view depth; off-axis it is
/// `radial·cos(angle)`, shorter than the radial distance), so NDC
/// z = n/d. The far frustum's row is the same row with `n = split`:
/// one row of the matrix, no other term changes (xy columns and w row
/// are bit-equal, so far geometry lands on the same screen pixels
/// with only the depth remapped). The clip test `0 ≤ clip.z ≤ clip.w`
/// becomes `split ≤ forward_depth` — a STALENESS SAFETY NET, never a
/// coverage mechanism: routing (see `patch_extent_m`) sends a patch
/// far only when its forward-nearest point clears the split, so no
/// legitimately-routed far quad reaches nearer than the split along
/// the view axis and the clip never trims visible surface.
///
/// **Precision arithmetic** (f32 reverse-Z): the depth value at range
/// `d` is `z = n/d` and its ulp is `~z·2⁻²³`, so the resolvable depth
/// difference is `ulp·d²/n ≈ d·2⁻²³` — INDEPENDENT of `n` (an f32
/// significand carries 2²⁴ steps total; at 10⁷ m that floors
/// separation at ~0.89 m per ulp in the near pass (z = 10⁻⁸, binade
/// [2⁻²⁷, 2⁻²⁶)) and ~0.75 m in the far pass (z = 0.1, binade
/// [2⁻⁴, 2⁻³)) — ~1.1 vs ~1.3 ulp for the §5.4 scene's 1 m). The
/// split therefore buys its precision two other ways: (a) it moves
/// the far band's z values from the 10⁻⁸ binade floor
/// (z/ulp ≈ 1.1·10⁷) up to 0.1 (z/ulp ≈ 1.3·10⁷, the best-placed
/// binade below 1), and (b) — the load-bearing half — the pass
/// partition resolves cross-pass ordering structurally: routed-far
/// quads lie wholly beyond the split along the view axis
/// (forward-nearest routing), the far-first/then-near-own-depth
/// composition (see `csp-app`'s frame loop) is therefore correct
/// across the partition — the one exception is a near-routed
/// straddler's beyond-split fringe, which rides the near pass's
/// unconditional-over policy (documented at the routing site) — and
/// submission order can never decide a winner across the partition.
/// Within the far pass, 1 m at 10⁷ m remains below the
/// 1-frame-width budget of a 24-bit depth (it would need ~2³²·⁴
/// steps) — no f32 mapping resolves that; the §2.3 prescription is
/// the partition, not a magically finer float.
pub const FAR_SPLIT_M: f64 = 1.0e6;

/// One vertex of the flight scene — clip-space NDC (z: 1 = near,
/// 0 = far) plus the per-pixel-lighting channels: camera-relative
/// world position (the eye sits at the origin), world normal, linear
/// albedo, and the material terms `[spec strength, spec exponent,
/// occlusion]`. A ~zero `nrm` marks the unlit stream (sky, plumes,
/// dust) — the fragment shader passes `color` straight through.
/// Identical layout to `VabVertex` minus the pick channel (flight has
/// no picking at M4c).
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
pub struct FlightVertex {
    /// Clip-space position (z is the depth value).
    pub pos: [f32; 3],
    /// Camera-relative world position (f32 — §2.3 already renders
    /// relative to the eye).
    pub world: [f32; 3],
    /// World-space unit normal; ~0 → unlit passthrough.
    pub nrm: [f32; 3],
    /// Linear RGBA albedo (unlit color when `nrm` ≈ 0).
    pub color: [f32; 4],
    /// `[specular strength, specular exponent, occlusion]` — baked
    /// per-material terms the fragment lighting reads.
    pub mat: [f32; 3],
}

impl FlightVertex {
    pub(crate) const LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<FlightVertex>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &wgpu::vertex_attr_array![
            0 => Float32x3,
            1 => Float32x3,
            2 => Float32x3,
            3 => Float32x4,
            4 => Float32x3
        ],
    };
}

/// The RenderExtract product — plain vertex data, no route back to
/// sim state (§2.1/§2.5).
#[derive(Clone, Debug, Default)]
pub struct FlightPacket {
    /// Triangle-list scene: craft part meshes, plumes — the NEAR
    /// stream (§2.3's first pass, projection near 0.1).
    pub vertices: Vec<FlightVertex>,
    /// §2.3's far-field stream: geometry whose vertices already carry
    /// the far pass's depth (`z = FAR_SPLIT/d`, baked f64→f32 at
    /// extract — the pipeline itself is shared, `vs_main` is a
    /// passthrough so no shader variant exists). Drawn by
    /// [`FlightRenderer::draw_far`] in the frame's FIRST pass (own
    /// cleared depth), before the near stream's pass. Today the
    /// extract routes only the fallback sky gradient here; the §5.4
    /// depth probe drives it with a far surface. Craft geometry
    /// deliberately NEVER lands here (a craft beyond the split is the
    /// map regime; the near pass draws it and wins over far terrain —
    /// the documented stable approximation, no flicker).
    pub far_vertices: Vec<FlightVertex>,
    /// The frame's part lighting — the staged sun + the hemisphere
    /// ambient the extract solved for the focus vessel's local sky.
    pub light: crate::PartLight,
    /// The R2 surface pass (§5.2): quadtree terrain + ocean instances
    /// and the LUT-scattered sky, drawn by
    /// [`crate::terrain::TerrainRenderer`] *before* the vertex stream
    /// (the sky is the backdrop; terrain is opaque over it).
    /// `None` without a physics environment or a solved camera — the
    /// gradient in `vertices` is the retained fallback.
    pub terrain: Option<crate::terrain::TerrainPacket>,
    /// The R1 cascade uniforms (terrain-overhaul 3b): the view's
    /// flight extract solves [`crate::shadow::solve_cascades`] for
    /// the chase camera + staged sun and hands the product here —
    /// `Default` (all-zero) is DISARMED, keeping both receivers
    /// (`fs_terrain`, this pass's `fs_main`) on their pre-cascade
    /// shading. The app feeds this row to
    /// [`crate::terrain::TerrainRenderer::draw_shadow`] before the
    /// scene pass; the cast reads the same matrices through the
    /// terrain group(0)'s binding 8.
    pub shadow: crate::shadow::ShadowUniforms,
}

/// GPU owner for the flight pass — one pipeline + one depth target +
/// the lighting uniform the lit vertex stream shades against. Since
/// 3b the pipeline also carries the cascade bindings: group(0) holds
/// the light (binding 0) beside the `SU` uniform (binding 8 — the
/// same slot the terrain group(0) gives it, so one WGSL declaration
/// serves both this pipeline and the craft-cast twins), and group(1)
/// is the terrain renderer's receiving set (depth array + comparison
/// sampler — the identical bind group object `fs_terrain` samples).
pub struct FlightRenderer {
    pipeline: wgpu::RenderPipeline,
    verts: GpuBuffer,
    /// §2.3's far-field stream buffer — same pipeline, separate
    /// upload so the near list's staging (and the cascade cast's
    /// `stage_cascade_cast` slice) never interleaves far geometry.
    far_verts: GpuBuffer,
    /// `PartLight` uniform — rewritten per draw from the packet.
    light_buf: wgpu::Buffer,
    /// group(0) — the `light` uniform (binding 0) + the `SU` cascade
    /// uniform (binding 8, the terrain renderer's shadow buffer).
    light_bind: wgpu::BindGroup,
    /// The cascade receiving set (group 1) — `ShadowCore`'s depth
    /// array + comparison sampler, shared with the terrain pass.
    shadow_bind: wgpu::BindGroup,
    depth: wgpu::Texture,
    depth_view: wgpu::TextureView,
    size: (u32, u32),
    /// G3+ cast-staging memo: the `(address, len)` of the vertex
    /// slice [`Self::stage_cascade_cast`] already uploaded THIS frame
    /// — the visible [`Self::draw`] runs on the same immutable packet
    /// a moment later and skips its identical ~9 MB re-staging write
    /// when the identity matches. `draw` consumes and clears it, so
    /// the memo never crosses a frame boundary; a `draw` whose slice
    /// does not match (a caller violating the stage→draw pairing)
    /// re-uploads anyway. The address rides as `usize` to keep the
    /// renderer `Send`.
    staged_verts: Option<(usize, usize)>,
}

impl FlightRenderer {
    /// Build the pipeline/depth target; `size` is the SCENE size (P1:
    /// the render-scaled attachment size the app's main pass binds —
    /// pass `Gpu::scene_size()`, which equals the surface at scale
    /// 1.0). `cascades` is the terrain renderer this flight pass
    /// shares a frame with — 3b's hand-off (its `SU` buffer + receiving
    /// group(1) are borrowed by handle, so construct the terrain
    /// renderer first; the app and the render gate both do).
    #[allow(clippy::too_many_lines)] // one device-resource construction; splitting fragments the descriptor set
    pub fn new(
        device: &wgpu::Device,
        surface_format: wgpu::TextureFormat,
        size: (u32, u32),
        cascades: &crate::terrain::TerrainRenderer,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("flight"),
            source: wgpu::ShaderSource::Wgsl(include_str!("flight.wgsl").into()),
        });
        let (su_buf, sample_bgl, sample_bind) = cascades.cascade_resources();
        let light_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("flight-light"),
            size: std::mem::size_of::<crate::PartLight>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let g0 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("flight"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                // Binding 8 mirrors the terrain group(0)'s `SU` slot so
                // flight.wgsl's single `su` declaration serves both the
                // visible pipeline (this layout) and the cast pipelines
                // (the terrain layout, via `ShadowCore`).
                wgpu::BindGroupLayoutEntry {
                    binding: 8,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let light_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("flight-light"),
            layout: &g0,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: light_buf.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 8,
                    resource: su_buf.as_entire_binding(),
                },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("flight"),
            bind_group_layouts: &[Some(&g0), Some(sample_bgl)],
            immediate_size: 0,
        });
        let depth_state = wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth32Float,
            depth_write_enabled: Some(true),
            // `GreaterEqual`, not the map/VAB `Greater`: the fallback
            // sky's NDC z = 0 IS the far plane, and `Greater` can never
            // pass against a buffer cleared to 0.0 — geometry at the
            // far plane would be culled to nothing.
            depth_compare: Some(wgpu::CompareFunction::GreaterEqual),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        };
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("flight"),
            layout: Some(&layout),
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
                ..wgpu::PrimitiveState::default()
            },
            depth_stencil: Some(depth_state),
            multisample: crate::msaa_state(),
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
            sample_count: crate::MSAA_SAMPLES,
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
            far_verts: GpuBuffer::new(
                device,
                "flight-far-verts",
                // Small steady state: the fallback sky's two triangles.
                (std::mem::size_of::<FlightVertex>() * 256) as u64,
            ),
            light_buf,
            light_bind,
            shadow_bind: sample_bind.clone(),
            depth,
            depth_view,
            size,
            staged_verts: None,
        }
    }

    /// Recreate the depth target after a resize — `width`/`height`
    /// are the SCENE size (P1: `Gpu::scene_size()`, the scaled
    /// attachment the main pass pairs this depth with).
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
            sample_count: crate::MSAA_SAMPLES,
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

    /// 3b (R1): stage this frame's flight stream for the cascade
    /// craft cast — upload the vertices and hand back the buffer +
    /// count `TerrainRenderer::draw_shadow` records into both cascade
    /// layers. Call BEFORE the scene pass (the cast precedes every
    /// receiver); `None` when the frame is disarmed or the stream is
    /// empty. Since G3+ the visible [`Self::draw`] does NOT re-stage
    /// the identical bytes: this call records the uploaded slice's
    /// `(address, len)` in `staged_verts` and `draw` skips its write
    /// when it sees the same slice (the app and the render gate both
    /// pair them over one immutable packet per frame).
    pub fn stage_cascade_cast(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        packet: &FlightPacket,
    ) -> Option<(wgpu::Buffer, u32)> {
        // Invalidate any memo a previous frame left behind FIRST —
        // this call leads every flight frame that carries terrain
        // (the app calls it armed or not), so a stale memo can never
        // outlive the frame that set it except through a mid-frame
        // abort followed by a terrain-less frame (documented at the
        // field).
        self.staged_verts = None;
        if !packet.shadow.armed() {
            return None;
        }
        let n = self.verts.upload(device, queue, &packet.vertices);
        if n > 0 {
            self.staged_verts = Some((packet.vertices.as_ptr() as usize, packet.vertices.len()));
            Some((self.verts.buffer.clone(), n))
        } else {
            None
        }
    }

    /// §2.3's far-field draw: the packet's [`FlightPacket::far_vertices`]
    /// through the SAME pipeline (their `pos.z` already carries the far
    /// remap `z = FAR_SPLIT/d` — `vs_main` is a passthrough, so no
    /// shader variant exists). Call inside the frame's FIRST render
    /// pass (the far pass: shared depth attachment, cleared 0.0);
    /// the near pass that follows clears the depth again and draws
    /// over, so far and near streams can never fight one depth buffer.
    /// The light uniform write is idempotent with [`Self::draw`]'s —
    /// both land before the frame's submit.
    pub fn draw_far(
        &mut self,
        pass: &mut wgpu::RenderPass<'_>,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        packet: &FlightPacket,
    ) {
        let n = self.far_verts.upload(device, queue, &packet.far_vertices);
        if n == 0 {
            return;
        }
        queue.write_buffer(&self.light_buf, 0, bytemuck::bytes_of(&packet.light));
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.light_bind, &[]);
        pass.set_bind_group(1, &self.shadow_bind, &[]);
        pass.set_vertex_buffer(0, self.far_verts.buffer.slice(..));
        pass.draw(0..n, 0..1);
    }

    /// Upload the packet and draw. When [`Self::stage_cascade_cast`]
    /// already staged this same slice this frame (the app's armed
    /// frame shape), the identical staging write is skipped — the
    /// memo is consumed here, so it never leaks across frames.
    pub fn draw(
        &mut self,
        pass: &mut wgpu::RenderPass<'_>,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        packet: &FlightPacket,
    ) {
        // Consume the memo FIRST — even the empty-stream early return
        // below must not leave it live for a later frame.
        let staged = self.staged_verts.take()
            == Some((packet.vertices.as_ptr() as usize, packet.vertices.len()));
        let n = if staged {
            packet.vertices.len() as u32
        } else {
            self.verts.upload(device, queue, &packet.vertices)
        };
        if n == 0 {
            return;
        }
        queue.write_buffer(&self.light_buf, 0, bytemuck::bytes_of(&packet.light));
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.light_bind, &[]);
        // The cascade receiving set (R1/3b) — the terrain pass's own
        // group(1); a disarmed `SU` keeps the lookup dead.
        pass.set_bind_group(1, &self.shadow_bind, &[]);
        pass.set_vertex_buffer(0, self.verts.buffer.slice(..));
        pass.draw(0..n, 0..1);
    }
}
