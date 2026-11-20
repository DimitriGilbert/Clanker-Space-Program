// P1's render-scale upscale: one fullscreen triangle sampling the
// scaled, MSAA-resolved scene into the swapchain frame. A straight
// linear-filter upscale (scale <= 1 — downscaling never happens), no
// sharpening, no gamma games: the source is the surface's own sRGB
// format, so the sample→write round trip is the identity on the 8-bit
// grid. The egui pass composites AFTER this at native resolution.

@group(0) @binding(0) var src: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;

struct VsOut {
    @builtin(position) pos: vec4f,
    @location(0) uv: vec2f,
};

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> VsOut {
    // One oversized triangle covering the viewport. uv maps NDC to the
    // source texels with the y-flip (NDC y is up, texture v is down):
    // NDC (-1,-1) is the framebuffer's bottom-left but the source's
    // LAST row, hence uv (0,1) there.
    var p = array<vec2f, 3>(
        vec2f(-1.0, -1.0),
        vec2f(3.0, -1.0),
        vec2f(-1.0, 3.0),
    );
    var u = array<vec2f, 3>(
        vec2f(0.0, 1.0),
        vec2f(2.0, 1.0),
        vec2f(0.0, -1.0),
    );
    var out: VsOut;
    out.pos = vec4f(p[vi], 0.0, 1.0);
    out.uv = u[vi];
    return out;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4f {
    return textureSample(src, samp, in.uv);
}
