// Near-flight view (M4c): sky/ground/craft triangle pass — flat
// vertex colors, no picking. Same reverse-Z depth convention as the
// map and VAB (z: 1 = near, 0 = far; `GreaterEqual` wins — the
// fallback sky lives exactly ON the far plane, which a strict
// `Greater` would cull to nothing).

struct Vin {
    @location(0) pos: vec3<f32>,
    @location(1) color: vec4<f32>,
};

struct Vout {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_main(v: Vin) -> Vout {
    var o: Vout;
    o.clip = vec4<f32>(v.pos, 1.0);
    o.color = v.color;
    return o;
}

@fragment
fn fs_main(i: Vout) -> @location(0) vec4<f32> {
    return i.color;
}
