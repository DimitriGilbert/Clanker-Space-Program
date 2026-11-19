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
//
// R1 shadow cascades (terrain-overhaul 3b): this module both CASTS
// (`vs_shadow0/1` — the depth-only twins that drop the flight stream
// into the cascade map; see `shadow.rs`) and RECEIVES (`sun_visibility`
// multiplies the direct-sun terms). Both halves share `terrain.wgsl`'s
// conventions verbatim — C16-F3's rule: the build has no WGSL
// include/preprocessor (each module loads via `include_str!`), so
// duplicated bodies carry cross-references and stay in sync by test.

struct Light {
    sun_dir: vec4<f32>,
    sun_col: vec4<f32>,
    amb_up: vec4<f32>,
    amb_dn: vec4<f32>,
    hemi: vec4<f32>,
};

@group(0) @binding(0) var<uniform> light: Light;

// The cascade uniform — a verbatim copy of terrain.wgsl's `SU` block
// (shadow.rs owns the Rust twin, `ShadowUniforms`). Binding 8 of
// group(0) on purpose: the CRAFT-CAST pipelines (below) ride the
// terrain bind group at group(0), whose binding 8 is this uniform —
// one declaration serves the visible pipeline's own group(0) (light
// at 0, `SU` at 8) and the cast pipelines' terrain group(0). All-zero
// = disarmed: `sun_visibility` short-circuits to fully lit.
struct SU {
    // Cascade 0/1 light-view-proj: (world − eye) → cascade clip.
    vp: array<mat4x4<f32>, 2>,
    // Per-cascade bias row: x world texel (m), y normal-offset bias
    // (m), z reference bias (normalized depth), w pad.
    cascade: array<vec4<f32>, 2>,
    // x armed (0 = no cascades), y split distance (m), z coverage-far
    // distance (m), w blend band (m).
    pars: vec4<f32>,
};

@group(0) @binding(8) var<uniform> su: SU;

// The receiving set — structurally terrain.wgsl's group(1) (the SAME
// bind group object is set: depth array + comparison sampler owned by
// `ShadowCore`). Bound only by the visible pass; the cast passes ride
// group(0) alone so they never bind the texture they render into.
@group(1) @binding(0) var shadow_tex: texture_depth_2d_array; // 2 layers
@group(1) @binding(1) var shadow_samp: sampler_comparison;     // LessEqual

// Craft CASTING (3b) lives in flight_shadow.wgsl — a dedicated
// module whose only group(0) member is `su`; keeping the cast entries
// here de-registered the binding against the cast layout's sparse
// binding set (see that file's header).

// ACES filmic fit (Narkowicz) — the same curve the terrain pass
// applies, so the lit hull shares the surface's highlight rolloff and
// doesn't read over-bright next to the tonemapped ground. Unlit
// streams (sky quads, plumes, dust, accents) return raw: their colors
// are authored emissive, not lit material.
// C16-F3: verbatim copies live in terrain.wgsl and vab.wgsl — the
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

// --- cascade receive (R1; craft half — terrain.wgsl's `cascade_one`/
// `sun_visibility` duplicated per C16-F3, arguments trimmed to what a
// hull needs: no heightfield ray-march fallback exists for craft, so
// disarmed/beyond-coverage/back-facing simply return full light) ----
// The sign chain is terrain.wgsl's: normal-offset along +n, slope
// bias SUBTRACTED from the projected depth (both toward the sun).
// `textureSampleCompareLevel` keeps the call legal in this
// data-dependent flow (single-mip array — no derivatives needed).
fn cascade_one(wrel: vec3<f32>, n: vec3<f32>, tan_slope: f32,
               vp: mat4x4<f32>, cb: vec4<f32>, layer: i32) -> f32 {
    let p = wrel + n * (cb.y * (1.0 + tan_slope));
    let cd = vp * vec4<f32>(p, 1.0);
    // Mirrored v, terrain.wgsl's twin: NDC y=+1 rasterizes at row 0,
    // so the read flips y (`0.5 − y·0.5`) — see that file for the
    // artifact the unmirrored form produced.
    let uv = clamp(vec2<f32>(cd.x * 0.5 + 0.5, 0.5 - cd.y * 0.5),
                   vec2<f32>(0.0), vec2<f32>(1.0));
    let ref_d = clamp(cd.z - cb.z * (1.0 + tan_slope), 0.0, 1.0);
    return textureSampleCompareLevel(shadow_tex, shadow_samp, uv, layer, ref_d);
}

// Sun visibility for a lit hull fragment: cascade lookup by EYE
// distance (`length(wrel)`), split blend and bias rows exactly as the
// terrain half selects. Disarmed (or beyond the coverage-far) ⇒ 1.0:
// the pre-cascade craft lighting is the fallback, unchanged.
fn sun_visibility(wrel: vec3<f32>, n: vec3<f32>) -> f32 {
    if (su.pars.x < 0.5) {
        return 1.0;
    }
    let dist = length(wrel);
    if (dist > su.pars.z) {
        return 1.0;
    }
    let l = light.sun_dir.xyz;
    let ndl = dot(n, l);
    // Back-facing to the sun: the direct term is already zero, so the
    // lookup cannot change the shade — skip it (also avoids the large
    // tan_slope of a grazing term biasing a dead sample).
    if (ndl <= 0.0) {
        return 1.0;
    }
    let tan_slope = length(n - l * ndl) / clamp(ndl, 0.25, 1.0);
    let split = su.pars.y;
    if (dist >= split) {
        return cascade_one(wrel, n, tan_slope, su.vp[1], su.cascade[1], 1);
    }
    let v0 = cascade_one(wrel, n, tan_slope, su.vp[0], su.cascade[0], 0);
    if (dist <= split - su.pars.w) {
        return v0;
    }
    let band = smoothstep(split - su.pars.w, split, dist);
    return mix(v0, cascade_one(wrel, n, tan_slope, su.vp[1], su.cascade[1], 1), band);
}

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
    // R1 (3b): the direct-sun terms scale with cascade visibility —
    // the same map the terrain under the hull samples, so a ridge
    // line crossing the sun darkens hull and ground together. The
    // hemisphere ambient and fresnel rim stay unshadowed (sky bounce
    // has no caster).
    let vis = sun_visibility(i.world, n);
    // Hemisphere ambient (sky/ground bounce) modulated by the baked
    // cavity occlusion.
    let hw = clamp(0.5 + 0.5 * dot(n, light.hemi.xyz), 0.0, 1.0);
    let amb = mix(light.amb_dn.rgb, light.amb_up.rgb, hw) * i.mat.z;
    var rgb = i.color.rgb * (amb + light.sun_col.rgb * max(ndl, 0.0) * vis);
    // Blinn specular, gated to the lit side; strength + exponent ride
    // the vertex's baked material pair.
    let h = normalize(l + v);
    let spec = i.mat.x
        * pow(max(dot(n, h), 0.0), max(i.mat.y, 1.0))
        * clamp(ndl * 8.0, 0.0, 1.0);
    rgb += light.sun_col.rgb * spec * vis;
    // Hemisphere fresnel rim — the cool edge light that separates the
    // silhouette from the backdrop.
    let fres = pow(clamp(1.0 - dot(n, v), 0.0, 1.0), 3.0) * light.hemi.w * i.mat.z;
    rgb += light.amb_up.rgb * fres;
    return vec4<f32>(tone_map(rgb), i.color.a);
}
