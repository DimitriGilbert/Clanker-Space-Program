// VAB craft view (M3): placeholder part boxes + object-id picking.
//
// Vertices arrive already in clip space (csp-view projects in f64
// camera-relative, §2.3); the vertex stage is a passthrough. The color
// fragment writes the part's flat color; the pick fragment writes the
// part's 1-based pick index to an R32Uint offscreen target (0 =
// background).

struct Vin {
    @location(0) pos: vec3<f32>,
    @location(1) color: vec4<f32>,
    @location(2) pick: u32,
};

struct Vout {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
    // Integer varyings have no default interpolation — WGSL requires an
    // explicit `flat` (naga: InvalidInterpolationForInteger otherwise).
    @location(1) @interpolate(flat) pick: u32,
};

@vertex
fn vs_main(v: Vin) -> Vout {
    var o: Vout;
    o.clip = vec4<f32>(v.pos, 1.0);
    o.color = v.color;
    o.pick = v.pick;
    return o;
}

@fragment
fn fs_main(i: Vout) -> @location(0) vec4<f32> {
    return i.color;
}

@fragment
fn fs_pick(i: Vout) -> @location(0) u32 {
    return i.pick;
}
