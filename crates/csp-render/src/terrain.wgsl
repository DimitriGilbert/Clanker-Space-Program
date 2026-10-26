// R2 terrain + atmosphere (PLAN §5.2): instanced quadtree patches over a
// pre-baked equirect heightfield, transmittance-LUT scattering (baseline
// tier's "precomputed LUTs, analytic single-scatter"), ocean instances,
// and a fullscreen scattering sky.
//
// Precision (§2.3): vertices are built patch-locally —
//   world_rel_eye = R·(dir − dir0) + dir·h + (patch_origin − eye)
// where `patch_origin − eye` was baked f64→f32 on the CPU and
// `dir − dir0` is a small unit-space delta. No f32 vector at
// planetary magnitude is ever subtracted from another.
//
// Depth: same reverse-Z convention as map/vab/flight (1 = near, 0 = far).

struct U {
    view_proj: mat4x4<f32>,   // (world − eye) → clip, column-major
    view_rot: mat4x4<f32>,    // view → world rotation (columns = axes)
    eye_rel_body: vec4<f32>,  // camera pos rel body centre (m)
    sun_dir: vec4<f32>,       // unit sun direction, body frame
    planet: vec4<f32>,        // x datum R, y atm_top (0 = vacuum), z ocean_r, w amplitude
    atmo: vec4<f32>,          // x scale_height, y has_biome, z craft alt (m, shadow fade), w unused
    misc: vec4<f32>,          // x tan(fov_y/2)·aspect, y tan(fov_y/2),
                              // z/w fs_sky shell constants |pa|²−r² (CPU f64 bake)
    spin: vec4<f32>,          // xyz unit spin axis, w phase ω·t (inertial → fixed)
    rayleigh: vec4<f32>,      // Rayleigh β rgb (1/m)
    biome_colors: array<vec4<f32>, 16>,
    shadow: vec4<f32>,        // xyz vessel-shadow anchor dir (inertial, unit), w blob radius (m, 0 = off)
    art: vec4<f32>,           // x real-texture flag (0 = procedural fallback), y clock s, z/w unused
    class_means: array<vec4<f32>, 4>, // per-class mean LINEAR albedo (pack header — single source of truth)
};

@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var heightmap: texture_2d<f32>;   // R32Float equirect
@group(0) @binding(2) var tlut: texture_2d<f32>;        // RGBA32Float T(h, μ)
@group(0) @binding(3) var biomemap: texture_2d<u32>;    // R8Uint slots
// Real art (data/art/terrain.csar, ambientCG — see
// assets/ASSETS.md): class albedo/normal arrays keyed
// [0 grass, 1 rock, 2 sand, 3 snow] and the ocean wave normal map.
// All 1×1 placeholders when the packet carries no art (u.art.x == 0
// keeps them unread). The night sky is fully procedural — no star
// texture exists to bind.
@group(0) @binding(4) var alb_arr: texture_2d_array<f32>;
@group(0) @binding(5) var nrm_arr: texture_2d_array<f32>;
@group(0) @binding(6) var water_nrm: texture_2d<f32>;
@group(0) @binding(7) var samp: sampler;

const PI: f32 = 3.141592653589793;

// --- heightfield sampling: manual bilinear over texel centres,
// longitude wraps, latitude clamps. Mirrors HeightField::sample_dir.
fn h_texel(i: i32, j: i32) -> f32 {
    let d = vec2<i32>(textureDimensions(heightmap));
    let ii = ((i % d.x) + d.x) % d.x;
    let jj = clamp(j, 0, d.y - 1);
    return textureLoad(heightmap, vec2<i32>(ii, jj), 0).x;
}

fn height_at(dir: vec3<f32>) -> f32 {
    let lat = asin(clamp(dir.z, -1.0, 1.0));
    let lon = atan2(dir.y, dir.x);
    let d = vec2<f32>(textureDimensions(heightmap));
    let uu = ((lon + PI) / (2.0 * PI)) * d.x - 0.5;
    let vv = ((lat + 0.5 * PI) / PI) * d.y - 0.5;
    let i0 = i32(floor(uu));
    let j0 = i32(floor(vv));
    let fu = uu - f32(i0);
    let fv = vv - f32(j0);
    let a = h_texel(i0, j0);
    let b = h_texel(i0 + 1, j0);
    let c = h_texel(i0, j0 + 1);
    let e = h_texel(i0 + 1, j0 + 1);
    return a + (b - a) * fu + (c - a) * fv + (a - b - c + e) * fu * fv;
}

// Axis-angle rotation (Rodrigues). The heightfield, biome map and
// ocean mask are *body-fixed* data while patch/vertex directions are
// inertial (§2.3); `u.spin` carries the same R(axis, ω·t) phase the
// sim's §2.6.4 surface latch co-rotates latched poses by. Sampling at
// the conjugate-rotated direction makes the terrain slide under a
// landed craft at exactly the surface velocity — positions and
// lighting stay inertial.
fn rot_axis(axis: vec3<f32>, angle: f32, v: vec3<f32>) -> vec3<f32> {
    let c = cos(angle);
    let s = sin(angle);
    return v * c + cross(axis, v) * s + axis * dot(axis, v) * (1.0 - c);
}

// --- procedural detail: deterministic integer-hash value noise on the
// sphere. This is the "not a smudge" channel — the baked heightfield
// tops out near ~4 km texels while the quadtree subdivides to ~300 m
// patches, so everything finer than a texel is synthesized here at
// fragment rate: mid-scale relief bump normals, albedo mottling, water
// waves, star cells. Deterministic by construction (u32 avalanche),
// camera-independent (sampled on the body-fixed direction).
//
// STABILITY: every noise lookup below is a DIRECT `vnoise3(d·freq)`
// evaluation — no anchor vector, no camera-derived term. The lattice
// coordinate is a pure function of the body-fixed direction, so the
// field is bit-identical frame to frame and identical across quadtree
// re-partitions (the old per-patch anchor moved the pattern every LOD
// event — the "ground looks like it is moving" crawl). The catch is
// f32: `d·freq` rounds to ~freq·2⁻²⁴ cells of per-pixel error, so a
// point-sampled octave sparkles once freq ≳ ~25k (its 1–2% boundary
// flips re-randomize with sub-pixel camera motion — that WAS the
// shimmer). Everything finer than ~40 m cells therefore comes from the
// mip/aniso-FILTERED texture maps, which absorb sub-texel jitter,
// instead of unfiltered hash noise. Keep every `vnoise3w` call at
// freq ≤ ~16000.

fn hash_i3(x: i32, y: i32, z: i32) -> f32 {
    var h = bitcast<u32>(x) * 0x8da6b343u;
    h = h ^ (bitcast<u32>(y) * 0xd8163841u);
    h = h ^ (bitcast<u32>(z) * 0xcb1ab31fu);
    h = (h ^ (h >> 15u)) * 0x85ebca6bu;
    h = (h ^ (h >> 13u)) * 0xc2b2ae35u;
    h = h ^ (h >> 16u);
    return f32(h & 0xffffffu) * (1.0 / 16777216.0); // [0,1)
}

// Trilinear value noise ∈ [-1,1). `p` is `dir·freq` — frequency is
// measured in cells per unit-direction, i.e. wavelength ≈ 2πR/freq.
fn vnoise3(p: vec3<f32>) -> f32 {
    let ip = floor(p);
    let i = vec3<i32>(ip);
    let f = p - ip;
    let t = f * f * (3.0 - 2.0 * f);
    let x00 = mix(hash_i3(i.x, i.y, i.z), hash_i3(i.x + 1, i.y, i.z), t.x);
    let x10 = mix(hash_i3(i.x, i.y + 1, i.z), hash_i3(i.x + 1, i.y + 1, i.z), t.x);
    let x01 = mix(hash_i3(i.x, i.y, i.z + 1), hash_i3(i.x + 1, i.y, i.z + 1), t.x);
    let x11 = mix(hash_i3(i.x, i.y + 1, i.z + 1), hash_i3(i.x + 1, i.y + 1, i.z + 1), t.x);
    return mix(mix(x00, x10, t.y), mix(x01, x11, t.y), t.z) * 2.0 - 1.0;
}

// World-stable noise sample (see the STABILITY note above): the raw
// `d·freq` product is exact enough up to ~freq 20k that the lattice
// evaluation never leaves the cell it should — and because the
// coordinate depends on NOTHING but the body-fixed direction, there is
// no anchor that could shift on a quadtree re-split. Never pass a
// freq above ~24k: finer octaves must come from the filtered textures.
fn vnoise3w(d: vec3<f32>, freq: f32) -> f32 {
    return vnoise3(d * freq);
}

// The fixed world-space anchor grid for TEXTURE coordinates (the
// Repeat sampler absorbs whole-cell shifts, so a quantized anchor is
// exact where it isn't for noise — see TriPlanes). 1/64 rad ≈ 9.4 km
// cells on a 600 km body.
fn dir_anchor(d: vec3<f32>) -> vec3<f32> {
    return floor(d * 64.0 + 0.5) / 64.0;
}

// A local tangent frame on the sphere (fixed frame): east = d/dlon,
// north = d/dlat. Pole-safe fallback matches the vertex shader's.
struct Basis { e: vec3<f32>, n: vec3<f32> };

fn tangent_basis(d: vec3<f32>) -> Basis {
    var east = cross(vec3<f32>(0.0, 0.0, 1.0), d);
    if (dot(east, east) < 1.0e-6) {
        east = vec3<f32>(1.0, 0.0, 0.0);
    } else {
        east = normalize(east);
    }
    var b: Basis;
    b.e = east;
    b.n = normalize(cross(d, east));
    return b;
}

// Geometric noise gradient at `dir·freq`: returns ∂n/∂x in noise units
// (∼±0.6), so a virtual relief of height `h_eq` metres converts to a
// surface slope as `grad · h_eq·freq/R`.
fn ngrad(d: vec3<f32>, e: vec3<f32>, n: vec3<f32>, freq: f32) -> vec2<f32> {
    let off = 0.8 / freq;
    let c0 = vnoise3w(d, freq);
    return vec2<f32>(
        vnoise3w(d + e * off, freq) - c0,
        vnoise3w(d + n * off, freq) - c0,
    ) / (off * freq);
}

// Henyey–Greenstein-ish forward lobe for the Mie haze term — cheap
// form, `d·sqrt(d)` instead of pow(d, 1.5).
fn mie_phase(mu: f32) -> f32 {
    let g = 0.72;
    let d = 1.0 + g * g - 2.0 * g * mu;
    return (1.0 - g * g) / (4.0 * PI * max(d * sqrt(d), 1.0e-6));
}

const MIE_BETA: vec3<f32> = vec3<f32>(2.9e-6, 2.9e-6, 2.9e-6); // 1/m, grey aerosol

// ACES filmic fit (Narkowicz) — applied per-channel on the linear HDR
// colour before the sRGB surface encode. The untone-mapped pipeline
// clipped every lit-slope/glint term above 1.0 to flat white (the
// "white worm" streaks) and let ~0.4-linear albedos land at ~0.75
// sRGB (the washed-out pale terrain). The curve compresses the top
// octave instead of clipping it, so texture detail survives.
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

// --- transmittance LUT: T(h, μ) manual bilinear (clamped both axes).
// The bake is *endpoint*-centered (bake_transmittance: col/(W−1),
// row/(H−1)), so the texel coordinate is `·(d − 1)` — NOT the
// texel-center `·d − 0.5` convention `height_at` uses (the heightfield
// bakes at (i+0.5)/w). The two conventions must not be shared: mixing
// them costs up to half a texel of systematic bias per axis.
fn trans(h: f32, mu: f32) -> vec3<f32> {
    if (u.planet.y <= 0.0) {
        return vec3<f32>(1.0, 1.0, 1.0);
    }
    let d = vec2<i32>(textureDimensions(tlut));
    let uu = (mu * 0.5 + 0.5) * f32(d.x - 1);
    let vv = clamp(h / u.planet.y, 0.0, 1.0) * f32(d.y - 1);
    let i0 = i32(floor(uu));
    let j0 = i32(floor(vv));
    let fu = uu - f32(i0);
    let fv = vv - f32(j0);
    var acc = vec3<f32>(0.0);
    for (var dj = 0; dj < 2; dj = dj + 1) {
        for (var di = 0; di < 2; di = di + 1) {
            let wi = select(1.0 - fu, fu, di == 1);
            let wj = select(1.0 - fv, fv, dj == 1);
            let p = vec2<i32>(
                clamp(i0 + di, 0, d.x - 1),
                clamp(j0 + dj, 0, d.y - 1),
            );
            acc = acc + textureLoad(tlut, p, 0).xyz * (wi * wj);
        }
    }
    return acc;
}

// Transmittance between two points on the same ray via the LUT ratio
// T(a→b) = T(a→∞)/T(b→∞) — exact only in the planar limit of an
// exponential atmosphere (μ constant along the ray); on the sphere it's
// the usual ratio approximation. Clamped where the LUT floors at 0
// (ground-blocked rays).
//
// The LUT only holds "to-space" transmittance, so a DESCENDING segment
// (mua < 0 — the ray grounds out past b) used to read 0 at both ends
// and collapse the ratio: every ground fragment on a below-horizon ray
// shaded featureless black (`col·0 + ins·0`). For a descending segment
// the fix is the same ratio taken along the ray's UP-going
// continuation — the continuation from the lower end passes through
// the upper end and on to space, so T(a→b) = T(b→∞)/T(a→∞) evaluated
// at zenith −mu.
fn trans_between(ha: f32, hb: f32, mua: f32, mub: f32) -> vec3<f32> {
    if (mua < 0.0) {
        let ta = trans(ha, -mua);
        return clamp(trans(hb, -mub) / max(ta, vec3<f32>(1.0e-4)),
                     vec3<f32>(0.0), vec3<f32>(1.0));
    }
    let tb = trans(hb, mub);
    return clamp(trans(ha, mua) / max(tb, vec3<f32>(1.0e-4)), vec3<f32>(0.0), vec3<f32>(1.0));
}

// Per-pixel march jitter — decorrelates the fixed step lattice so the
// aerial-perspective integral doesn't band (the same trick fs_sky's
// 20-step march already pulls).
fn screen_hash(px: vec2<f32>) -> f32 {
    return fract(sin(dot(px, vec2<f32>(12.9898, 78.233))) * 43758.5453);
}

// Analytic single-scatter along the eye→frag segment: N midpoint
// samples of  β·P_r(μ_sun)·T(eye→s)·T(s→sun)·ρ(s)  (the §5.1 Baseline
// "analytic single-scatter" — no inscatter LUT).
fn view_inscatter(pa: vec3<f32>, pb: vec3<f32>, ray: vec3<f32>, dist: f32, px: vec2<f32>) -> vec3<f32> {
    let R = u.planet.x;
    let H = max(u.atmo.x, 1.0);
    let Hm = max(H * 0.30, 1.0); // aerosol hugging the surface harder
    let mu_sun = dot(ray, u.sun_dir.xyz);
    let phase = (3.0 / (16.0 * PI)) * (1.0 + mu_sun * mu_sun);
    let phase_m = mie_phase(mu_sun);
    let alt_a = max(length(pa) - R, 0.0);
    let mua = dot(ray, pa / max(length(pa), 1.0));
    var acc = vec3<f32>(0.0);
    var acc_m = vec3<f32>(0.0);
    // 12 jittered steps — the old fixed 5-step lattice banded the
    // inscatter integral into visible shells on long view segments;
    // the per-pixel phase offset breaks the bands the same way the
    // sky march's jitter does.
    let steps = 12.0;
    let jit = screen_hash(px);
    let ds = dist / steps;
    for (var i = 0.0; i < steps; i = i + 1.0) {
        let p = pa + ray * ((i + 0.05 + jit * 0.9) * ds);
        let alt_i = max(length(p) - R, 0.0);
        let up = p / max(length(p), 1.0);
        let t_ei = trans_between(alt_a, alt_i, mua, dot(ray, up));
        let t_is = trans(alt_i, dot(u.sun_dir.xyz, up));
        acc = acc + u.rayleigh.rgb * phase * exp(-alt_i / H) * t_ei * t_is * ds;
        acc_m = acc_m + MIE_BETA * phase_m * exp(-alt_i / Hm) * t_ei * t_is * ds;
    }
    // Rayleigh gain ~4 (single-scatter proxy for multiple scatter) plus
    // a Mie veil that whitens the horizon and glows around the sun.
    return acc * 4.2 + acc_m * 1.6;
}

// --- terrain/ocean instanced draw -------------------------------------

struct Gv {
    @location(0) uv: vec2<f32>,
    @location(1) skirt: f32,
};

struct Inst {
    @location(3) corner: vec4<f32>,
    @location(4) du: vec4<f32>,
    @location(5) dv: vec4<f32>,
    @location(6) dir0_kind: vec4<f32>,
    @location(7) origin_skirt: vec4<f32>,
};

struct Vt {
    @builtin(position) clip: vec4<f32>,
    @location(0) dir: vec3<f32>,
    @location(1) pos_rel_body: vec3<f32>,
    @location(2) h_norm: f32,
    @location(3) @interpolate(flat) kind: f32,
    @location(4) dir_fix: vec3<f32>,
};

@vertex
fn vs_terrain(g: Gv, inst: Inst) -> Vt {
    let cube = inst.corner.xyz + g.uv.x * inst.du.xyz + g.uv.y * inst.dv.xyz;
    let dir = normalize(cube);
    // `dir` is inertial; the heightfield/biome data is body-fixed —
    // sample at R(axis, −phase)·dir (see `rot_axis`).
    let dir_fix = rot_axis(u.spin.xyz, -u.spin.w, dir);
    let ocean = inst.dir0_kind.w > 0.5;
    let radius = select(u.planet.x, u.planet.z, ocean);
    var h_norm = height_at(dir_fix);
    var h = h_norm * u.planet.w;
    if (ocean) {
        h_norm = 0.0;
        h = 0.0;
    }
    // Patch-local position: small deltas only (§2.3).
    var local = radius * (dir - inst.dir0_kind.xyz) + dir * h;
    if (g.skirt > 0.5) {
        local = local - dir * inst.origin_skirt.w;
    }
    let wrel = local + inst.origin_skirt.xyz;

    // Shading normals are built per-fragment now (fs_terrain derives
    // the heightfield gradient + procedural bump at the interpolated
    // direction) — the vertex stage only carries geometry.

    var o: Vt;
    o.clip = u.view_proj * vec4<f32>(wrel, 1.0);
    o.dir = dir;
    o.pos_rel_body = dir * (radius + h);
    o.h_norm = h_norm;
    o.kind = inst.dir0_kind.w;
    o.dir_fix = dir_fix;
    return o;
}

// Per-pixel macro normal from the heightfield gradient (fixed frame) —
// the old vertex-rate normal interpolated across ~18 m cells and
// flattened every sub-texel facet; sampling the gradient per fragment
// keeps relief crisp down to the texel, and `bump_fix` adds the
// procedural micro-relief below it.
fn terrain_normal_fix(dir_fix: vec3<f32>, b: Basis) -> vec3<f32> {
    let e = 0.0035;
    let dhe = height_at(normalize(dir_fix + b.e * e))
              - height_at(normalize(dir_fix - b.e * e));
    let dhn = height_at(normalize(dir_fix + b.n * e))
              - height_at(normalize(dir_fix - b.n * e));
    let denom = 2.0 * e * u.planet.x;
    return normalize(dir_fix - b.e * (dhe * u.planet.w / denom)
                     - b.n * (dhn * u.planet.w / denom));
}

// Micro-relief bump: distance-banded noise octaves perturb the macro
// normal. Frequencies are in cells per unit-direction — a cell spans
// ~R/freq metres. Each octave fades with view distance before it can
// sub-pixel shimmer. This is what stops the surface reading as a
// smooth smudge at chase-camera range: the baked heightfield carries
// nothing below ~4 km texels. ALL octaves stay at freq ≤ ~24k so the
// direct `vnoise3w` lattice sample is bit-stable (see STABILITY);
// finer grain comes from the filtered normal-map detail instead.
// The far+mid octaves. These feed BOTH the lighting normal and the
// slope that drives class weights/albedo banding — 65 m hillocks are
// legitimately rocky on their flanks.
fn bump_fix(dir_fix: vec3<f32>, n: vec3<f32>, b: Basis, dist: f32) -> vec3<f32> {
    let R = u.planet.x;
    var g = vec2<f32>(0.0);
    // ~2 km / ~400 m features — visible from low orbit.
    let f_far = clamp(1.0 - dist / 2.4e5, 0.0, 1.0);
    g = g + ngrad(dir_fix, b.e, b.n, 2800.0) * (90.0 * 2800.0 / R * f_far);
    // ~65 m hillocks — fade out by ~60 km altitude.
    let f_mid = clamp(1.0 - dist / 6.0e4, 0.0, 1.0);
    g = g + ngrad(dir_fix, b.e, b.n, 9200.0) * (30.0 * 9200.0 / R * f_mid);
    return normalize(n - b.e * g.x - b.n * g.y);
}

// The fine octave (~40 m cells), applied on top of `bump_fix` for
// LIGHTING only — letting it into the class-weight slope would speckle
// rock/grass assignment at boot level. Metre-scale grain is the normal
// maps' job (filtered, stable); this octave deliberately stops at the
// coarsest "fine" band that stays f32-stable.
fn bump_fix_near(dir_fix: vec3<f32>, n: vec3<f32>, b: Basis, dist: f32) -> vec3<f32> {
    let R = u.planet.x;
    let f_near = clamp(1.0 - dist / 1.5e4, 0.0, 1.0);
    if (f_near <= 0.001) {
        return n;
    }
    let g = ngrad(dir_fix, b.e, b.n, 16000.0) * (5.0 * 16000.0 / R * f_near);
    return normalize(n - b.e * g.x - b.n * g.y);
}

// Cheap terrain self-shadowing: march the sun ray over the heightfield
// in the local-horizontal approximation — `s` metres along the sun's
// projected direction, the ray's clearance grows at the sun-elevation
// slope. Long soft shadows appear automatically at low sun, which is
// where the terminator needs them most.
fn sun_occlusion(dir_fix: vec3<f32>, h_m: f32, sun_fix: vec3<f32>) -> f32 {
    let up = dir_fix;
    let su = dot(sun_fix, up);
    if (su <= 0.035) {
        return 0.0; // sun below the local horizon — fully occluded
    }
    var hs = sun_fix - up * su;
    let hl = length(hs);
    if (hl < 1.0e-4) {
        return 1.0; // sun at the zenith — nothing shadows overhead
    }
    hs = hs / hl;
    let rise = su / hl;
    let R = u.planet.x;
    let amp = u.planet.w;
    var vis = 1.0;
    for (var k = 0; k < 5; k = k + 1) {
        let s = 55.0 * pow(3.1, f32(k));            // 55 m → ~5 km taps
        let pd = normalize(up + hs * (s / R));
        let h_ray = h_m + s * rise;
        let h_t = height_at(pd) * amp;
        let pen = s * 0.08 + 30.0;                  // penumbra widens with distance
        vis = min(vis, clamp((h_ray - h_t + pen) / (2.0 * pen), 0.0, 1.0));
    }
    return vis;
}

// Vessel contact shadow — a Gaussian blob on the surface where the
// chase camera's focus (the craft) casts it. `u.atmo.z` carries the
// craft altitude for the fade; a high craft diffuses into a broad
// haze instead of a crisp disc.
fn blob_shadow(dir: vec3<f32>) -> f32 {
    if (u.shadow.w <= 0.0) {
        return 0.0;
    }
    // Ground distance via atan2(|dir×anchor|, dir·anchor) — at the
    // arc-second scale a craft shadow lives at, acos(1−ε) quantizes
    // to zero in f32; the cross-magnitude stays accurate.
    let d = u.planet.x * atan2(length(cross(dir, u.shadow.xyz)),
                               dot(dir, u.shadow.xyz));
    let rr = u.shadow.w;
    let g = exp(-(d * d) / (rr * rr));
    let fade = exp(-u.atmo.z / 850.0);
    return 0.55 * g * fade;
}

// Fallback palette when a body has no biome map — elevation bands
// (abyss → shelf → beach → grass → dry highland) crossed with a
// slope-driven rock mask and a snowline that sheds off cliffs.
// `dither` (detail noise) raggeds the band edges so they don't read
// as contour lines; `slope` is 1 − n·up (0 flat … ~1 cliff).
fn ramp_albedo(dir: vec3<f32>, h: f32, slope: f32, dither: f32) -> vec3<f32> {
    let hd = h + dither * 0.018;
    var c = mix(vec3<f32>(0.075, 0.085, 0.055), vec3<f32>(0.32, 0.27, 0.14),
                smoothstep(-0.45, -0.02, hd));           // abyss → coastal shelf
    c = mix(c, vec3<f32>(0.60, 0.52, 0.33),              // beach sand
            smoothstep(-0.015, 0.012, hd) * (1.0 - smoothstep(0.035, 0.11, hd)));
    c = mix(c, vec3<f32>(0.21, 0.38, 0.12), smoothstep(0.03, 0.14, hd)); // grass
    c = mix(c, vec3<f32>(0.38, 0.29, 0.16), smoothstep(0.32, 0.55, hd)); // dry highland
    let rock = mix(vec3<f32>(0.27, 0.25, 0.24), vec3<f32>(0.50, 0.47, 0.44),
                   smoothstep(0.0, 0.6, hd));
    c = mix(c, rock, smoothstep(0.14, 0.34, slope + dither * 0.10));
    let snowline = 0.58 - abs(dir.z) * 0.28; // snow reaches lower near the poles
    let snowy = smoothstep(snowline, snowline + 0.10, hd)
              * (1.0 - smoothstep(0.22, 0.48, slope));
    c = mix(c, vec3<f32>(0.88, 0.90, 0.94), snowy);
    return c;
}

// --- real texture detail -------------------------------------------------
// Full three-plane triplanar UVs on the body-fixed sphere. Two
// upgrades over the old dominant+runner-up blend:
//  * the MACRO surface normal picks the |axis|^4 blend weights, so a
//    slope is sampled on the plane that actually faces it — the old
//    position-direction weights foreshortened every hillside into an
//    axially stretched smear;
//  * texture coordinates are ANCHORED: `fract(a·k) + (d−a)·k` equals
//    `d·k` modulo an integer (identical texel under the Repeat
//    sampler) but never carries the ~7e4 magnitude whose f32 ulp is
//    2–4 texels — the raw product jittered every pixel's sample
//    position and broke the implicit derivatives into random mip
//    picks, which was most of the near-ground "blurry smear".
//    The anchor `a` is `dir_anchor(d)` — a fixed world-space grid —
//    NOT the patch centre: a per-patch anchor re-phased every texel
//    field ~cm each time the quadtree re-split (the ground visibly
//    crawled under a moving camera). The quantized anchor is also
//    exact across its own cell borders: a border flip only changes
//    `fract(a·k)`'s complement `floor(a·k)` by an integer, which the
//    Repeat wrap drops entirely.
struct TriPlanes {
    // Per-plane `d − a` projected on each axis-dropped pair
    // (yz for the x plane, xz for y, xy for z).
    dd: array<vec2<f32>, 3>,
    // Per-plane anchor components of `a` (same pairing).
    pb: array<vec2<f32>, 3>,
    // Normalized |n|^4 blend weights.
    w: vec3<f32>,
};

fn tri_planes(d: vec3<f32>, n: vec3<f32>) -> TriPlanes {
    let a = dir_anchor(d);
    let dd = d - a;
    var o: TriPlanes;
    o.dd = array<vec2<f32>, 3>(dd.yz, dd.xz, dd.xy);
    o.pb = array<vec2<f32>, 3>(a.yz, a.xz, a.xy);
    let an = abs(n);
    let a4 = an * an * an * an;
    o.w = a4 / max(a4.x + a4.y + a4.z, 1.0e-9);
    return o;
}

// Anchored texture coordinate on one projection plane (see TriPlanes).
fn tex_uv(dd: vec2<f32>, pb: vec2<f32>, k: f32) -> vec2<f32> {
    return fract(pb * k) + dd * k;
}

// Per-class tile edge length (m) — grass is fine-grained, rock coarse.
fn class_tile(layer: i32) -> f32 {
    if (layer == 0) { return 9.0; }   // grass
    if (layer == 1) { return 22.0; }  // rock
    if (layer == 2) { return 12.0; }  // sand
    return 16.0;                      // snow
}

// One class's normalized albedo detail at `scale` × the base tiling:
// texel ÷ layer mean, weighted across the three projection planes.
// The mean comes from the pack header (u.class_means — the bake's
// exact level-0 average, sRGB→linear), NOT the deepest mip: the
// header row is the single source of truth, and the per-level floor
// bias in the mip chain no longer reaches the shader at all. The mean
// floor has to sit BELOW every layer's actual per-channel mean (the
// rock class averages ~0.01 linear, the grass blue channel ~0.012):
// at the old 0.05 floor the dark layers divided by ~5× their own
// mean, so the pattern's bright veins hit the downstream detail clamp
// as isolated white streaks while the rest of the layer went mud-dark.
fn class_albedo(tp: TriPlanes, layer: i32, scale: f32) -> vec3<f32> {
    let k = u.planet.x / (class_tile(layer) * scale);
    var acc = vec3<f32>(0.0);
    for (var i = 0; i < 3; i = i + 1) {
        acc = acc + textureSample(alb_arr, samp,
                                  tex_uv(tp.dd[i], tp.pb[i], k), layer).rgb
                  * tp.w[i];
    }
    let mean = u.class_means[layer].rgb;
    return acc / max(mean, vec3<f32>(0.008, 0.008, 0.008));
}

// Matching normal-map sample for `layer` — tangent-space xy slope.
fn class_normal(tp: TriPlanes, layer: i32, scale: f32) -> vec2<f32> {
    let k = u.planet.x / (class_tile(layer) * scale);
    var acc = vec2<f32>(0.0);
    for (var i = 0; i < 3; i = i + 1) {
        acc = acc + (textureSample(nrm_arr, samp,
                                   tex_uv(tp.dd[i], tp.pb[i], k), layer).xy
                     * 2.0 - 1.0)
                  * tp.w[i];
    }
    return acc;
}

// Surface-class weights (grass, rock, sand, snow) from height/slope/
// latitude — the same decisions ramp_albedo makes, re-expressed as
// blend weights so neighbouring bands share texture detail instead of
// hard-switching at a threshold.
fn class_weights(h: f32, slope: f32, lat_abs: f32, dither: f32) -> vec4<f32> {
    let hd = h + dither * 0.02;
    // order: x grass, y rock, z sand, w snow — the array layer order
    let snowline = 0.58 - lat_abs * 0.28;
    let snow = smoothstep(snowline, snowline + 0.10, hd)
             * (1.0 - smoothstep(0.22, 0.48, slope));
    let rock = max(smoothstep(0.14, 0.34, slope + dither * 0.10),
                   smoothstep(0.45, 0.62, hd) * 0.7);
    let sand = (1.0 - smoothstep(-0.01, 0.05, hd)) * 0.9;
    let grass = smoothstep(-0.45, -0.02, hd);
    var w = vec4<f32>(grass, rock, sand, snow);
    w = max(w, vec4<f32>(0.0));
    let s = w.x + w.y + w.z + w.w;
    if (s < 1.0e-4) { return vec4<f32>(1.0, 0.0, 0.0, 0.0); }
    return w / s;
}

fn biome_albedo(dir: vec3<f32>, h: f32, slope: f32, dither: f32) -> vec3<f32> {
    var base = ramp_albedo(dir, h, slope, dither);
    if (u.atmo.y > 0.5) {
        let lat = asin(clamp(dir.z, -1.0, 1.0));
        let lon = atan2(dir.y, dir.x);
        let d = vec2<i32>(textureDimensions(biomemap));
        let ii = i32(floor(((lon + PI) / (2.0 * PI)) * f32(d.x)));
        // Decoded PNG row 0 is the north pole (+π/2) — equirect maps
        // scan top→bottom as north→south, the reverse of height_at's
        // south-first v.
        let jj = i32(floor(((0.5 * PI - lat) / PI) * f32(d.y)));
        let slot = textureLoad(biomemap,
            vec2<i32>(clamp(ii, 0, d.x - 1), clamp(jj, 0, d.y - 1)), 0).x;
        if (slot < 255u) {
            base = u.biome_colors[min(slot, 15u)].xyz;
            let hd = h + dither * 0.018;
            // Height and slope still modulate: beaches, rock faces and
            // snow on top of the authored biome tint.
            base = mix(base, vec3<f32>(0.62, 0.55, 0.38),
                       smoothstep(-0.01, 0.02, hd) * (1.0 - smoothstep(0.03, 0.10, hd)));
            let rock = vec3<f32>(0.42, 0.40, 0.38);
            base = mix(base, rock, smoothstep(0.16, 0.38, slope + dither * 0.10));
            let snowline = 0.58 - abs(dir.z) * 0.28;
            base = mix(base, vec3<f32>(0.85, 0.88, 0.92),
                       smoothstep(snowline, snowline + 0.10, hd)
                       * (1.0 - smoothstep(0.22, 0.48, slope)));
        }
    }
    return base;
}

@fragment
fn fs_terrain(i: Vt) -> @location(0) vec4<f32> {
    let pa = u.eye_rel_body.xyz;
    let pb = i.pos_rel_body;
    let seg = pb - pa;
    let dist = max(length(seg), 1.0);
    let ray = seg / dist;
    let sun = u.sun_dir.xyz;
    let has_atmo = u.planet.y > 0.0;
    let dir = normalize(i.dir);
    let dir_fix = normalize(i.dir_fix);
    let sun_fix = rot_axis(u.spin.xyz, -u.spin.w, sun);
    let basis = tangent_basis(dir_fix);
    // Detail fades out long before orbit distances — a planet seen from
    // 300 km shouldn't shimmer with 100 m noise.
    let det_fade = clamp(1.0 - dist / 2.4e5, 0.0, 1.0);

    var col: vec3<f32>;
    var alpha = 1.0;
    if (i.kind > 0.5) {
        // --- ocean ----------------------------------------------------
        // Water depth under the fragment: ocean datum minus the seabed
        // radius — drives the colour, the shoreline foam and the
        // see-through alpha.
        let seabed = u.planet.x + height_at(dir_fix) * u.planet.w;
        let depth = max(u.planet.z - seabed, 0.0);
        // Wave normal: a long swell plus chop octaves over the
        // geometric normal — static (no time uniform), but the
        // perturbed glint alone reads as water. Near the surface the
        // ~40 m chop takes over the shading.
        let wf_near = clamp(1.0 - dist / 2.0e4, 0.0, 1.0);
        var wg = ngrad(dir_fix, basis.e, basis.n, 1200.0) * (4.5 * 1200.0 / u.planet.x)
               + ngrad(dir_fix, basis.e, basis.n, 6400.0) * (1.8 * 6400.0 / u.planet.x);
        wg = wg * (0.4 + 0.6 * det_fade)
           + ngrad(dir_fix, basis.e, basis.n, 14000.0) * (0.9 * 14000.0 / u.planet.x)
             * wf_near;
        if (u.art.x > 0.5) {
            // Real wave normal map (Foam 002, ambientCG CC0) at two
            // scrolled scales — chop + swell — over the noise swell.
            // Scroll rides the sim-tick clock, so it stays deterministic.
            // Anchored UVs (see tex_uv): the raw `d·R/11` product ran
            // ~5e4 — its f32 ulp was a whole texel of jitter.
            let tpw = tri_planes(dir_fix, dir_fix);
            let t = u.art.y;
            let k1 = u.planet.x / 37.0;
            let k2 = u.planet.x / 11.0;
            var w12 = vec2<f32>(0.0);
            for (var pi = 0; pi < 3; pi = pi + 1) {
                w12 = w12 + (textureSample(water_nrm, samp,
                        tex_uv(tpw.dd[pi], tpw.pb[pi], k1)
                        + vec2<f32>(t * 0.011, t * 0.007)).xy * 2.0 - 1.0)
                      * tpw.w[pi];
            }
            var di = 0;
            if (tpw.w[1] > tpw.w[di]) { di = 1; }
            if (tpw.w[2] > tpw.w[di]) { di = 2; }
            let w2 = textureSample(water_nrm, samp,
                        tex_uv(tpw.dd[di], tpw.pb[di], k2)
                        + vec2<f32>(-t * 0.019, t * 0.013)).xy * 2.0 - 1.0;
            wg = wg * 0.5 + w12 * 0.85
               + w2 * 0.45 * max(wf_near * 2.0 + 0.3, 0.0);
        }
        let n_fix = normalize(dir_fix - basis.e * wg.x - basis.n * wg.y);
        let n = rot_axis(u.spin.xyz, u.spin.w, n_fix);

        let v = -ray;
        let fres = 0.02 + 0.98 * pow(1.0 - max(dot(n, v), 0.0), 5.0);
        let sun_up = max(dot(dir, sun), 0.0);
        // Depth body colour — teal shelf to near-black basin.
        var water = mix(vec3<f32>(0.045, 0.26, 0.30), vec3<f32>(0.006, 0.042, 0.115),
                        clamp(depth / 60.0, 0.0, 1.0));
        water = mix(vec3<f32>(0.10, 0.32, 0.26), water, smoothstep(0.4, 6.0, depth));
        // Approximate reflected sky: horizon haze at grazing, deep
        // blue at the zenith, a warm forward-scatter lobe sun-side.
        let r = reflect(ray, n);
        let rup = clamp(dot(r, dir), 0.0, 1.0);
        let rsun = max(dot(r, sun), 0.0);
        var skyc = mix(vec3<f32>(0.38, 0.52, 0.74), vec3<f32>(0.045, 0.095, 0.24),
                       pow(rup, 0.6));
        skyc = skyc + vec3<f32>(0.55, 0.38, 0.20) * pow(rsun, 5.0);
        if (!has_atmo) {
            skyc = vec3<f32>(0.015, 0.02, 0.05);
        }
        // Sun glint: a tight sparkle lobe (the wave noise makes it
        // glitter) plus a broad sheen — both killed by terrain/vessel
        // shadow and the atmosphere's sun transmittance.
        var sun_lum = vec3<f32>(1.0, 0.96, 0.86);
        if (has_atmo) {
            let alt_b = max(length(pb) - u.planet.x, 0.0);
            sun_lum = sun_lum * trans(alt_b, dot(sun, dir));
        }
        let occ = sun_occlusion(dir_fix, u.planet.z - u.planet.x, sun_fix);
        let vis = occ * (1.0 - blob_shadow(dir));
        // Sun glitter: the tight sparkle lobe rides the wave-normal
        // map (Foam002's squiggly foam pattern) — at the old 2.6 gain
        // every glint texel clipped to a glowing white worm. Keep the
        // sparkle but at a gain that survives tonemapping.
        let spec = pow(rsun, 700.0) * 1.1 + pow(rsun, 48.0) * 0.14;
        col = mix(water * (0.10 + 0.90 * sun_up * vis),
                  skyc * (0.22 + 0.78 * sun_up), fres)
              + sun_lum * vis * spec;
        // Shoreline foam: a noise-broken band where the seabed meets
        // the surface.
        let foam_n = smoothstep(0.35, 0.85, 0.5 + 0.5 * vnoise3w(dir_fix, 5400.0));
        let foam = (1.0 - smoothstep(0.2, 2.6, depth)) * foam_n;
        col = mix(col, vec3<f32>(0.80, 0.85, 0.87) * (0.10 + 0.90 * sun_up * vis),
                  clamp(foam, 0.0, 1.0) * 0.75);
        // Shallow water goes translucent — the drawn seabed shows
        // through at the shoreline.
        alpha = clamp(0.62 + 0.38 * (depth / 25.0), 0.62, 1.0);
    } else {
        // --- terrain --------------------------------------------------
        let n_macro = terrain_normal_fix(dir_fix, basis);
        // Class/albedo slope comes from the macro + far/mid relief
        // only — the fine bump octave and the texture normals
        // below would speckle the rock/grass weights per-pixel.
        var n_fix = bump_fix(dir_fix, n_macro, basis, dist);
        let slope = 1.0 - clamp(dot(n_fix, dir_fix), 0.0, 1.0);
        // Albedo grain — the same near-field banding as the bump:
        // large patches mottle, close-ups get a finer octave. Every
        // freq stays ≤ ~24k so the direct lattice sample is bit-stable
        // (see STABILITY) — finer speckle lives in the filtered detail
        // textures, not in point-sampled hash noise.
        let f_near = clamp(1.0 - dist / 1.5e4, 0.0, 1.0);
        let mottle = vnoise3w(dir_fix, 1500.0) * 0.5
                   + vnoise3w(dir_fix, 5300.0) * 0.3
                   + vnoise3w(dir_fix, 15000.0) * 0.2 * max(f_near, 0.15);
        var alb = biome_albedo(dir_fix, i.h_norm, slope, mottle);
        // The fine relief octave returns for lighting.
        n_fix = bump_fix_near(dir_fix, n_fix, basis, dist);
        if (u.art.x > 0.5) {
            // Real PBR detail: class-weighted (grass/rock/sand/snow)
            // ambientCG textures normalized to unit mean so the
            // authored biome palette keeps the hue while the texels
            // carry the pattern — plus a real normal map perturbing
            // the shading normal on the dominant class.
            let wts = class_weights(i.h_norm, slope, abs(dir_fix.z), mottle);
            let tp = tri_planes(dir_fix, n_macro);
            // Distance bands: the mid octave (a ~2.4× coarser,
            // incommensurate retiling) decorrelates the visible
            // 10–20 m tile repeat out to aerial range; the near
            // octave (~5.9× finer, metre-scale tiles) adds cm-grain
            // that only exists at boot/rover distance and fades long
            // before it can sub-pixel shimmer. Both multiply in — a
            // product of mean-1 fields keeps the authored brightness.
            let mid_fade = clamp(1.0 - dist / 4.0e4, 0.0, 1.0);
            let near_fade = clamp(1.0 - dist / 1.8e3, 0.0, 1.0);
            var detail = vec3<f32>(0.0);
            var dom_layer = 0;
            var dom_w = 0.0;
            for (var l = 0; l < 4; l = l + 1) {
                let w = wts[l];
                if (w > 0.02) {
                    var det_l = class_albedo(tp, l, 1.0);
                    if (mid_fade > 0.001) {
                        det_l = det_l * mix(vec3<f32>(1.0),
                                            class_albedo(tp, l, 0.41),
                                            0.60 * mid_fade);
                    }
                    if (near_fade > 0.001) {
                        det_l = det_l * mix(vec3<f32>(1.0),
                                            class_albedo(tp, l, 5.9),
                                            0.80 * near_fade);
                    }
                    detail = detail + det_l * w;
                    if (w > dom_w) { dom_w = w; dom_layer = l; }
                }
            }
            // Detail modulates the authored albedo — bounded so the
            // texture's bright veins/ripples can't multiply it into
            // clipping (the old 3.0 ceiling let isolated texels blow
            // out as white worm streaks while the layer mean sat
            // near 1).
            let det_str = clamp(0.25 + 0.75 * det_fade, 0.0, 1.0);
            alb = alb * mix(vec3<f32>(1.0), clamp(detail, vec3<f32>(0.22), vec3<f32>(1.6)), det_str);
            // Detail normals: the base map plus two finer retilings —
            // ~1.5–3.5 m and ~0.4–1.4 m grain — so the ground reads
            // as rough rock/soil at boot level instead of a linear-
            // filtered smear. Gains rise close in and die with range.
            var ntex = class_normal(tp, dom_layer, 1.0) * (0.35 + 0.65 * det_fade);
            if (near_fade > 0.001) {
                ntex = ntex + class_normal(tp, dom_layer, 5.9) * (0.85 * near_fade)
                            + class_normal(tp, dom_layer, 15.7) * (0.55 * near_fade * near_fade);
            }
            n_fix = normalize(n_fix - basis.e * ntex.x * 0.62 - basis.n * ntex.y * 0.62);
        }
        let n = rot_axis(u.spin.xyz, u.spin.w, n_fix);
        alb = alb * (1.0 + (0.09 * mottle + 0.10 * vnoise3w(dir_fix, 16000.0) * f_near));
        // Sun dimmed by the atmosphere at the fragment's altitude.
        var sun_lum = vec3<f32>(1.0, 0.98, 0.92);
        if (has_atmo) {
            let alt_b = max(length(pb) - u.planet.x, 0.0);
            sun_lum = sun_lum * trans(alt_b, dot(sun, dir));
        }
        let lam = max(dot(n, sun), 0.0);
        let occ = sun_occlusion(dir_fix, i.h_norm * u.planet.w, sun_fix);
        let vis = occ * (1.0 - blob_shadow(dir));
        // Hemisphere ambient — cool sky fill on day, a faint airglow
        // floor on the night side so the terminator never goes dead
        // black, both weighted by how up-facing the surface is.
        let dayness = clamp(dot(dir, sun) * 0.5 + 0.5, 0.0, 1.0);
        var amb = mix(vec3<f32>(0.020, 0.026, 0.045), vec3<f32>(0.16, 0.20, 0.28),
                      dayness);
        amb = amb * (0.55 + 0.45 * max(dot(n, dir), 0.0));
        if (!has_atmo) {
            amb = vec3<f32>(0.016, 0.018, 0.030);
        }
        col = alb * (amb + lam * vis * sun_lum);
    }

    if (has_atmo) {
        let R = u.planet.x;
        let alt_a = max(length(pa) - R, 0.0);
        let alt_b = max(length(pb) - R, 0.0);
        let mua = dot(ray, pa / max(length(pa), 1.0));
        let mub = dot(ray, pb / max(length(pb), 1.0));
        let t_view = trans_between(alt_a, alt_b, mua, mub);
        col = col * t_view + view_inscatter(pa, pb, ray, dist, i.clip.xy);
    }
    return vec4<f32>(tone_map(col), alpha);
}

// --- sky pass: fullscreen triangle, per-pixel scattering ray ----------

struct Vs {
    @builtin(position) clip: vec4<f32>,
    @location(0) ndc: vec2<f32>,
};

@vertex
fn vs_sky(@builtin(vertex_index) vi: u32) -> Vs {
    var o: Vs;
    let p = array<vec2<f32>, 3>(
        vec2<f32>(-1.0, -1.0),
        vec2<f32>(3.0, -1.0),
        vec2<f32>(-1.0, 3.0),
    );
    o.ndc = p[vi];
    o.clip = vec4<f32>(p[vi], 0.0, 1.0); // z = 0: reverse-Z far plane
    return o;
}

// Procedural starfield — crisp POINT stars hashed out of a 3D shell
// lattice in direction space. `rd` is an inertial direction, so the
// field is a real sky (not screenspace noise, and not an equirect
// texture: density is uniform at every latitude, poles included, and
// nothing can smear — there is no image to blur). Each occupied cell
// holds one star at a stable interior offset; the ray's distance to it
// falls off as a sub-cell Gaussian, so stars land as soft luminous
// points with a small halo. The atmosphere dims them through the view
// ray's transmittance; the planet occludes them on a ground strike.
//
// Density stays deliberately SPARSE — ~2k stars all-sky, not the
// tens of thousands a finer lattice resolves as film grain (the old
// 11–23%-occupancy faint layer was the "grainy static" sky). And no
// star may ever go sub-pixel: `sig_ang` is floored at the per-pixel
// ray footprint, so the dimmest stars read as soft dots instead of
// aliased 1-px specks.
//
// One star's contribution: `rd` the view ray, `s` the shell scale,
// `ci` a lattice cell near the ray. The star's position is hashed
// inside the cell, then PROJECTED onto the direction sphere — the
// distance measured is the small-angle ray↔star offset (in cell
// units), so a star anywhere in the cell's depth range shows at full
// brightness (a 3D-position Gaussian would hide ~85% of stars off the
// shell). `sig` is the Gaussian sigma in cell units — floored by the
// caller so the core is ≥ ~1 px at any resolution.
fn star_cell(rd: vec3<f32>, s: f32, ci: vec3<i32>, gain: f32, sig: f32) -> vec3<f32> {
    // |off| < 0.5 → the star stays inside its own cell.
    let off = (vec3<f32>(
        hash_i3(ci.x + 11, ci.y + 5, ci.z - 7),
        hash_i3(ci.x - 3, ci.y + 17, ci.z + 9),
        hash_i3(ci.x + 5, ci.y - 13, ci.z + 2),
    ) - 0.5) * 0.7;
    let star_dir = normalize(vec3<f32>(ci) + 0.5 + off);
    let d = (star_dir - rd) * s;
    let d2 = dot(d, d);
    // Magnitude: bent hash — most stars sit near/below the visibility
    // floor (a real sky is a handful of bright stars over a sparse dim
    // field, not a uniform sheet of equal dots), a few burn bright.
    let mag = hash_i3(ci.x - 7, ci.y + 23, ci.z + 13);
    let bright = 0.05 + 1.6 * pow(mag, 3.5);
    // Blue-white ↔ warm spread (B/A through K/M colours).
    let t = hash_i3(ci.x + 19, ci.y - 11, ci.z + 3);
    let tint = mix(vec3<f32>(0.70, 0.80, 1.0), vec3<f32>(1.0, 0.82, 0.62), t);
    // Soft Gaussian core + a whisper of ~2σ halo — a luminous point,
    // not a 1-px delta and not a glow ball. The caller's 3×3×3 cell
    // walk keeps the halo whole at cell borders (a star sits ≤0.35
    // cell from its own centre).
    let inv = 1.0 / (2.0 * sig * sig);
    let core = exp(-d2 * inv);
    let halo = exp(-d2 * inv * 0.25) * 0.10;
    return tint * ((core + halo) * bright * gain);
}

// One sparse layer: the ray's 3×3×3 cell neighbourhood, each cell
// holding a star with probability `occ`. `occ` may vary per pixel
// (the Milky Way band feeds it) — the field stays deterministic.
fn star_layer(
    rd: vec3<f32>,
    s: f32,
    p: vec3<f32>,
    occ: f32,
    gain: f32,
    sig_ang: f32,
) -> vec3<f32> {
    let base = vec3<i32>(floor(p));
    let sig = sig_ang * s;
    var acc = vec3<f32>(0.0);
    for (var dz = -1; dz <= 1; dz = dz + 1) {
        for (var dy = -1; dy <= 1; dy = dy + 1) {
            for (var dx = -1; dx <= 1; dx = dx + 1) {
                let ci = base + vec3<i32>(dx, dy, dz);
                if (hash_i3(ci.x, ci.y, ci.z) < occ) {
                    acc = acc + star_cell(rd, s, ci, gain, sig);
                }
            }
        }
    }
    return acc;
}

// `pr` = per-pixel ray footprint in radians (computed by the caller
// from the NDC derivative — exact near axis, a slight underestimate
// toward the frame edge) — the resolution-aware floor that keeps every
// star a resolved dot instead of sub-pixel noise.
fn star_field(rd: vec3<f32>, pr: f32) -> vec3<f32> {
    // Bright layer: ~750 stars all-sky at ~1.1° cells. σ ≈ 0.65 px
    // (0.35′ floor) — crisp points with a slight bloom.
    var acc = star_layer(
        rd, 52.0, rd * 52.0,
        0.022, 2.1,
        max(0.65 * pr, 3.5e-4),
    );
    // Milky Way: a fixed great-circle band (galactic pole n) that only
    // thickens the faint layer's density — no additive glow, so the
    // band reads as a star field, not haze.
    let mw_band = exp(-pow(dot(rd, vec3<f32>(0.3586, 0.2054, -0.9106)), 2.0) * 30.0);
    // Faint layer: ~0.7k dimmer stars on ~0.6° cells, ~4× denser
    // inside the band. The lattice is offset so it decorrelates from
    // the bright one. Dim and still resolved (σ ≈ 0.6 px) — sparse
    // resolved points read as stars, dense sub-pixel ones as grain.
    acc = acc + star_layer(
        rd, 96.0, rd * 96.0 + vec3<f32>(0.37, 0.59, 0.83),
        0.006 + 0.018 * mw_band, 0.55,
        max(0.55 * pr, 2.5e-4),
    );
    return acc;
}

@fragment
fn fs_sky(i: Vs) -> @location(0) vec4<f32> {
    let pa = u.eye_rel_body.xyz;
    let sun = u.sun_dir.xyz;
    // View-space ray → body frame (view_rot columns are the view axes).
    let v = vec3<f32>(i.ndc.x * u.misc.x, i.ndc.y * u.misc.y, -1.0);
    let rd = normalize((u.view_rot * vec4<f32>(v, 0.0)).xyz);

    let R = u.planet.x;
    let alt_a = max(length(pa) - R, 0.0);
    let mu_eye = dot(rd, pa / max(length(pa), 1.0));
    let t_bg = trans(alt_a, mu_eye); // 1.0 on vacuum bodies

    // Space: near-black + the sun disc, all attenuated by the
    // atmosphere along the view ray. The starfield stays out of
    // `space_bg` — where the shell march below runs, stars are gated
    // by the inscatter they share the ray with (day side → dark sky
    // only), so they must be addable separately.
    let disc = pow(max(dot(rd, sun), 0.0), 4000.0) * 24.0;
    let halo = pow(max(dot(rd, sun), 0.0), 16.0) * 0.10;
    // The procedural starfield (always — no baked star image exists);
    // sun glare washes stars out toward the disc. `pr` is the
    // per-pixel ray footprint (tan(half-fov)·ndc/px) — the star
    // profile floor.
    let pr = u.misc.y * fwidth(i.ndc.y);
    let stars = star_field(rd, pr)
              * (1.0 - 0.85 * pow(max(dot(rd, sun), 0.0), 12.0));
    let space_bg = vec3<f32>(0.004, 0.006, 0.014)
                 + vec3<f32>(1.0, 0.95, 0.85) * (disc + halo);
    // Vacuum bodies and rays that never cross the shell see the raw
    // night sky — nothing scatters sunlight into them.
    let space = space_bg + stars;

    if (u.planet.y <= 0.0) {
        return vec4<f32>(tone_map(space), 1.0);
    }

    // Ray/shell quadratic |pa + t·rd|² = r² ⇒ t² + 2b·t + c = 0,
    // disc = b² − c. The constant `c = |pa|² − r²` cancels two
    // ~1e11-magnitude terms at planetary radii — computing it in f32
    // would cost ~1e5 m² of absolute error (a ±300 m wobble on the
    // limb), so it arrives pre-baked from the CPU in f64 (u.misc.z/w).
    let b = dot(pa, rd);
    let disc_top = b * b - u.misc.z;
    if (disc_top <= 0.0) {
        // The ray misses the top shell entirely — nothing crosses the
        // atmosphere, so the space backdrop arrives unattenuated (the
        // clamped LUT row would falsely dim the annulus past the limb).
        return vec4<f32>(tone_map(space), 1.0);
    }
    let sq = sqrt(disc_top);
    var t0 = max(-b - sq, 0.0);
    var t1 = -b + sq;
    // Datum strike — the shell march stops where the ray grounds out.
    let disc_g = b * b - u.misc.w;
    var hit_ground = false;
    if (disc_g > 0.0) {
        let tg = -b - sqrt(disc_g);
        if (tg > 0.0 && tg < t1) {
            t1 = tg;
            hit_ground = true;
        }
    }
    var col = space;
    if (t1 > t0) {
        // 20 jittered steps — the per-pixel phase offset breaks the
        // banding a fixed lattice leaves in the thin shell.
        let steps = 20.0;
        let jit = screen_hash(i.ndc);
        let ds = (t1 - t0) / steps;
        let H = max(u.atmo.x, 1.0);
        let Hm = max(H * 0.30, 1.0); // aerosols hug the low shell
        let mu_sun = dot(rd, sun);
        let phase = (3.0 / (16.0 * PI)) * (1.0 + mu_sun * mu_sun);
        let phase_m = mie_phase(mu_sun);
        var acc = vec3<f32>(0.0);
        var acc_m = vec3<f32>(0.0);
        for (var s = 0.0; s < steps; s = s + 1.0) {
            let p = pa + rd * (t0 + (s + 0.05 + jit * 0.9) * ds);
            let pr = length(p);
            let alt_i = max(pr - R, 0.0);
            let up = p / pr;
            let t_ei = trans_between(alt_a, alt_i, mu_eye, dot(rd, up));
            let t_is = trans(alt_i, dot(sun, up));
            acc = acc + u.rayleigh.rgb * phase * exp(-alt_i / H) * t_ei * t_is * ds;
            acc_m = acc_m + MIE_BETA * phase_m * exp(-alt_i / Hm) * t_ei * t_is * ds;
        }
        // Inscatter + the space term attenuated by the view ray's own
        // transmittance; a ground strike swaps the backdrop for haze.
        // The Mie veil is what whitens the limb and warms the glow
        // around the sun instead of a flat blue tint.
        var bg_t = vec3<f32>(0.0);
        if (!hit_ground) {
            bg_t = t_bg;
        }
        let ins = acc * 4.2 + acc_m * 1.5;
        // Daylight gate: stars survive only where the shell scatters
        // essentially nothing into the ray. Day zenith integrates
        // ins_lum ≈ 0.03–0.10 (exp(−160·lum) ≤ ~1%), a bright limb far
        // more; the night side's t_is→0 collapses the gate to 1. The
        // twilight band fades stars in as the sun drops below the
        // samples' horizon — the same term that paints the bright
        // horizon also clears the stars in front of it.
        let ins_lum = dot(ins, vec3<f32>(0.2126, 0.7152, 0.0722));
        let star_vis = exp(-ins_lum * 160.0);
        col = ins + (space_bg + stars * star_vis) * bg_t;
    }
    return vec4<f32>(tone_map(col), 1.0);
}
