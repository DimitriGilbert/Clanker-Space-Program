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
    atmo: vec4<f32>,          // x scale_height, y has_biome, z/w unused
    misc: vec4<f32>,          // x tan(fov_y/2)·aspect, y tan(fov_y/2),
                              // z/w fs_sky shell constants |pa|²−r² (CPU f64 bake)
    spin: vec4<f32>,          // xyz unit spin axis, w phase ω·t (inertial → fixed)
    rayleigh: vec4<f32>,      // Rayleigh β rgb (1/m)
    biome_colors: array<vec4<f32>, 16>,
};

@group(0) @binding(0) var<uniform> u: U;
@group(0) @binding(1) var heightmap: texture_2d<f32>;   // R32Float equirect
@group(0) @binding(2) var tlut: texture_2d<f32>;        // RGBA32Float T(h, μ)
@group(0) @binding(3) var biomemap: texture_2d<u32>;    // R8Uint slots

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

// Analytic single-scatter along the eye→frag segment: N midpoint
// samples of  β·P_r(μ_sun)·T(eye→s)·T(s→sun)·ρ(s)  (the §5.1 Baseline
// "analytic single-scatter" — no inscatter LUT).
fn view_inscatter(pa: vec3<f32>, pb: vec3<f32>, ray: vec3<f32>, dist: f32) -> vec3<f32> {
    let R = u.planet.x;
    let H = max(u.atmo.x, 1.0);
    let mu_sun = dot(ray, u.sun_dir.xyz);
    let phase = (3.0 / (16.0 * PI)) * (1.0 + mu_sun * mu_sun);
    let alt_a = max(length(pa) - R, 0.0);
    let mua = dot(ray, pa / max(length(pa), 1.0));
    var acc = vec3<f32>(0.0);
    let steps = 4.0;
    let ds = dist / steps;
    for (var i = 0.0; i < steps; i = i + 1.0) {
        let p = pa + ray * (dist * (i + 0.5) / steps);
        let alt_i = max(length(p) - R, 0.0);
        let up = p / max(length(p), 1.0);
        let t_ei = trans_between(alt_a, alt_i, mua, dot(ray, up));
        let t_is = trans(alt_i, dot(u.sun_dir.xyz, up));
        acc = acc + u.rayleigh.rgb * phase * exp(-alt_i / H) * t_ei * t_is * ds;
    }
    return acc;
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
    @location(1) nrm: vec3<f32>,
    @location(2) pos_rel_body: vec3<f32>,
    @location(3) h_norm: f32,
    @location(4) @interpolate(flat) kind: f32,
    @location(5) dir_fix: vec3<f32>,
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

    // Surface normal from the heightfield gradient (terrain only) —
    // built in the fixed frame around `dir_fix`, then rotated back to
    // inertial for the (inertial-frame) sun lighting.
    var nrm = dir;
    if (!ocean) {
        var east = cross(vec3<f32>(0.0, 0.0, 1.0), dir_fix);
        if (dot(east, east) < 1.0e-6) {
            east = vec3<f32>(1.0, 0.0, 0.0);
        } else {
            east = normalize(east);
        }
        let north = cross(dir_fix, east);
        let e = 0.004;
        let dhe = height_at(normalize(dir_fix + east * e))
                  - height_at(normalize(dir_fix - east * e));
        let dhn = height_at(normalize(dir_fix + north * e))
                  - height_at(normalize(dir_fix - north * e));
        let denom = 2.0 * e * radius;
        let nrm_fix = normalize(dir_fix - east * (dhe * u.planet.w / denom)
                                - north * (dhn * u.planet.w / denom));
        nrm = rot_axis(u.spin.xyz, u.spin.w, nrm_fix);
    }

    var o: Vt;
    o.clip = u.view_proj * vec4<f32>(wrel, 1.0);
    o.dir = dir;
    o.nrm = nrm;
    o.pos_rel_body = dir * (radius + h);
    o.h_norm = h_norm;
    o.kind = inst.dir0_kind.w;
    o.dir_fix = dir_fix;
    return o;
}

// Fallback palette when a body has no biome map (h = normalized height,
// dir.z = sin(latitude)).
fn ramp_albedo(dir: vec3<f32>, h: f32) -> vec3<f32> {
    var c = mix(vec3<f32>(0.10, 0.09, 0.06), vec3<f32>(0.26, 0.38, 0.15),
                smoothstep(-0.05, 0.05, h)); // sea floor → green
    c = mix(c, vec3<f32>(0.42, 0.33, 0.20), smoothstep(0.12, 0.32, h)); // highland
    c = mix(c, vec3<f32>(0.40, 0.38, 0.36), smoothstep(0.45, 0.65, h)); // rock
    let snowline = 0.72 - abs(dir.z) * 0.30; // snow reaches lower near the poles
    c = mix(c, vec3<f32>(0.85, 0.88, 0.92), smoothstep(snowline, snowline + 0.12, h));
    return c;
}

fn biome_albedo(dir: vec3<f32>, h: f32) -> vec3<f32> {
    var base = ramp_albedo(dir, h);
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
            // Height still modulates: beaches and snow on top of the
            // authored biome tint.
            base = mix(base, vec3<f32>(0.62, 0.55, 0.38),
                       smoothstep(0.0, 0.03, h) * (1.0 - smoothstep(0.03, 0.10, h)));
            let snowline = 0.72 - abs(dir.z) * 0.30;
            base = mix(base, vec3<f32>(0.85, 0.88, 0.92),
                       smoothstep(snowline, snowline + 0.10, h));
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

    var col: vec3<f32>;
    var alpha = 1.0;
    if (i.kind > 0.5) {
        // Ocean: flat datum surface — fresnel toward the sky tint,
        // sun glint on the sphere normal.
        let v = -ray;
        let n = i.dir;
        let fres = 0.03 + 0.97 * pow(1.0 - max(dot(n, v), 0.0), 5.0);
        let sun_up = max(dot(n, sun), 0.0);
        let spec = pow(max(dot(reflect(-v, n), sun), 0.0), 220.0) * sun_up;
        let water = vec3<f32>(0.012, 0.045, 0.10) * (0.25 + 0.75 * sun_up);
        let skyc = vec3<f32>(0.30, 0.44, 0.66) * (0.3 + 0.7 * sun_up);
        col = mix(water, skyc, fres) + vec3<f32>(1.0, 0.95, 0.8) * spec;
        alpha = 1.0;
    } else {
        var alb = biome_albedo(i.dir_fix, i.h_norm);
        // Sun dimmed by the atmosphere at the fragment's altitude.
        var sun_lum = vec3<f32>(1.0, 0.98, 0.92);
        if (has_atmo) {
            let alt_b = max(length(pb) - u.planet.x, 0.0);
            sun_lum = sun_lum * trans(alt_b, dot(sun, i.dir));
        }
        let lam = max(dot(i.nrm, sun), 0.0);
        // A faint cool ambient floor keeps the night side readable —
        // starlight/airglow rather than a hard 0 that reads as a
        // rendering bug (the unlit terminator side was featureless
        // black at 0.08·albedo).
        col = alb * (vec3<f32>(0.10, 0.12, 0.17) + lam * sun_lum);
    }

    if (has_atmo) {
        let R = u.planet.x;
        let alt_a = max(length(pa) - R, 0.0);
        let alt_b = max(length(pb) - R, 0.0);
        let mua = dot(ray, pa / max(length(pa), 1.0));
        let mub = dot(ray, pb / max(length(pb), 1.0));
        let t_view = trans_between(alt_a, alt_b, mua, mub);
        let ins = view_inscatter(pa, pb, ray, dist);
        col = col * t_view + ins * 4.0;
    }
    return vec4<f32>(col, alpha);
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

    // Space: near-black + the sun disc, both attenuated by the
    // atmosphere along the view ray.
    let disc = pow(max(dot(rd, sun), 0.0), 4000.0) * 24.0;
    let halo = pow(max(dot(rd, sun), 0.0), 16.0) * 0.10;
    var space = vec3<f32>(0.004, 0.006, 0.014) + vec3<f32>(1.0, 0.95, 0.85) * (disc + halo);

    if (u.planet.y <= 0.0) {
        return vec4<f32>(space, 1.0);
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
        return vec4<f32>(space, 1.0);
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
        let steps = 16.0;
        let ds = (t1 - t0) / steps;
        let H = max(u.atmo.x, 1.0);
        let mu_sun = dot(rd, sun);
        let phase = (3.0 / (16.0 * PI)) * (1.0 + mu_sun * mu_sun);
        var acc = vec3<f32>(0.0);
        for (var s = 0.0; s < steps; s = s + 1.0) {
            let p = pa + rd * (t0 + (s + 0.5) * ds);
            let pr = length(p);
            let alt_i = max(pr - R, 0.0);
            let up = p / pr;
            let t_ei = trans_between(alt_a, alt_i, mu_eye, dot(rd, up));
            let t_is = trans(alt_i, dot(sun, up));
            acc = acc + u.rayleigh.rgb * phase * exp(-alt_i / H) * t_ei * t_is * ds;
        }
        // Inscatter + the space term attenuated by the view ray's own
        // transmittance; a ground strike swaps the backdrop for haze.
        var bg_t = vec3<f32>(0.0);
        if (!hit_ground) {
            bg_t = t_bg;
        }
        col = acc * 4.0 + space * bg_t;
    }
    return vec4<f32>(col, 1.0);
}
