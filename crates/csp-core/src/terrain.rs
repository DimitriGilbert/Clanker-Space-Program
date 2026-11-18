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
//!
//! Two relief profiles ship (the §5.2-R2 terrain overhaul, M5,
//! steepened by §2.1): [`ReliefProfile::Legacy`] — the R2-launch
//! [`height_normalized`] spectrum every existing body bakes, and the
//! data default — and [`ReliefProfile::Mountain`] — the steep ridged
//! spectrum with valley carving and domain warp, baked at 2048×1024
//! so its extra peak detail survives the texel grid
//! ([`HeightField::bake_profiled`]). The profile is data-authored
//! (`csp_data`'s `TerrainDef`), reaches the bake key through a seed
//! fold ([`HeightField::bake_profiled_key`]), and never touches the
//! sim state hash — heights stay a pure function of
//! `(seed, direction, profile)`.

use serde::{Deserialize, Serialize};

use crate::DVec3;

/// Heightfield bake dimensions — equirect texels per row. 1024×512 ≈
/// 3.7 km texels at Terrin's 600 km radius: the low-frequency surface
/// the R2 quadtree displaces and the surface `csp-phys` contacts.
pub const BAKE_WIDTH: u32 = 1024;
/// Rows (latitude) of the baked field.
pub const BAKE_HEIGHT: u32 = 512;

/// Mountain-profile bake dimensions — twice the legacy grid: the
/// sharper spectrum carries ridge detail that 1024-wide rows would
/// alias into stair-steps (~1.8 km texels at Terrin's radius). Used
/// only by [`HeightField::bake_profiled`] under
/// [`ReliefProfile::Mountain`] (§5.2-R2's terrain overhaul, M5).
pub const MOUNTAIN_BAKE_WIDTH: u32 = 2048;
/// Rows (latitude) of the Mountain-profile bake.
pub const MOUNTAIN_BAKE_HEIGHT: u32 = 1024;

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

/// The §2.1 flat-top lattice interpolant — [`smooth`]'s Mountain-only
/// twin. The cubic peaks at slope 1.5 mid-cell (and the survey's
/// extreme pairs sit exactly there); this one ramps to a slope
/// plateau of `1/(1-A)` ≈ 1.03 instead, cutting each layer's
/// peak-vs-mean gradient ratio by ~31% for near-zero bulk cost.
/// C1 at the cell borders and at the ramp knees; monotone; maps
/// [0,1] → [0,1]. [`smooth`] itself stays bit-frozen for Legacy.
fn smooth_flat(t: f64) -> f64 {
    const A: f64 = 0.03;
    const G1: f64 = 1.0 - A;
    // G(t)/G(1) where G' = w, w = smoothstep(t/A)·smoothstep((1−t)/A):
    // S-curve with derivative bounded by 1/(1−A).
    let g = if t <= A {
        let x = t / A;
        A * (x * x * x - x * x * x * x * 0.5)
    } else if t >= 1.0 - A {
        let x = (1.0 - t) / A;
        G1 - A * (x * x * x - x * x * x * x * 0.5)
    } else {
        0.5 * A + (t - A)
    };
    g / G1
}

/// [`value_noise3`]'s Mountain-only twin: identical lattice and corner
/// hashing, [`smooth_flat`] as the interpolant (see its doc for why).
fn value_noise3_flat(seed: u64, p: DVec3) -> f64 {
    let (fx, fy, fz) = (p.x.floor(), p.y.floor(), p.z.floor());
    let (ix, iy, iz) = (fx as i64, fy as i64, fz as i64);
    let (tx, ty, tz) = (
        smooth_flat(p.x - fx),
        smooth_flat(p.y - fy),
        smooth_flat(p.z - fz),
    );
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

/// [`fbm3`]'s gained twin for the Mountain spectrum: frequency still
/// doubles per layer, but each layer's amplitude comes from `gains`
/// (slice length = layer count) instead of the halving ladder — the
/// §2.1 steep-spectrum reweighting moves energy between the SAME
/// layers instead of adding any (the 16-layer budget holds; legacy
/// keeps the frozen [`fbm3`]). Samples through [`value_noise3_flat`].
fn fbm3_gained(seed: u64, p: DVec3, gains: &[f64]) -> f64 {
    let (mut sum, mut q) = (0.0, p);
    for &g in gains {
        sum += g * value_noise3_flat(seed, q);
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

/// The authored relief spectrum a body bakes — which height function
/// and bake resolution [`HeightField::bake_profiled`] uses (§5.2-R2's
/// terrain overhaul, M5). Data-authored via `csp_data`'s `TerrainDef`
/// (serde default [`Self::Legacy`], so every existing body keeps its
/// authored world bit-for-bit); serde rejects unknown variants at
/// parse time, so the closed set needs no validator re-check.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReliefProfile {
    /// The R2-launch spectrum — the exact [`height_normalized`]
    /// function at [`BAKE_WIDTH`]×[`BAKE_HEIGHT`]. The default, and
    /// the only profile any shipped body authors today.
    #[default]
    Legacy,
    /// The M5 terrain-overhaul spectrum, steepened by §2.1:
    /// co-dominant ridged/detail stacks in the survey's frequency
    /// sandwich, unsquared V-crests, ridge-inversion valley carving,
    /// a low-frequency domain warp, flat-top lattice interpolation,
    /// and a crest guard — baked at
    /// [`MOUNTAIN_BAKE_WIDTH`]×[`MOUNTAIN_BAKE_HEIGHT`]
    /// ([`height_normalized_mountain`]'s doc has the full spectrum).
    Mountain,
}

impl ReliefProfile {
    /// [`Self::Legacy`]'s bake dims — [`BAKE_WIDTH`]×[`BAKE_HEIGHT`],
    /// unchanged from R2.
    pub const LEGACY_BAKE_DIMS: (u32, u32) = (BAKE_WIDTH, BAKE_HEIGHT);
    /// [`Self::Mountain`]'s bake dims —
    /// [`MOUNTAIN_BAKE_WIDTH`]×[`MOUNTAIN_BAKE_HEIGHT`].
    pub const MOUNTAIN_BAKE_DIMS: (u32, u32) = (MOUNTAIN_BAKE_WIDTH, MOUNTAIN_BAKE_HEIGHT);

    /// The equirect dims this profile's [`HeightField`] bakes at.
    pub const fn bake_dims(self) -> (u32, u32) {
        match self {
            Self::Legacy => Self::LEGACY_BAKE_DIMS,
            Self::Mountain => Self::MOUNTAIN_BAKE_DIMS,
        }
    }

    /// The seed-space fold separating profiles in
    /// [`HeightField::bake_profiled_key`]: `0` for [`Self::Legacy`] —
    /// the profiled key at legacy dims IS the historical key, so
    /// migrating a caller from `bake` to `bake_profiled(Legacy)`
    /// never churns a live cache — and a fixed nonzero constant for
    /// [`Self::Mountain`] (ASCII `RELIEF2M`; any nonzero fold separates
    /// the `mix64` preimages).
    pub const fn key_seed_fold(self) -> u64 {
        match self {
            Self::Legacy => 0,
            Self::Mountain => 0x5245_4c49_4546_324d,
        }
    }
}

/// Profile-dispatching twin of [`height_normalized`] (§5.2-R2, M5):
/// [`ReliefProfile::Legacy`] IS the legacy function — dispatched, not
/// re-implemented, so bit-stability is structural — and
/// [`ReliefProfile::Mountain`] is the sharper spectrum below. Same
/// `[-1, 1]` clamp, same determinism class: a pure function of
/// `(seed, dir, profile)` with integer hashing + f64 arithmetic only.
pub fn height_normalized_profile(seed: u64, dir: DVec3, profile: ReliefProfile) -> f64 {
    match profile {
        ReliefProfile::Legacy => height_normalized(seed, dir),
        ReliefProfile::Mountain => height_normalized_mountain(seed, dir),
    }
}

/// The Mountain spectrum — §5.2-R2's terrain overhaul (M5), steepened
/// by the §2.1 relief pass — same construction rules as
/// [`height_normalized`] (integer-hash lattice + f64 arithmetic only;
/// no std transcendentals), tuned so the `csp-phys` land-slope survey
/// at Terrin's radius reads ridge-scale p99 tan ≈ 0.36 / max ≈ 0.74 at
/// `amplitude_frac` 0.006 (the §2.1 steep band) vs the M5 launch's
/// 0.14 / 0.51 — real mountain angles, still macro-gentle (96×48 grid
/// p99 ≈ 0.20). The shape levers, each load-bearing for that band:
///
/// - **Inherited land/ocean mask** (byte-identical): the continents
///   term — same seed constant, frequency ×3.0, 4 octaves, same
///   `smoothstep(-0.05, 0.35, …)` gate — keeps the legacy coastline
///   statistics, so an `OceanDef` body's seas still sit where its
///   continents say they do. Biome maps and ocean geometry do not
///   move.
/// - **Frequency sandwich**: the dominant ridge energy sits at
///   lattice wavelengths whose flanks (~8–20 km) are wider than the
///   survey's ridge-grid pairs (7.4 km — so the bulk reads the full
///   gradient, high p99) but narrower than the macro grid's 39 km
///   pairs (so the macro grid sees amplitude, not gradient — low
///   macro p99). Two stacks share the band: the ridge field at `×5`
///   (5 octaves, per-octave gains in the code) and the detail field
///   at `×13` (2 octaves) — independent seeds, co-dominant, so their
///   steepest lattice events rarely coincide (the max-tan tail).
/// - **Unsquared crests**: `1 − 2|n|` (clamped to `[-0.84, 1]`) keeps
///   V-sharp crests while the slope factor stays a constant 2 —
///   squaring (`r*r`) spreads it over `[0, 4]`, the fat max tail the
///   M5 spectrum capped out on. The asymmetric floor bounds the
///   deepest trough swings (the co-aligned cliff events).
/// - **Valley carving**: a ridge *inversion* at `×6` (2 octaves,
///   squared — rounded basins) subtracted on land.
/// - **Domain warp** (3 single-octave axis fields at `×2`): folds the
///   direction the ridge terms sample — folded strata instead of
///   straight lattice ridges — at 0.10 amplitude: the warp Jacobian
///   multiplies local gradients, so its gain is deliberately small.
/// - **Flat-top interpolation** ([`smooth_flat`]): every Mountain
///   layer interpolates with a slope plateau of ~1.03 instead of the
///   cubic's 1.5 peak — the survey's extreme pairs sit exactly at the
///   interpolant's steepest point, and this one lever moved the max
///   more than any reweighting.
/// - **Crest guard**: unit slope through the working range `|z| ≤
///   0.42`, easing to a 0.585 ceiling by `|z| = 0.75` — whole-range
///   cliff crossings (the max-tan events) compress instead of mesafing
///   at the hard clamp, and peaks top out ~0.59 rather than slamming
///   ±1.
///
/// Octave budget: 4 (continents) + 5 (ridges) + 2 (detail) + 2
/// (valleys) + 3 (warp axes) = **16 noise layers**, the documented
/// ceiling that keeps bake cost bounded; legacy uses 13.
fn height_normalized_mountain(seed: u64, dir: DVec3) -> f64 {
    let continents = fbm3(seed ^ 0x9e37_79b9_7f4a_7c15, dir * 3.0, 4);
    let land = smoothstep(-0.05, 0.35, continents);
    let q = dir
        + DVec3::new(
            value_noise3_flat(seed ^ 0x6a09_e667_f3bc_c908, dir * 2.0),
            value_noise3_flat(seed ^ 0xbb67_ae85_84ca_a73b, dir * 2.0),
            value_noise3_flat(seed ^ 0x3c6e_f372_fe94_f82b, dir * 2.0),
        ) * 0.10;
    let ridged = {
        let n = fbm3_gained(
            seed ^ 0x1656_67b1_9e37_79f9,
            q * 5.0,
            &[1.00, 2.14, 1.05, 0.10, 0.02],
        );
        (1.0 - 2.0 * n.abs()).clamp(-0.84, 1.0)
    };
    let detail = {
        let n = fbm3_gained(seed ^ 0xa54f_f53a_5f1d_36f1, q * 13.0, &[1.59, 0.62]);
        (1.0 - 2.0 * n.abs()).clamp(-0.84, 1.0)
    };
    let valleys = {
        let n = fbm3_gained(seed ^ 0x510e_527f_ade6_82d1, q * 6.0, &[1.00, 0.70]);
        let r = (1.0 - 2.0 * n.abs()).clamp(-1.0, 1.0);
        1.0 - r * r // ridge inversion — the basin between crests
    };
    let z = 0.62 * continents + land * (1.00 * ridged + 1.00 * detail - 0.65 * valleys + 0.29);
    // §2.1 crest guard (see the spectrum doc above): slope exactly 1
    // through |z| ≤ 0.42 — the p99 bulk rides mid-range swings, any
    // knee there would reshape the steep-slope band — easing to the
    // 0.585 ceiling over |z| ∈ [0.42, 0.75].
    let az = z.abs();
    let mag = if az <= 0.42 {
        az
    } else {
        let t = ((az - 0.42) / 0.33).clamp(0.0, 1.0);
        0.42 + 0.33 * (t - t * t * 0.5)
    };
    (if z >= 0.0 { mag } else { -mag }).clamp(-1.0, 1.0)
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
    /// — the exact field the shader samples. Signature and behavior
    /// are frozen: this delegates to [`Self::bake_with_profile`] under
    /// [`ReliefProfile::Legacy`] (whose seed fold is 0, so even the
    /// key is bit-identical to the pre-profile construction).
    pub fn bake(seed: u64, width: u32, height: u32) -> Self {
        Self::bake_with_profile(seed, width, height, ReliefProfile::Legacy)
    }

    /// Bake at the profile's own dims from [`height_normalized_profile`]
    /// (§5.2-R2's terrain overhaul, M5): [`ReliefProfile::Legacy`] →
    /// [`BAKE_WIDTH`]×[`BAKE_HEIGHT`] (bit-identical to [`Self::bake`]
    /// at those dims, key included), [`ReliefProfile::Mountain`] →
    /// [`MOUNTAIN_BAKE_WIDTH`]×[`MOUNTAIN_BAKE_HEIGHT`]. The key comes
    /// from [`Self::bake_profiled_key`], so per-profile caches cannot
    /// share a slot.
    pub fn bake_profiled(seed: u64, profile: ReliefProfile) -> Self {
        let (width, height) = profile.bake_dims();
        Self::bake_with_profile(seed, width, height, profile)
    }

    /// The shared bake loop — both constructors above land here; only
    /// the height function and the key's seed fold differ by profile.
    fn bake_with_profile(seed: u64, width: u32, height: u32, profile: ReliefProfile) -> Self {
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
                data.push(height_normalized_profile(seed, dir, profile) as f32);
            }
        }
        Self {
            width,
            height,
            data,
            key: Self::bake_key(seed ^ profile.key_seed_fold(), width, height),
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

    /// The profile-aware content key (§5.2-R2, M5): [`Self::bake_key`]
    /// over the profile's own dims with [`ReliefProfile::key_seed_fold`]
    /// mixed into the seed *before* `mix64`. Profiles therefore cannot
    /// share a cache slot: a collision needs a seed pair solving
    /// `mix64(s ^ fold) == mix64(s') ^ (dims ^ dims')` while the
    /// per-profile dims also differ — the same bijection-backed
    /// separation the seed↔dims XOR itself rests on ([`Self::bake_key`]'s
    /// doc). [`ReliefProfile::Legacy`] folds to 0, so the profiled key
    /// at legacy dims equals the historical key exactly.
    pub fn bake_profiled_key(seed: u64, profile: ReliefProfile) -> u64 {
        let (width, height) = profile.bake_dims();
        Self::bake_key(seed ^ profile.key_seed_fold(), width, height)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The §5.2-R2/M5 land/ocean-grid sweep shared by the range and
    /// identity checks below (36×72 directions, seed 0 and the probe
    /// seed both exercised where the test cares).
    fn grid_dirs() -> impl Iterator<Item = DVec3> {
        (0..36).flat_map(|j| {
            let lat =
                -core::f64::consts::FRAC_PI_2 + core::f64::consts::PI * (j as f64 + 0.5) / 36.0;
            let (sl, cl) = (libm::sin(lat), libm::cos(lat));
            (0..72).map(move |i| {
                let lon =
                    -core::f64::consts::PI + 2.0 * core::f64::consts::PI * (i as f64 + 0.5) / 72.0;
                DVec3::new(cl * libm::cos(lon), cl * libm::sin(lon), sl)
            })
        })
    }

    /// Legacy bit-stability (§5.2-R2, M5): the recorded f64 bits of
    /// [`height_normalized`] at fixed dirs — the pre-profile function
    /// is untouched — and [`height_normalized_profile`]'s `Legacy`
    /// arm is bit-identical to it across the whole grid, proving the
    /// dispatch adds no arithmetic of its own.
    #[test]
    fn legacy_height_is_bit_stable() {
        let seed = 0x5eed_c0de_1234_5678u64;
        let recorded = [
            (DVec3::new(1.0, 0.0, 0.0), 0xbfa8_f026_c1bb_d98eu64),
            (DVec3::new(0.0, 1.0, 0.0), 0xbfcc_39df_6a3e_91f2u64),
            (DVec3::new(0.0, 0.0, 1.0), 0xbfb0_4e72_850b_73ccu64),
            (DVec3::new(0.6, 0.8, 0.0), 0xbfc4_90fa_0cfd_7928u64),
        ];
        for (dir, bits) in recorded {
            assert_eq!(height_normalized(seed, dir).to_bits(), bits);
        }
        for dir in grid_dirs() {
            assert_eq!(
                height_normalized_profile(seed, dir, ReliefProfile::Legacy).to_bits(),
                height_normalized(seed, dir).to_bits()
            );
        }
    }

    /// Mountain determinism: same `(seed, dir)` twice ⇒ identical bits
    /// — the profile is a pure function with no hidden state, the same
    /// contract legacy carries across platforms and the sim/render
    /// boundary.
    #[test]
    fn mountain_height_is_deterministic() {
        for seed in [0u64, 0x5eed_c0de_1234_5678] {
            for dir in grid_dirs() {
                let a = height_normalized_profile(seed, dir, ReliefProfile::Mountain);
                let b = height_normalized_profile(seed, dir, ReliefProfile::Mountain);
                assert_eq!(a.to_bits(), b.to_bits());
            }
        }
    }

    /// The Mountain spectrum keeps the `[-1, 1]` contract — clamped
    /// output despite the steep §2.1 composite.
    #[test]
    fn mountain_height_stays_clamped() {
        for seed in [0u64, 0x5eed_c0de_1234_5678] {
            for dir in grid_dirs() {
                let h = height_normalized_profile(seed, dir, ReliefProfile::Mountain);
                assert!((-1.0..=1.0).contains(&h), "{h} at {dir:?}");
            }
        }
    }

    /// The Mountain spectrum keeps meaningful oceans and real peaks
    /// (the inherited continents mask): recorded probe dirs — an ocean
    /// sample below the datum and a peak above 0.5, pinned to their
    /// exact bits so the spectrum cannot silently drift. The ocean
    /// probe survived the §2.1 steepening bit-for-bit (the land gate is
    /// closed there, so the frozen continents term is the whole value);
    /// the peak moved onto the §2.1 crest guard's 0.585 ceiling.
    #[test]
    fn mountain_oceans_and_peaks_exist() {
        let ocean_dir = DVec3::new(
            0.115_075_127_486_383_7,
            0.277_815_933_469_440_8,
            -0.953_716_950_748_226_8,
        );
        let ocean_h = height_normalized_profile(0, ocean_dir, ReliefProfile::Mountain);
        assert_eq!(ocean_h.to_bits(), 0xbfcd_b4bf_458c_3333);
        assert!(ocean_h < 0.0);
        let peak_dir = DVec3::new(
            -0.216_233_611_384_725_58,
            -0.009_440_963_361_569_903,
            -0.976_296_007_119_933_4,
        );
        let peak_h = height_normalized_profile(0, peak_dir, ReliefProfile::Mountain);
        assert_eq!(peak_h.to_bits(), 0x3fe2_b851_eb85_1eb8);
        assert!(peak_h > 0.5);
    }

    /// The §2.1 steep-spectrum pin: six recorded probe dirs spanning
    /// the relief classes (two oceans, a coastal cell, a mid-slope,
    /// a sub-crest peak, and the crest-guard ceiling), pinned to
    /// exact bits so the steep tuning cannot silently drift — any
    /// Mountain-spectrum edit that moves a single one of these fails
    /// here, not in the `csp-phys` survey. Recorded at seed 0 after
    /// the §2.1 retune (ridge p99 0.3618 / max 0.7415 @ 0.006).
    #[test]
    fn mountain_steep_spectrum_pin() {
        let pins = [
            // deep ocean (h = −0.3892639776319882)
            (
                DVec3::new(
                    -0.097_312_364_451_771_05,
                    -0.011_733_009_979_987_902,
                    -0.995_184_726_672_196_8,
                ),
                0xbfd8_e9b3_755c_2d4du64,
            ),
            // shallow ocean, the §5.2-R2-era probe (h = −0.23207846542245533)
            (
                DVec3::new(
                    0.115_075_127_486_383_7,
                    0.277_815_933_469_440_8,
                    -0.953_716_950_748_226_8,
                ),
                0xbfcd_b4bf_458c_3333u64,
            ),
            // coastal cell, just above the datum (h = 0.016224170935803597)
            (
                DVec3::new(
                    -0.032_717_136_199_948_58,
                    -0.000_356_902_749_025_676_04,
                    -0.999_464_587_476_365_7,
                ),
                0x3f90_9d11_ae4b_c414u64,
            ),
            // mid-slope (h = 0.24852098285893956)
            (
                DVec3::new(
                    -0.071_963_822_378_108_21,
                    0.025_307_173_584_810_556,
                    -0.997_086_132_304_466_6,
                ),
                0x3fcf_cf89_1adf_dcadu64,
            ),
            // sub-crest peak, inside the crest guard's ease zone
            // (h = 0.5634845810688168)
            (
                DVec3::new(
                    -0.111_084_861_908_433_59,
                    -0.044_599_535_290_780_33,
                    -0.992_809_667_009_055,
                ),
                0x3fe2_0810_d0ef_b4dfu64,
            ),
            // the crest guard's 0.585 ceiling plateau (h = 0.585)
            (
                DVec3::new(
                    -0.216_233_611_384_725_58,
                    -0.009_440_963_361_569_903,
                    -0.976_296_007_119_933_4,
                ),
                0x3fe2_b851_eb85_1eb8u64,
            ),
        ];
        for (dir, bits) in pins {
            assert_eq!(
                height_normalized_profile(0, dir, ReliefProfile::Mountain).to_bits(),
                bits,
                "§2.1 steep-spectrum pin moved at {dir:?}"
            );
        }
    }

    /// Per-profile bake dims (§5.2-R2, M5): Legacy stays 1024×512 —
    /// the frozen R2 grid — and Mountain doubles to 2048×1024.
    #[test]
    fn profile_bake_dims() {
        assert_eq!(ReliefProfile::Legacy.bake_dims(), (BAKE_WIDTH, BAKE_HEIGHT));
        assert_eq!(ReliefProfile::Legacy.bake_dims(), (1024, 512));
        assert_eq!(
            ReliefProfile::Mountain.bake_dims(),
            (MOUNTAIN_BAKE_WIDTH, MOUNTAIN_BAKE_HEIGHT)
        );
        assert_eq!(ReliefProfile::Mountain.bake_dims(), (2048, 1024));
        assert_eq!(
            ReliefProfile::LEGACY_BAKE_DIMS,
            ReliefProfile::Legacy.bake_dims()
        );
        assert_eq!(
            ReliefProfile::MOUNTAIN_BAKE_DIMS,
            ReliefProfile::Mountain.bake_dims()
        );
    }

    /// `bake_profiled(seed, Legacy)` is bit-identical to the frozen
    /// `bake` path at the legacy dims — dims, texels, AND key — so
    /// migrating callers never churn a live renderer cache.
    #[test]
    fn legacy_profiled_bake_matches_bake() {
        let seed = 0x5eed_c0de_1234_5678u64;
        let a = HeightField::bake(seed, BAKE_WIDTH, BAKE_HEIGHT);
        let b = HeightField::bake_profiled(seed, ReliefProfile::Legacy);
        assert_eq!((b.width, b.height, b.key), (a.width, a.height, a.key));
        assert_eq!(b.data, a.data);
    }

    /// Profile-aware keys cannot share a slot (§5.2-R2, M5): the
    /// legacy profiled key IS the historical key (0 fold), the
    /// mountain key differs from it AND from a same-dims un-folded
    /// key — the seed fold matters beyond the dims word — and both
    /// stay in the real-bake namespace (bit 0 clear, the flat-key
    /// tag).
    #[test]
    fn profiled_keys_are_distinct() {
        for seed in [0u64, 1, 0x5eed_c0de_1234_5678] {
            let legacy = HeightField::bake_profiled_key(seed, ReliefProfile::Legacy);
            assert_eq!(legacy, HeightField::bake_key(seed, BAKE_WIDTH, BAKE_HEIGHT));
            let mountain = HeightField::bake_profiled_key(seed, ReliefProfile::Mountain);
            assert_ne!(mountain, legacy);
            let (mw, mh) = ReliefProfile::Mountain.bake_dims();
            assert_ne!(mountain, HeightField::bake_key(seed, mw, mh));
            assert_eq!(legacy & 1, 0);
            assert_eq!(mountain & 1, 0);
        }
    }

    /// Terrin's radius — the survey constant `csp-phys`'s
    /// `mountain_land_slope_distribution` uses (m).
    const SURVEY_R: f64 = 600_000.0;

    /// The §2.1 survey's two-seed bake for one grid: heights are
    /// amplitude-independent, so the grids are baked once and both
    /// authored amplitude fractions read off the same field.
    fn survey_bake(rows: usize, cols: usize) -> Vec<Vec<f64>> {
        let mut grids = Vec::new();
        for seed in [0u64, 601] {
            let mut h = vec![0.0f64; rows * cols];
            for j in 0..rows {
                let lat = -core::f64::consts::FRAC_PI_2
                    + core::f64::consts::PI * (j as f64 + 0.5) / rows as f64;
                let (slat, clat) = (libm::sin(lat), libm::cos(lat));
                for i in 0..cols {
                    let lon = -core::f64::consts::PI
                        + 2.0 * core::f64::consts::PI * (i as f64 + 0.5) / cols as f64;
                    let dir = DVec3::new(clat * libm::cos(lon), clat * libm::sin(lon), slat);
                    h[j * cols + i] = height_normalized_profile(seed, dir, ReliefProfile::Mountain);
                }
            }
            grids.push(h);
        }
        grids
    }

    /// One `(grid, amplitude)` slice of the §2.1 survey report: the
    /// land-pair tans (east pairs wrap longitude, north pairs clamp —
    /// the equirect seam rules), percentiles, exceedance counts, and
    /// the top-10 steepest pairs with their heights — the tail-lever
    /// diagnostics the retune iterated on.
    fn survey_report(rows: usize, cols: usize, grids: &[Vec<f64>], amp_frac: f64) {
        let amplitude_m = SURVEY_R * amp_frac;
        // (tan, seed, j, i, east?) — tracked so the max pair's location
        // and the pair's heights can be read off the printed line.
        let mut tans: Vec<(f64, u64, usize, usize, bool)> = Vec::new();
        for (gi, h) in grids.iter().enumerate() {
            let seed = [0u64, 601][gi];
            for j in 0..rows {
                let lat = -core::f64::consts::FRAC_PI_2
                    + core::f64::consts::PI * (j as f64 + 0.5) / rows as f64;
                let arc_e = SURVEY_R * libm::cos(lat) * 2.0 * core::f64::consts::PI / cols as f64;
                for i in 0..cols {
                    if h[j * cols + i] <= 0.0 {
                        continue; // ocean/coast cell — land survey only
                    }
                    let dh = (h[j * cols + (i + 1) % cols] - h[j * cols + i]).abs();
                    tans.push((dh * amplitude_m / arc_e, seed, j, i, true));
                }
            }
            let arc_n = SURVEY_R * core::f64::consts::PI / rows as f64;
            for j in 0..rows - 1 {
                for i in 0..cols {
                    if h[j * cols + i] <= 0.0 {
                        continue;
                    }
                    let dh = (h[(j + 1) * cols + i] - h[j * cols + i]).abs();
                    tans.push((dh * amplitude_m / arc_n, seed, j, i, false));
                }
            }
        }
        tans.sort_by(|a, b| a.0.total_cmp(&b.0));
        let pick = |frac: f64| tans[((tans.len() - 1) as f64 * frac) as usize];
        let (p50, p95, p99, p999) = (pick(0.50).0, pick(0.95).0, pick(0.99).0, pick(0.999).0);
        let over75 = tans.iter().filter(|t| t.0 > 0.75).count();
        let over50 = tans.iter().filter(|t| t.0 > 0.50).count();
        let (max_tan, mseed, mj, mi, meast) = tans[tans.len() - 1];
        let top10: Vec<String> = tans[tans.len() - 10..]
            .iter()
            .rev()
            .map(|(t, s, j, i, e)| {
                let g = &grids[if *s == 601 { 1 } else { 0 }];
                let (a, b) = if *e {
                    (g[j * cols + i], g[j * cols + (i + 1) % cols])
                } else {
                    (g[j * cols + i], g[(j + 1) * cols + i])
                };
                format!(
                    "{t:.3}/s{s}/j{j}/{}{}/h{a:+.2}{b:+.2}",
                    if *e { "E" } else { "N" },
                    i % 10
                )
            })
            .collect();
        println!(
            "mountain land slopes, {rows}×{cols} grid ({:.1} km spacing) \
             @ amplitude_frac {amp_frac} ({:.0} m at R=600 km), {} land pairs: \
             p50 {:.4}  p95 {:.4}  p99 {:.4}  p99.9 {:.4}  max {:.4} \
             [>0.5: {}  >0.75: {}] [max pair: seed {mseed} j={mj} i={mi} {}]",
            2.0 * core::f64::consts::PI * SURVEY_R / cols as f64 / 1_000.0,
            SURVEY_R * amp_frac,
            tans.len(),
            p50,
            p95,
            p99,
            p999,
            max_tan,
            over50,
            over75,
            if meast { "east" } else { "north" }
        );
        println!("  top10: {}", top10.join(" "));
        assert!(p50 <= p95 && p95 <= p99 && p99 <= max_tan);
    }

    /// The §2.1 slope-survey tuning probe — a faithful mirror of
    /// `csp-phys`'s `mountain_land_slope_distribution` survey (same
    /// grids, seeds, land rule, pair math, percentile picks), run
    /// explicitly when retuning [`height_normalized_mountain`]:
    /// `cargo test -p csp-core mountain_slope_survey -- --ignored
    /// --nocapture`. The authoritative warrant assert stays in
    /// `csp-phys` (the contact model owns it); this print-only twin
    /// exists so spectrum tuning does not need the physics crate in
    /// the loop.
    #[test]
    #[ignore = "tuning probe — print-only survey; csp-phys owns the warrant assert"]
    fn mountain_slope_survey_probe() {
        for (rows, cols) in [(48usize, 96usize), (256usize, 512usize)] {
            let grids = survey_bake(rows, cols);
            for amp_frac in [0.004f64, 0.006] {
                survey_report(rows, cols, &grids, amp_frac);
            }
        }
    }

    /// Timing probes — run explicitly (substring filter, no
    /// `--exact`: the bare fn name is not the full test path), never
    /// in CI: this crate denies the wall-clock types (`clippy.toml`
    /// bans `Instant`/`SystemTime`/`Duration` everywhere, tests
    /// included), so each probe just bakes a fixed 3 times and the
    /// wall time is measured from the shell around the whole test
    /// process.
    #[test]
    #[ignore = "timing probe — measure from the shell: time cargo test -p csp-core perf_probe_bake_legacy -- --ignored --nocapture"]
    fn perf_probe_bake_legacy() {
        for _ in 0..3 {
            let f = HeightField::bake(0x5eed_c0de_1234_5678, BAKE_WIDTH, BAKE_HEIGHT);
            std::hint::black_box(&f.data[f.data.len() / 2]);
        }
    }

    /// [`Self::perf_probe_bake_legacy`]'s Mountain twin — 3 bakes at
    /// 2048×1024 (4× the texels, 16/13 the layers).
    #[test]
    #[ignore = "timing probe — measure from the shell: time cargo test -p csp-core perf_probe_bake_mountain -- --ignored --nocapture"]
    fn perf_probe_bake_mountain() {
        for _ in 0..3 {
            let f = HeightField::bake_profiled(0x5eed_c0de_1234_5678, ReliefProfile::Mountain);
            std::hint::black_box(&f.data[f.data.len() / 2]);
        }
    }
}
