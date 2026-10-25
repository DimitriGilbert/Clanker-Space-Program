// Near-flight view (M4c): sky/ground/craft triangle pass. Same
// reverse-Z depth convention as the map and VAB (z: 1 = near, 0 = far;
// `GreaterEqual` wins — the fallback sky lives exactly ON the far
// plane, which a strict `Greater` would cull to nothing).
//
// Vertices arrive already in clip space (csp-view projects in f64
// camera-relative, §2.3); the vertex stage is a passthrough that also
// forwards the camera-relative world position, the world normal, the
// linear albedo, and the baked material terms. Lit vertices (nonzero
// normal) get per-pixel lighting here: hemisphere ambient × baked
// occlusion, a directional sun, Blinn specular from the vertex's
// material pair, and a hemisphere fresnel rim — the per-pixel shading
// that keeps the baked models reading as machined parts instead of
// flat-shaded facets. A zero normal marks unlit geometry (sky quads,
// plumes, dust, selection accents) which passes its color through.

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
};

struct Vout {
    @builtin(position) clip: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) nrm: vec3<f32>,
    @location(2) color: vec4<f32>,
    @location(3) mat: vec3<f32>,
};

@vertex
fn vs_main(v: Vin) -> Vout {
    var o: Vout;
    o.clip = vec4<f32>(v.pos, 1.0);
    o.world = v.world;
    o.nrm = v.nrm;
    o.color = v.color;
    o.mat = v.mat;
    return o;
}

@fragment
fn fs_main(i: Vout) -> @location(0) vec4<f32> {
    let nn = dot(i.nrm, i.nrm);
    if nn < 0.25 {
        return i.color; // unlit stream (sky, plumes, dust, accents)
    }
    var n = i.nrm * inverseSqrt(nn);
    let v = -normalize(i.world); // camera-relative: the eye is the origin
    // Two-sided lighting — the pass never culls, so a fragment seen
    // from inside a shell shades against the flipped normal.
    if dot(n, v) < 0.0 {
        n = -n;
    }
    let l = light.sun_dir.xyz;
    let ndl = dot(n, l);
    // Hemisphere ambient (sky/ground bounce) modulated by the baked
    // cavity occlusion.
    let hw = clamp(0.5 + 0.5 * dot(n, light.hemi.xyz), 0.0, 1.0);
    let amb = mix(light.amb_dn.rgb, light.amb_up.rgb, hw) * i.mat.z;
    var rgb = i.color.rgb * (amb + light.sun_col.rgb * max(ndl, 0.0));
    // Blinn specular, gated to the lit side; strength + exponent ride
    // the vertex's baked material pair.
    let h = normalize(l + v);
    let spec = i.mat.x
        * pow(max(dot(n, h), 0.0), max(i.mat.y, 1.0))
        * clamp(ndl * 8.0, 0.0, 1.0);
    rgb += light.sun_col.rgb * spec;
    // Hemisphere fresnel rim — the cool edge light that separates the
    // silhouette from the backdrop.
    let fres = pow(clamp(1.0 - dot(n, v), 0.0, 1.0), 3.0) * light.hemi.w * i.mat.z;
    rgb += light.amb_up.rgb * fres;
    return vec4<f32>(rgb, i.color.a);
}
