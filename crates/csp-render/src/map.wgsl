// Map-view shader (PLAN §5.1 R0): unlit colored primitives, shaped in
// the fragment stage. Vertices arrive ALREADY projected into wgpu clip
// space (NDC): the f64 camera-relative transform runs on the CPU in
// csp-view (§2.3 — the GPU never sees f64), so the vertex stage is a
// passthrough. `uv`/`kind` shape the quad: 0 solid (line segments),
// 1 soft disc, 2 ring, 3 radial glow.

struct VsIn {
    @location(0) pos: vec3<f32>,
    @location(1) color: vec4<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) kind: f32,
};

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) color: vec4<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) kind: f32,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var out: VsOut;
    out.pos = vec4<f32>(in.pos, 1.0);
    out.color = in.color;
    out.uv = in.uv;
    out.kind = in.kind;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let r = length(in.uv);
    var a = in.color.a;
    if in.kind > 2.5 {
        // Radial glow — quadratic falloff to zero at the quad edge.
        let t = max(0.0, 1.0 - r);
        a = a * t * t;
    } else if in.kind > 1.5 {
        // Ring — a thin band at ~80% radius, AA on both edges.
        a = a * (smoothstep(0.62, 0.78, r) - smoothstep(0.84, 1.0, r));
    } else if in.kind > 0.5 {
        // Soft disc — solid to ~80%, AA edge to the quad's corner bound.
        a = a * (1.0 - smoothstep(0.8, 1.0, r));
    }
    return vec4<f32>(in.color.rgb, a);
}
