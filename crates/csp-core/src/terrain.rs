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
//! Two relief profiles ship (the §5.2-R2 terrain overhaul, M5):
//! [`ReliefProfile::Legacy`] — the R2-launch [`height_normalized`]
//! spectrum every existing body bakes, and the data default — and
//! [`ReliefProfile::Mountain`] — a sharper ridged spectrum with
//! valley carving and domain warp, baked at 2048×1024 so its extra
//! peak detail survives the texel grid ([`HeightField::bake_profiled`]).
//! The profile is data-authored (`csp_data`'s `TerrainDef`), reaches
//! the bake key through a seed fold ([`HeightField::bake_profiled_key`]),
//! and never touches the sim state hash — heights stay a pure function
//! of `(seed, direction, profile)`.

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
    /// The M5 terrain-overhaul spectrum: stronger ridged weighting,
    /// extra high-frequency ridge octaves for peak detail,
    /// ridge-inversion valley carving, and a low-frequency domain
    /// warp — baked at [`MOUNTAIN_BAKE_WIDTH`]×[`MOUNTAIN_BAKE_HEIGHT`]
    /// ([`height_normalized_mountain`]'s doc has the spectrum).
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

/// The Mountain spectrum (§5.2-R2's terrain overhaul, M5) — same
/// construction rules as [`height_normalized`] (integer-hash lattice
/// + f64 arithmetic only; no std transcendentals), harder relief:
///
/// - **Inherited land/ocean mask**: the continents term — identical
///   seed constant, frequency, and octave count — keeps the legacy
///   coastline statistics, so an `OceanDef` body's seas still sit
///   where its continents say they do.
/// - **Stronger ridges**: the legacy ridge field (same seed constant)
///   at 5 octaves instead of 4, composite-weighted 0.80 on land vs
///   legacy's 0.35.
/// - **Peak detail**: two extra high-frequency ridge octaves (`×28`)
///   for crest sharpening legacy's `×18` hills smear over.
/// - **Valley carving**: a ridge *inversion* at `×5` — `1 − r²` is
///   the basin between ridge crests, subtracted on land to cut
///   channels through the ranges.
/// - **Domain warp**: one low-frequency warp layer (three
///   single-octave axis fields at `×2`) folds the direction the ridge
///   terms sample — folded strata instead of straight lattice ridges.
///
/// Octave budget: 4 (continents) + 5 (ridges) + 2 (detail) + 2
/// (valleys) + 3 (warp axes) = **16 noise layers**, the documented
/// ceiling that keeps bake cost bounded; legacy uses 13.
fn height_normalized_mountain(seed: u64, dir: DVec3) -> f64 {
    let continents = fbm3(seed ^ 0x9e37_79b9_7f4a_7c15, dir * 3.0, 4);
    let land = smoothstep(-0.05, 0.35, continents);
    let q = dir
        + DVec3::new(
            value_noise3(seed ^ 0x6a09_e667_f3bc_c908, dir * 2.0),
            value_noise3(seed ^ 0xbb67_ae85_84ca_a73b, dir * 2.0),
            value_noise3(seed ^ 0x3c6e_f372_fe94_f82b, dir * 2.0),
        ) * 0.3;
    let ridged = {
        let n = fbm3(seed ^ 0x1656_67b1_9e37_79f9, q * 7.0, 5);
        let r = 1.0 - 2.0 * n.abs();
        r * r // ~[0,1], crested
    };
    let detail = {
        let n = fbm3(seed ^ 0xa54f_f53a_5f1d_36f1, q * 28.0, 2);
        let r = 1.0 - 2.0 * n.abs();
        r * r
    };
    let valleys = {
        let n = fbm3(seed ^ 0x510e_527f_ade6_82d1, q * 5.0, 2);
        let r = 1.0 - 2.0 * n.abs();
        1.0 - r * r // ridge inversion — the basin between crests
    };
    let ridge_mix = 0.72 * ridged + 0.28 * detail;
    (0.62 * continents + land * (0.80 * ridge_mix - 0.15 * valleys - 0.10)).clamp(-1.0, 1.0)
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
    /// output despite the stronger 0.80 ridge weighting.
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
    /// exact bits so the spectrum cannot silently drift.
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
        assert_eq!(peak_h.to_bits(), 0x3fe9_9874_edf2_ee71);
        assert!(peak_h > 0.5);
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
