//! The seeded procedural terrain heightfield — the ONE source both
//! sides of the surface question share:
//!
//! - **Render** (`csp-render::terrain`, via re-export): the vertex
//!   shader displaces the drawn surface to `radius + h·amplitude`
//!   where `h` is the manual-bilinear sample of the baked equirect
//!   [`HeightField`] — [`HeightField::sample_dir`] is the CPU mirror
//!   of the WGSL `height_at`, texel-exact by construction.
//! - **Sim** (`csp-phys`'s `TerrainSurface`/`Environment`): the
//!   contact surface is `radius_m + sample_dir(dir_fixed)·amplitude`
//!   evaluated on the same baked field, so the ground a vessel rests
//!   on is bit-identical to the ground the player sees. (Before this
//!   shared source existed the contact surface was the bare datum
//!   sphere while the render displaced ±amplitude — a resting craft
//!   visually sank below the drawn terrain.)
//!
//! Determinism: heights are a pure function of `(seed, direction)` —
//! integer hashing plus `f64` add/mul for the noise itself, `libm`
//! for the bake's spherical trig — so the field is bit-stable across
//! platforms and across the sim/render boundary. The bake dims and
//! the amplitude fraction live here too: the sim bake must be the
//! *same* field the render displaces, not a lookalike.

use crate::DVec3;

/// Heightfield bake dimensions — equirect texels per row. 1024×512 ≈
/// 3.7 km texels at Terrin's 600 km radius: the low-frequency surface
/// the R2 quadtree displaces and the surface `csp-phys` contacts.
pub const BAKE_WIDTH: u32 = 1024;
/// Rows (latitude) of the baked field.
pub const BAKE_HEIGHT: u32 = 512;

/// Terrain amplitude as a fraction of the body radius — ~2.4 km of
/// relief at Terrin, proportionally less on moons. Single source for
/// the render's `amplitude_m` and the sim's contact amplitude.
pub const AMPLITUDE_FRAC: f64 = 0.004;

/// splitmix64-style avalanche finalizer — bijective, deterministic.
/// Exported for the renderer's content-hash keys (`BiomeField::key`
/// mixes identically); not part of the height contract.
#[doc(hidden)]
pub fn mix64(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// One lattice corner's value-noise sample ∈ `[-1, 1)`.
fn lattice(seed: u64, x: i64, y: i64, z: i64) -> f64 {
    let h = mix64(
        seed ^ mix64((x as u64).wrapping_add(mix64((y as u64).wrapping_add(mix64(z as u64))))),
    );
    // Top 53 bits → [0,1) → [-1,1).
    (h >> 11) as f64 * (1.0 / 9_007_199_254_740_992.0) * 2.0 - 1.0
}

/// The lattice interpolant and the height compositor's shared curve.
fn smooth(t: f64) -> f64 {
    t * t * (3.0 - 2.0 * t)
}

/// Trilinear value noise at `p` (any scale — the caller multiplies by
/// frequency). Output ∈ `[-1, 1)`.
fn value_noise3(seed: u64, p: DVec3) -> f64 {
    let (fx, fy, fz) = (p.x.floor(), p.y.floor(), p.z.floor());
    let (ix, iy, iz) = (fx as i64, fy as i64, fz as i64);
    let (tx, ty, tz) = (smooth(p.x - fx), smooth(p.y - fy), smooth(p.z - fz));
    let lerp = |a: f64, b: f64, t: f64| a + (b - a) * t;
    let c = |dx: i64, dy: i64, dz: i64| lattice(seed, ix + dx, iy + dy, iz + dz);
    lerp(
        lerp(
            lerp(c(0, 0, 0), c(1, 0, 0), tx),
            lerp(c(0, 1, 0), c(1, 1, 0), tx),
            ty,
        ),
        lerp(
            lerp(c(0, 0, 1), c(1, 0, 1), tx),
            lerp(c(0, 1, 1), c(1, 1, 1), tx),
            ty,
        ),
        tz,
    )
}

/// Fractal Brownian motion over [`value_noise3`]: `octaves` layers,
/// frequency ×2, amplitude ×0.5 per layer.
fn fbm3(seed: u64, p: DVec3, octaves: u32) -> f64 {
    let (mut sum, mut amp, mut q) = (0.0, 0.5, p);
    for _ in 0..octaves {
        sum += amp * value_noise3(seed, q);
        amp *= 0.5;
        q = q * 2.0;
    }
    sum
}

fn smoothstep(e0: f64, e1: f64, x: f64) -> f64 {
    smooth(((x - e0) / (e1 - e0)).clamp(0.0, 1.0))
}

/// The R2 height function: normalized terrain height `∈ [-1, 1]` for a
/// unit-sphere direction (multiply by the body's amplitude for metres).
/// Continents from low-frequency FBM, ridged mountains only on land,
/// small hills everywhere — roughly 40% of the sphere lands below the
/// `0` datum so `OceanDef` bodies get real seas.
///
/// Deterministic: same `(seed, dir)` ⇒ bit-identical output on every
/// platform (integer hashing + f64 arithmetic). Biomes are explicitly
/// NOT derived from this (§3.0.1).
pub fn height_normalized(seed: u64, dir: DVec3) -> f64 {
    let continents = fbm3(seed ^ 0x9e37_79b9_7f4a_7c15, dir * 3.0, 4);
    let hills = fbm3(seed ^ 0xc2b2_ae35_d27d_4ebf, dir * 18.0, 5);
    let ridged = {
        let n = fbm3(seed ^ 0x1656_67b1_9e37_79f9, dir * 7.0, 4);
        let r = 1.0 - 2.0 * n.abs();
        r * r // ~[0,1], crested
    };
    let land = smoothstep(-0.05, 0.35, continents);
    (0.62 * continents + 0.10 * hills + land * (0.35 * ridged - 0.12)).clamp(-1.0, 1.0)
}

/// Baked normalized heights, equirect projection: texel `(i, j)` covers
/// longitude `−π + 2π(i+½)/w`, latitude `−π/2 + π(j+½)/h` (north = +Z,
/// the §3.0 tilt-free convention `body_spin` also uses). The vertex
/// shader samples this with manual bilinear; [`Self::sample_dir`] is the
/// CPU mirror of the same texel math (ocean-patch culling, the chase
/// camera's ground floor, and the physics contact surface).
#[derive(Clone, Debug)]
pub struct HeightField {
    /// Texels per row.
    pub width: u32,
    /// Rows (latitude).
    pub height: u32,
    /// Normalized `[-1,1]` heights, row-major.
    pub data: Vec<f32>,
    /// Bake identity — the renderer re-uploads only on change.
    pub key: u64,
}

impl HeightField {
    /// Bake `w×h` texels from [`height_normalized`] — a one-time cost
    /// per body (the view caches the `Arc` per dominant body; the
    /// physics backend lazily bakes its own copy behind `OnceCell`).
    /// The sim's contact surface bakes at [`BAKE_WIDTH`]×[`BAKE_HEIGHT`]
    /// — the exact field the shader samples.
    pub fn bake(seed: u64, width: u32, height: u32) -> Self {
        let (w, h) = (width as usize, height as usize);
        let mut data = Vec::with_capacity(w * h);
        for j in 0..h {
            let lat =
                -core::f64::consts::FRAC_PI_2 + core::f64::consts::PI * (j as f64 + 0.5) / h as f64;
            let (slat, clat) = (libm::sin(lat), libm::cos(lat));
            for i in 0..w {
                let lon = -core::f64::consts::PI
                    + 2.0 * core::f64::consts::PI * (i as f64 + 0.5) / w as f64;
                let (slon, clon) = (libm::sin(lon), libm::cos(lon));
                let dir = DVec3::new(clat * clon, clat * slon, slat);
                data.push(height_normalized(seed, dir) as f32);
            }
        }
        Self {
            width,
            height,
            data,
            key: Self::bake_key(seed, width, height),
        }
    }

    /// The renderer's height-texture change key for a real bake at
    /// `(seed, w, h)` — `mix64(seed) ^ dims` shifted left one bit so bit
    /// 0 carries the stand-in tag: `0` here (a real bake), `1` for the
    /// view's flat stand-in ([`Self::flat_key`]). The tag bit makes the
    /// two namespaces *provably* disjoint (C01-F3): `mix64` is
    /// bijective, so ANY same-space derivation collides with exactly
    /// one real seed — the earlier `mix64(!seed)` stand-in did so at
    /// `!seed` itself, i.e. two bodies with complementary seeds, and
    /// the renderer's `uploaded_height_key` skip then bound one body's
    /// real field to the other's stand-in.
    pub fn bake_key(seed: u64, width: u32, height: u32) -> u64 {
        (mix64(seed) ^ (u64::from(width) << 32 | u64::from(height))) << 1
    }

    /// [`Self::bake_key`]'s tagged twin — the flat stand-in's key with
    /// bit 0 set, so it can never equal any real bake's key regardless
    /// of seed. The seed still mixes in, keeping stand-ins for
    /// different bodies distinct from each other.
    pub fn flat_key(seed: u64, width: u32, height: u32) -> u64 {
        Self::bake_key(seed, width, height) | 1
    }

    /// Texel value (longitude wraps, latitude clamps — the equirect
    /// edge rules, mirrored in WGSL).
    fn texel(&self, i: i64, j: i64) -> f64 {
        let i = i.rem_euclid(self.width as i64);
        let j = j.clamp(0, self.height as i64 - 1);
        f64::from(self.data[j as usize * self.width as usize + i as usize])
    }

    /// Bilinear sample at a unit-sphere direction — the exact mirror of
    /// the WGSL `height_at` (texel-centre coordinates, wrap-u/clamp-v).
    /// Returns the *normalized* height; callers scale by amplitude.
    /// `dir` is BODY-FIXED: inertial directions conjugate by
    /// `R(axis, −ω·t)` before sampling (the renderer's `unspin`, the
    /// sim's contact conjugate — same rotation).
    pub fn sample_dir(&self, dir: DVec3) -> f64 {
        let lat = libm::asin(dir.z.clamp(-1.0, 1.0));
        let lon = libm::atan2(dir.y, dir.x);
        let w = self.width as f64;
        let h = self.height as f64;
        let u = (lon + core::f64::consts::PI) / (2.0 * core::f64::consts::PI) * w - 0.5;
        let v = (lat + core::f64::consts::FRAC_PI_2) / core::f64::consts::PI * h - 0.5;
        let (u0, v0) = (u.floor() as i64, v.floor() as i64);
        let (fu, fv) = (u - u0 as f64, v - v0 as f64);
        let a = self.texel(u0, v0);
        let b = self.texel(u0 + 1, v0);
        let c = self.texel(u0, v0 + 1);
        let d = self.texel(u0 + 1, v0 + 1);
        a + (b - a) * fu + (c - a) * fv + (a - b - c + d) * fu * fv
    }
}
