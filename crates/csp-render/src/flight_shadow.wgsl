// Craft cast into the R1 cascade map (terrain-overhaul 3b). A
// dedicated module rather than an island inside flight.wgsl: the
// depth-only cast pipelines ride their own minimal group(0) — the
// `SU` uniform alone — and a module that also declares the visible
// pass's `light` (group 0, binding 0) or the group(1) shadow sample
// set leaves those declarations dangling against the cast layout,
// which de-registers the uniform binding on-device (the cast read
// `su` as zeros and silently discarded everything — pinned by the
// pad-probe triangle experiments during 3b debugging). This file is
// the WGSL twin of the cast half; the receiving half
// (`sun_visibility`) lives in flight.wgsl and terrain.wgsl (C16-F3
// duplication rule — no include mechanism).
//
// Vertex input is `FlightVertex` (flight.wgsl's `Vin`): `world` is
// the camera-relative body-frame position — the exact `wrel` the
// terrain cast writes (the bubble frame differs by translation
// only). Unlit vertices (zero normal: sky quads, plumes, dust,
// sheath, contact quads carry placeholder `world`) are degenerated
// outside the clip volume — a triangle whose every corner sits there
// rasterizes nothing, so emissive geometry never casts.

struct SU {
    // Cascade 0/1 light-view-proj: (world − eye) → cascade clip.
    // Verbatim twin of terrain.wgsl's `SU` (shadow.rs owns the Rust
    // layout, `ShadowUniforms`).
    vp: array<mat4x4<f32>, 2>,
    // Per-cascade bias row: x world texel (m), y normal-offset bias
    // (m), z reference bias (normalized depth), w pad.
    cascade: array<vec4<f32>, 2>,
    // x armed, y split (m), z coverage-far (m), w blend band (m).
    pars: vec4<f32>,
};

@group(0) @binding(8) var<uniform> su: SU;

struct Vin {
    @location(0) pos: vec3<f32>,
    @location(1) world: vec3<f32>,
    @location(2) nrm: vec3<f32>,
    @location(3) color: vec4<f32>,
    @location(4) mat: vec3<f32>,
};

// One entry point per array layer, the terrain pair's convention
// (wgpu has no layered rendering without features). The unlit gate is
// PER VERTEX, which relies on a stream invariant worth pinning: the
// extract never emits a MIXED triangle (lit corners and unlit corners
// in one primitive — every emitter is homogeneous per tri), so the
// degenerate corner trick whole-triangle-culls the unlit geometry. A
// mixed triangle would instead clip only its unlit corner and cast a
// sliver from the lit remainder; no such triangle exists in the
// stream today — if one is ever introduced (e.g. a baked part mesh
// with a placeholder normal), it must set nrm ≈ 0 on ALL its corners
// or be split, never relied on this gate to remove it whole.
@vertex
fn vs_shadow0(v: Vin) -> @builtin(position) vec4<f32> {
    if (dot(v.nrm, v.nrm) < 0.25) {
        return vec4<f32>(0.0, 0.0, 2.0, 1.0);
    }
    return su.vp[0] * vec4<f32>(v.world, 1.0);
}

@vertex
fn vs_shadow1(v: Vin) -> @builtin(position) vec4<f32> {
    if (dot(v.nrm, v.nrm) < 0.25) {
        return vec4<f32>(0.0, 0.0, 2.0, 1.0);
    }
    return su.vp[1] * vec4<f32>(v.world, 1.0);
}
