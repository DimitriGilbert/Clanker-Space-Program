// VAB craft view (M3): per-pixel-lit part meshes + object-id picking.
//
// Vertices arrive already in clip space (csp-view projects in f64
// camera-relative, §2.3); the vertex stage is a passthrough that also
// forwards the camera-relative world position, the world normal, the
// linear albedo, and the baked material terms. Lit vertices (nonzero
// normal) get per-pixel lighting here — hemisphere ambient × baked
// occlusion, a directional key light, Blinn specular from the
// vertex's material pair, and a fresnel rim — so the kit models read
// as smooth machined parts in the editor. A zero normal marks unlit
// geometry (selection shells, attach-node markers, ghost tint) which
// passes its color through. The pick fragment writes the part's
// 1-based pick index to an R32Uint offscreen target (0 = background)
// — it ignores the lighting varyings entirely.

struct Light {
    sun_dir: vec4<f32>,
    sun_col: vec4<f32>,
    amb_up: vec4<f32>,
    amb_dn: vec4<f32>,
    hemi: vec4<f32>,
};

@group(0) @binding(0) var<uniform> light: Light;

struct Vin {
    @location(0) pos: vec3<f32>,
    @location(1) world: vec3<f32>,
    @location(2) nrm: vec3<f32>,
    @location(3) color: vec4<f32>,
    @location(4) mat: vec3<f32>,
    @location(5) pick: u32,
};

struct Vout {
    @builtin(position) clip: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) nrm: vec3<f32>,
    @location(2) color: vec4<f32>,
    @location(3) mat: vec3<f32>,
    // Integer varyings have no default interpolation — WGSL requires an
    // explicit `flat` (naga: InvalidInterpolationForInteger otherwise).
    @location(4) @interpolate(flat) pick: u32,
};

// ACES filmic fit (Narkowicz) — the same curve the flight and terrain
// passes apply, so VAB parts roll off highlights identically to the
// flight hull instead of clipping to flat white (raw RGB here used to
// read over-bright against the tonemapped scene). The unlit stream
// (shells, markers, accents — the `nn < 0.25` early return below)
// stays raw: those colors are authored emissive, not lit material.
// C16-F3: verbatim copies live in terrain.wgsl and flight.wgsl — the
// build has no WGSL include/preprocessor (each module loads via
// `include_str!`), so keep the bodies in sync until a shared-source
// mechanism lands.
fn tone_map(c: vec3<f32>) -> vec3<f32> {
    let a = 2.51;
    let b = 0.03;
    let d = 2.43;
    let e = 0.59;
    let f = 0.14;
    return clamp(
        (c * (a * c + vec3<f32>(b))) / (c * (d * c + vec3<f32>(e)) + vec3<f32>(f)),
        vec3<f32>(0.0),
        vec3<f32>(1.0),
    );
}

@vertex
fn vs_main(v: Vin) -> Vout {
    var o: Vout;
    o.clip = vec4<f32>(v.pos, 1.0);
    o.world = v.world;
    o.nrm = v.nrm;
    o.color = v.color;
    o.mat = v.mat;
    o.pick = v.pick;
    return o;
}

@fragment
fn fs_main(i: Vout) -> @location(0) vec4<f32> {
    let nn = dot(i.nrm, i.nrm);
    if nn < 0.25 {
        return i.color; // unlit stream (shells, markers, accents)
    }
    var n = i.nrm * inverseSqrt(nn);
    let v = -normalize(i.world); // camera-relative: the eye is the origin
    // Two-sided lighting — the pass never culls.
    if dot(n, v) < 0.0 {
        n = -n;
    }
    let l = light.sun_dir.xyz;
    let ndl = dot(n, l);
    let hw = clamp(0.5 + 0.5 * dot(n, light.hemi.xyz), 0.0, 1.0);
    let amb = mix(light.amb_dn.rgb, light.amb_up.rgb, hw) * i.mat.z;
    var rgb = i.color.rgb * (amb + light.sun_col.rgb * max(ndl, 0.0));
    let h = normalize(l + v);
    let spec = i.mat.x
        * pow(max(dot(n, h), 0.0), max(i.mat.y, 1.0))
        * clamp(ndl * 8.0, 0.0, 1.0);
    rgb += light.sun_col.rgb * spec;
    let fres = pow(clamp(1.0 - dot(n, v), 0.0, 1.0), 3.0) * light.hemi.w * i.mat.z;
    rgb += light.amb_up.rgb * fres;
    return vec4<f32>(tone_map(rgb), i.color.a);
}

@fragment
fn fs_pick(i: Vout) -> @location(0) u32 {
    return i.pick;
}
