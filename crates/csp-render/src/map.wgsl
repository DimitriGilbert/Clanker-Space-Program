// M1 map-view shader (PLAN §5.1 R0 — the honest minimum: unlit colored
// primitives). Vertices arrive ALREADY projected into wgpu clip space
// (NDC): the f64 camera-relative transform runs on the CPU in csp-view
// (§2.3 — the GPU never sees f64), so this shader is a pure passthrough.

struct VsIn {
    @location(0) pos: vec3<f32>,
    @location(1) color: vec4<f32>,
};

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var out: VsOut;
    out.pos = vec4<f32>(in.pos, 1.0);
    out.color = in.color;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return in.color;
}
