//! R2 terrain + atmosphere (PLAN §5.2 row "CDLOD/quadtree terrain with
//! per-patch f64 origins, precomputed-LUT scattering, ocean, planet-scale
//! LOD"; §5.1 Baseline column: "CPU quadtree LOD, pre-baked tiles" +
//! "precomputed LUTs, analytic single-scatter").
//!
//! # Scheme
//!
//! - **Height source.** §10's open question ("procedural vs authored
//!   heightmap vs hybrid") resolves to *deterministic seeded noise* at
//!   this tier: `TerrainDef::noise_seed` drives a lattice value-noise
//!   FBM evaluated on the unit-sphere direction (no equirect seams, no
//!   polar pinching), baked once into an `R32Float` equirect
//!   [`HeightField`] texture ("pre-baked tiles"). A `heightmap: Some(_)`
//!   ref is the R4/native-tier hook — nothing authors one yet.
//!   Biomes stay decoupled (§3.0.1): the authored index map only *tints*
//!   the surface, it never moves a vertex.
//! - **LOD.** A CPU quadtree over the six cube-sphere faces
//!   ([`select_patches`]): split while `dist(eye, node) < K · node_size`,
//!   horizon-culled, budget-bounded, BFS-deterministic. All leaves share
//!   one unit grid mesh — the draw is a single instanced call.
//! - **Precision (§2.3).** Each patch carries an `f64` surface origin
//!   (the datum point under its centre); [`patch_instance`] bakes
//!   `origin − eye` in `f64` to `f32`, and the shader builds only the
//!   small patch-local delta `R·(dir − dir0) + dir·h` — no large-magnitude
//!   `f32` subtraction anywhere. Residual vertex error at a 600 km body
//!   is ~5 cm.
//! - **Stitching.** Skirts: the shared grid carries an outer ring flagged
//!   `skirt = 1` that the vertex shader drops radially by
//!   `patch_extent · SKIRT_FRAC + 2 m`, hiding T-junction cracks.
//! - **Scattering.** [`bake_transmittance`] builds the Bruneton-style
//!   transmittance LUT `T(altitude, μ)` for the §3.0 exponential
//!   atmosphere at texture-build time (CPU ray-march, deterministic);
//!   the shader consumes it for sky colour (sun-angle + altitude
//!   dependent) and terrain aerial perspective — the Baseline tier's
//!   "precomputed LUTs, analytic single-scatter".
//! - **Ocean.** A `BodyDef::ocean` datum draws a second instanced batch
//!   of the same quadtree at `h = 0` on its own datum radius, emitted
//!   only for leaf patches whose sampled heights reach the datum —
//!   airless/oceanless bodies emit nothing.
//!
//! Everything sim-visible is baked into [`TerrainPacket`] by `csp-view`
//! (§2.1: this crate never sees a body def at draw time — the
//! `csp_core::DVec3` use below is confined to the pure builders the
//! extractor calls).

use std::collections::VecDeque;
use std::sync::Arc;

use csp_core::DVec3;

use crate::GpuBuffer;

// ---------------------------------------------------------------------------
// Seeded procedural height — the DETERMINISTIC SHARED SOURCE lives in
// `csp_core::terrain` so the physics contact surface (`csp-phys`'s
// `Environment::terrain`) samples bit-identically to this shader-fed
// bake. Re-exported here so the `rt::HeightField`/`height_normalized`
// paths the extractor and tests already use keep naming it.
// `mix64` stays for the `BiomeField` content key below.
// ---------------------------------------------------------------------------

use csp_core::terrain::mix64;
pub use csp_core::terrain::{HeightField, height_normalized};

// ---------------------------------------------------------------------------
// Quadtree LOD over the cube sphere.
// ---------------------------------------------------------------------------

/// One quadtree node/leaf on the cube sphere: `face` selects the cube
/// face, `(x, y)` the cell inside the face's `2^level` grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct PatchId {
    /// Cube face 0..6 (see [`face_basis`]).
    pub face: u8,
    /// Quadtree depth (0 = whole face).
    pub level: u8,
    /// Cell column `0..2^level`.
    pub x: u16,
    /// Cell row `0..2^level`.
    pub y: u16,
}

/// `(normal, u-basis, v-basis)` for a cube face: the face's cube-space
/// point at `(su, sv) ∈ [-1,1]²` is `n + su·bu + sv·bv`.
pub fn face_basis(face: u8) -> (DVec3, DVec3, DVec3) {
    match face {
        0 => (DVec3::X, -DVec3::Y, -DVec3::Z),
        1 => (-DVec3::X, DVec3::Y, -DVec3::Z),
        2 => (DVec3::Y, DVec3::X, -DVec3::Z),
        3 => (-DVec3::Y, -DVec3::X, -DVec3::Z),
        4 => (DVec3::Z, DVec3::X, -DVec3::Y),
        _ => (-DVec3::Z, -DVec3::X, -DVec3::Y),
    }
}

/// LOD selection tunables (presentation constants — §8.3 owns sim
/// tuning; these are view geometry).
#[derive(Clone, Copy, Debug)]
pub struct LodConfig {
    /// Deepest quadtree level (finest patch ≈ `2·R / 2^max_level`
    /// across — 12 on a 600 km body ≈ 290 m). Hard bound: `PatchId`
    /// cell indices are `u16`, so a level-16 cell is the largest
    /// representable — `x * 2 + dx` wraps past `max_level = 16`.
    pub max_level: u8,
    /// Split while `dist(eye, patch) < split_factor · patch_size`.
    pub split_factor: f64,
    /// Hard cap on emitted patches (BFS order ⇒ coarse coverage wins).
    pub max_patches: usize,
}

impl Default for LodConfig {
    fn default() -> Self {
        Self {
            max_level: 12,
            split_factor: 4.0,
            max_patches: 2048,
        }
    }
}

/// Cube-space `(corner, du, dv)` of a patch — `du`/`dv` are the patch's
/// full spans, so a unit `uv ∈ [0,1]²` maps to `corner + u·du + v·dv`.
/// All magnitudes `≤ √3` — f32-safe by construction (§2.3).
pub fn patch_cube(id: PatchId) -> (DVec3, DVec3, DVec3) {
    let (n, bu, bv) = face_basis(id.face);
    let cells = (1u32 << id.level) as f64;
    let span = 2.0 / cells;
    let corner =
        n + bu * (2.0 * f64::from(id.x) / cells - 1.0) + bv * (2.0 * f64::from(id.y) / cells - 1.0);
    (corner, bu * span, bv * span)
}

/// Patch centre direction (unit) + cube-space span length.
fn patch_dir0_size(id: PatchId) -> (DVec3, f64) {
    let (corner, du, dv) = patch_cube(id);
    let c = corner + (du + dv) * 0.5;
    (c / c.norm(), du.norm())
}

/// The R2 LOD walk: BFS over the six cube faces, splitting while the
/// eye is within `split_factor · patch_size` of the patch's datum
/// sphere point, up to `max_level` / `max_patches`. Nodes fully beyond
/// the horizon are culled (angle from camera nadir > horizon
/// half-angle + patch angular radius). Deterministic — fixed face
/// order, fixed child order, FIFO queue; a saturated budget emits the
/// queued nodes as leaves, so coarse coverage completes before detail.
///
/// `amplitude_m` is the surface's maximum displacement ABOVE the
/// datum: relief rises to `radius_m + amplitude_m`, so the cull must
/// credit peaks beyond the bare-datum horizon or the displaced limb
/// culls its own relief (missing mountains + subtree-level popping,
/// P08-F1). The margin is the two-observer horizon rule: the eye
/// (height `alt`) and a peak (height `amplitude_m`) see each other
/// while the SUM of their datum-horizon angles covers the
/// separation — `acos(R/(R+alt)) + acos(R/(R+amp))`. It errs wide on
/// purpose: elevated intervening terrain narrows the real horizon,
/// and an over-kept leaf is a few verts, not a missing mountain.
pub fn select_patches(
    eye_rel_body: DVec3,
    radius_m: f64,
    amplitude_m: f64,
    cfg: &LodConfig,
) -> Vec<PatchId> {
    // `PatchId::{x, y}` are `u16`: a level-16 node holds cell indices
    // up to `2^16 − 1`, so splitting one would wrap `x * 2 + dx`.
    // Clamp rather than assert — a tuned-up `LodConfig` silently
    // degrades to depth 16 instead of emitting corrupted cells in
    // release builds (the old `debug_assert!` covered only debug).
    let max_level = cfg.max_level.min(16);
    let mut out = Vec::new();
    let eye_norm = eye_rel_body.norm();
    let eye_r = eye_norm.max(radius_m * 1e-9);
    let alt = (eye_r - radius_m).max(0.0);
    // Horizon half-angle about the sub-camera point: the visible surface
    // is the disc of directions within `acos(R / (R+h))` of the eye's
    // own radial direction; beyond it the planet occludes itself —
    // PLUS the horizon a peak of height `amplitude_m` commands beyond
    // that datum limb (the two-observer rule in the fn doc). The
    // bare-datum term alone culls displaced relief at the limb.
    let horizon = libm::acos((radius_m / (radius_m + alt)).clamp(-1.0, 1.0))
        + libm::acos((radius_m / (radius_m + amplitude_m.max(0.0))).clamp(-1.0, 1.0));
    // Degenerate eye at the body centre → no culling.
    let sub_dir = (eye_norm > radius_m * 1e-9).then(|| eye_rel_body / eye_norm);
    let mut queue = VecDeque::new();
    for face in 0..6u8 {
        queue.push_back(PatchId {
            face,
            level: 0,
            x: 0,
            y: 0,
        });
    }
    while let Some(id) = queue.pop_front() {
        let (dir0, cube_size) = patch_dir0_size(id);
        // Horizon cull: patch centre direction vs the sub-camera point.
        if let Some(s) = sub_dir {
            let cos_ang = s.dot(dir0).clamp(-1.0, 1.0);
            let ang_radius = libm::atan(cube_size * 0.75);
            if libm::acos(cos_ang) > horizon + ang_radius {
                continue;
            }
        }
        let dist = (eye_rel_body - dir0 * radius_m).norm() - cube_size * radius_m * 0.5;
        if out.len() + queue.len() < cfg.max_patches
            && id.level < max_level
            && dist < cfg.split_factor * cube_size * radius_m
        {
            for (dx, dy) in [(0u16, 0u16), (1, 0), (0, 1), (1, 1)] {
                queue.push_back(PatchId {
                    face: id.face,
                    level: id.level + 1,
                    x: id.x * 2 + dx,
                    y: id.y * 2 + dy,
                });
            }
        } else {
            out.push(id);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Packet + GPU-facing data types.
// ---------------------------------------------------------------------------

/// Per-patch instance row (5 × `vec4<f32>`, instance step mode):
/// `corner`/`du`/`dv` are cube-space span data; `dir0_kind` is the
/// patch-centre unit direction + draw kind (0 = terrain, 1 = ocean);
/// `origin_rel_eye` is the §2.3 bake — the patch's `f64` datum origin
/// minus the eye, cast to `f32`, with `w` = skirt depth (m).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub struct TerrainInstance {
    /// Cube-space patch corner (`xyz`), `w` unused.
    pub corner: [f32; 4],
    /// Cube-space u-span (`xyz`), `w` unused.
    pub du: [f32; 4],
    /// Cube-space v-span (`xyz`), `w` unused.
    pub dv: [f32; 4],
    /// Patch-centre unit direction (`xyz`); `w` = kind (0 terrain, 1 ocean).
    pub dir0_kind: [f32; 4],
    /// `patch_origin − eye` in metres (`xyz`, f64→f32 bake);
    /// `w` = skirt depth.
    pub origin_rel_eye: [f32; 4],
}

/// The shared grid mesh vertex: `uv ∈ [0,1]²` inside the patch, `skirt`
/// flags the outer ring the shader drops radially.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub struct GridVertex {
    /// Patch parametric coordinate.
    pub uv: [f32; 2],
    /// 1.0 on the skirt ring, 0.0 interior.
    pub skirt: f32,
    /// Alignment pad.
    pub pad: f32,
}

/// Frame uniforms — the whole GPU-visible frame state (f32 only, §2.3).
/// Every member is 16-byte so the Rust `repr(C)` and WGSL layouts agree;
/// matrices are stored **column-major** (`m[col][row]`) to match WGSL.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub struct TerrainUniforms {
    /// `(world − eye) → clip` (the camera's f64 view-proj, transposed
    /// into column-major and cast down).
    pub view_proj: [[f32; 4]; 4],
    /// View→world rotation (columns are the view axes) for sky rays.
    pub view_rot: [[f32; 4]; 4],
    /// Camera position relative to the body centre (`xyz`, metres).
    pub eye_rel_body: [f32; 4],
    /// Unit sun direction in the body frame (`xyz`).
    pub sun_dir: [f32; 4],
    /// `x` datum radius, `y` atmosphere top (m, 0 = none), `z` ocean
    /// datum radius (0 = none), `w` terrain amplitude (m).
    pub planet: [f32; 4],
    /// `x` atmosphere scale height (m), `y` has-biome flag, `z` craft
    /// altitude above the local terrain surface (m — the blob shadow's
    /// altitude fade), `w` unused.
    pub atmo: [f32; 4],
    /// `x` tan(fov_y/2)·aspect, `y` tan(fov_y/2) — sky ray
    /// reconstruction; `z`/`w` are `fs_sky`'s ray/shell quadratic
    /// constants `|pa|² − r_top²` / `|pa|² − R²` — baked CPU-side in
    /// f64 (the cancelled terms are ~1e11 m², past f32 resolution).
    pub misc: [f32; 4],
    /// Body spin for the fixed-frame terrain lookup: `xyz` unit spin
    /// axis (bubble frame), `w` the `ω·t` phase wrapped to `[0, 2π)` —
    /// the same convention the sim's §2.6.4 surface latch uses
    /// (`inertial = R(axis, phase)·fixed`). The shader samples the
    /// heightfield/biome map at `R(axis, −phase)·dir`; positions and
    /// lighting stay inertial.
    pub spin: [f32; 4],
    /// Rayleigh β (`rgb`, 1/m), `w` unused.
    pub rayleigh: [f32; 4],
    /// Biome palette colours (slot → linear RGB), up to 16 slots.
    pub biome_colors: [[f32; 4]; 16],
    /// Vessel blob-shadow anchor: `xyz` unit direction (inertial,
    /// body-centred) of the point where the focused craft's shadow
    /// lands — the craft position projected down-sun to the local
    /// surface; `w` blob radius (m), `0` disables. The chase camera's
    /// focus is the vessel, so the extractor derives this from the
    /// solved view basis rather than plumbing sim state through the
    /// packet contract.
    pub shadow: [f32; 4],
    /// Art-asset row: `x` = real-texture flag (1.0 when the packet
    /// carries [`TerrainArt`] — the shader falls back to procedural
    /// detail when 0), `y` = presentation clock in seconds (wave
    /// scroll — the sim tick-derived value, still deterministic),
    /// `z`/`w` unused.
    pub art: [f32; 4],
    /// Per-class mean LINEAR albedo (grass, rock, sand, snow —
    /// `TerrainTex::means` sRGB→linear at packet build). The shader
    /// divides class detail by these; the header row is the single
    /// source of truth (no deepest-mip readback).
    pub class_means: [[f32; 4]; 4],
}

/// Atmosphere signature the renderer keys its LUT rebuild on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AtmoDesc {
    /// Datum radius (m).
    pub radius_m: f64,
    /// Atmosphere top (m).
    pub top_m: f64,
    /// Exponential scale height (m).
    pub scale_height_m: f64,
}

/// A decoded biome index map + its resolved palette — presentation
/// tinting only (§3.0.1 keeps biomes decoupled from terrain shape).
/// `indices` are already resolved to *palette slots* (`0..16`), not PNG
/// palette indices — the shader reads `biome_colors[slot]` directly.
#[derive(Debug)]
pub struct BiomeField {
    /// Texels per row.
    pub width: u32,
    /// Rows (latitude, PNG scanline order = north→south).
    pub height: u32,
    /// `width·height` palette-slot bytes, row-major (255 = unmapped).
    pub indices: Vec<u8>,
    /// Slot → linear RGB (unused slots carry the fallback tint).
    pub colors: [[f32; 4]; 16],
    /// Content hash — the renderer's re-upload key. Keying on content
    /// (like [`HeightField::key`]) rather than the `Arc` address keeps
    /// the cache correct if an `Arc` is ever dropped and a new field
    /// lands on the recycled pointer (latent ABA).
    pub key: u64,
}

impl BiomeField {
    /// Build a field, hashing the content for [`Self::key`]. The hash
    /// is a `mix64` fold over dims + slots + palette — deterministic,
    /// order-sensitive, one-time (fields are immutable).
    pub fn new(width: u32, height: u32, indices: Vec<u8>, colors: [[f32; 4]; 16]) -> Self {
        let mut key = mix64(u64::from(width) << 32 | u64::from(height));
        for (i, &slot) in indices.iter().enumerate() {
            key = mix64(key ^ u64::from(slot).wrapping_add(i as u64));
        }
        for c in colors.iter().flatten() {
            key = mix64(key ^ u64::from(c.to_bits()));
        }
        Self {
            width,
            height,
            indices,
            colors,
            key,
        }
    }
}

/// A decoded real texture (RGBA8, mip-chained, optionally layered) —
/// the `.csar` image payload as uploaded. `data` is mip-major
/// (level 0..mips, each level holding `layers` layers of
/// `(w>>l)×(h>>l)×4` bytes contiguous).
#[derive(Debug)]
pub struct TerrainTex {
    /// Level-0 width (px).
    pub w: u32,
    /// Level-0 height (px).
    pub h: u32,
    /// Array layers (1 = plain 2D texture).
    pub layers: u32,
    /// Mip levels present in `data`.
    pub mips: u32,
    /// Per-layer mean sRGB RGBA — consumed by `csp-view`'s terrain
    /// packet build into `uniforms.class_means`, which the shader
    /// divides texel detail by so the authored palette keeps the hue.
    pub means: Vec<[u8; 4]>,
    /// Concatenated mip levels (see above).
    pub data: Vec<u8>,
}

/// The real-asset set for the surface pass — decoded from
/// `data/art/terrain.csar` by `csp-view` (see `assets/ASSETS.md` for
/// provenance). All three members ride one upload key: the pack is
/// all-or-nothing. The night sky is procedural (`fs_sky`'s shell-lattice
/// starfield) — no star texture ships.
#[derive(Debug)]
pub struct TerrainArt {
    /// Class albedo array — layers `[grass, rock, sand, snow]`.
    pub albedo: TerrainTex,
    /// Matching tangent-space normal maps (same layer order).
    pub normal: TerrainTex,
    /// Ocean wave normal map.
    pub water: TerrainTex,
    /// Content key — the renderer re-uploads on change.
    pub key: u64,
}

/// The R2 RenderExtract product — everything [`TerrainRenderer::draw`]
/// needs for one frame. `heightfield`/`biome`/`art` are `Arc`-shared so the
/// per-frame packet clone is O(1); the renderer re-uploads each only
/// when its key changes.
#[derive(Clone, Debug, Default)]
pub struct TerrainPacket {
    /// Frame uniforms.
    pub uniforms: TerrainUniforms,
    /// Baked heightfield (absent ⇒ no terrain drawn).
    pub heightfield: Option<Arc<HeightField>>,
    /// Biome tint field (absent ⇒ height-ramp palette only).
    pub biome: Option<Arc<BiomeField>>,
    /// Atmosphere signature — `None` draws the vacuum path.
    pub atmo: Option<AtmoDesc>,
    /// Terrain leaf instances (heightfield-displaced).
    pub terrain: Vec<TerrainInstance>,
    /// Ocean leaf instances (datum-clamped surface).
    pub ocean: Vec<TerrainInstance>,
    /// Draw the sky backdrop pass.
    pub sky: bool,
    /// The real texture set — `None` keeps the procedural fallback
    /// shading (the packet stays valid with the placeholder textures).
    pub art: Option<Arc<TerrainArt>>,
}

/// Skirt depth as a fraction of patch extent (+ a constant floor so the
/// finest levels still cover high-frequency mismatch).
pub const SKIRT_FRAC: f64 = 0.02;
/// Ocean-patch culling margin (fraction of amplitude, min 250 m): a
/// leaf whose 5×5 height samples all sit above this bound emits no
/// ocean quad — it cannot contain a shoreline at its own resolution.
pub const OCEAN_CULL_FRAC: f64 = 0.25;

/// Bake a patch's instance row — the §2.3 precision step: the f64
/// datum origin `dir0·R` minus the f64 eye lands in `origin_rel_eye`
/// as f32; everything else is small-magnitude cube-space data.
pub fn patch_instance(
    id: PatchId,
    kind: f32,
    eye_rel_body: DVec3,
    radius_m: f64,
) -> TerrainInstance {
    let (corner, du, dv) = patch_cube(id);
    let dir0 = {
        let c = corner + (du + dv) * 0.5;
        c / c.norm()
    };
    let origin_rel_eye = dir0 * radius_m - eye_rel_body;
    let skirt = (du.norm() * radius_m * SKIRT_FRAC + 2.0) as f32;
    let v4 = |v: DVec3, w: f32| [v.x as f32, v.y as f32, v.z as f32, w];
    TerrainInstance {
        corner: v4(corner, 0.0),
        du: v4(du, 0.0),
        dv: v4(dv, 0.0),
        dir0_kind: v4(dir0, kind),
        origin_rel_eye: v4(origin_rel_eye, skirt),
    }
}

// ---------------------------------------------------------------------------
// Scattering — the transmittance LUT bake (Bruneton-style; the §3.0
// exponential atmosphere makes the march a simple trapezoid integral).
// ---------------------------------------------------------------------------

/// Rayleigh β at sea level (1/m) — Earth-air-like spectral mix, drives
/// both the LUT bake and the shader's inscatter. Presentation constant.
pub const RAYLEIGH_BETA: [f64; 3] = [5.8e-6, 13.5e-6, 33.1e-6];

/// LUT dimensions (μ columns × altitude rows).
pub const LUT_W: usize = 64;
/// See [`LUT_W`].
pub const LUT_H: usize = 32;
/// Trapezoid steps per marched ray — fixed, deterministic.
const LUT_STEPS: usize = 96;

/// One transmittance-ray optical depth: from radius `r0` along
/// `(sx, sy)` in the zenith frame the ray runs `t_end` metres.
/// The trapezoid lattice matches `fs_sky`'s march conventions and is
/// shared by the LUT bake and the per-frame CPU lookups below.
fn march_tau(atmo: AtmoDesc, r0: f64, sx: f64, sy: f64, t_end: f64) -> f64 {
    let mut tau = 0.0;
    if t_end > 0.0 {
        let ds = t_end / LUT_STEPS as f64;
        for i in 0..LUT_STEPS {
            let t = (i as f64 + 0.5) * ds;
            let px = t * sx;
            let py = r0 + t * sy;
            let alt_i = (libm::sqrt(px * px + py * py) - atmo.radius_m).max(0.0);
            tau += libm::exp(-alt_i / atmo.scale_height_m) * ds;
        }
    }
    tau
}

/// Transmittance to space from altitude `h` along zenith-cosine `mu`,
/// in the same frame/convention as [`bake_transmittance`]: a
/// ground-struck ray returns `(0, true)`. Used by `csp-view` to tint
/// the craft's staged sun color by the same atmosphere the terrain
/// pass attenuates through (C11-F2) — one ray is ~96 trapezoid taps,
/// cheap enough per frame without rebuilding the whole LUT.
pub fn transmittance_to_space(atmo: AtmoDesc, beta: [f64; 3], h: f64, mu: f64) -> [f32; 3] {
    let mu = mu.clamp(-1.0, 1.0);
    let r0 = atmo.radius_m + h.clamp(0.0, atmo.top_m);
    let (sx, sy) = (libm::sqrt((1.0 - mu * mu).max(0.0)), mu);
    // Same path-length math as the bake: far top-shell root, or the
    // forward datum strike.
    let disc = r0 * r0 * mu * mu - (r0 * r0 - (atmo.radius_m + atmo.top_m).powi(2));
    let t_exit = if disc <= 0.0 {
        0.0
    } else {
        -r0 * mu + libm::sqrt(disc)
    };
    let gdisc = r0 * r0 * mu * mu - (r0 * r0 - atmo.radius_m * atmo.radius_m);
    let t_ground = if gdisc <= 0.0 {
        f64::INFINITY
    } else {
        let near = -r0 * mu - libm::sqrt(gdisc);
        let far = -r0 * mu + libm::sqrt(gdisc);
        if near > 1e-9 {
            near
        } else if far > 1e-9 && mu < 0.0 {
            0.0
        } else {
            f64::INFINITY
        }
    };
    let t_end = t_exit.min(t_ground);
    let tau = march_tau(atmo, r0, sx, sy, t_end);
    let blocked = t_ground < t_exit;
    [
        if blocked {
            0.0
        } else {
            libm::exp(-tau * beta[0]) as f32
        },
        if blocked {
            0.0
        } else {
            libm::exp(-tau * beta[1]) as f32
        },
        if blocked {
            0.0
        } else {
            libm::exp(-tau * beta[2]) as f32
        },
    ]
}

/// Bake the transmittance LUT `T(h, μ) = exp(−β_r · τ)` over
/// `μ ∈ [−1, 1]` (columns) × `h ∈ [0, top_m]` (rows), `RGBA32Float`,
/// row-major `(row, col)`, `a` = 1.
///
/// From altitude `h` along zenith-cos `μ` the march runs to the far
/// top-shell intersection, or stops at the datum — a ground-struck ray
/// transmits 0 (no light crosses the planet). Deterministic f64.
pub fn bake_transmittance(atmo: AtmoDesc, beta: [f64; 3]) -> Vec<f32> {
    let mut out = Vec::with_capacity(LUT_W * LUT_H * 4);
    for row in 0..LUT_H {
        let h = atmo.top_m * row as f64 / (LUT_H - 1) as f64;
        for col in 0..LUT_W {
            let mu = -1.0 + 2.0 * col as f64 / (LUT_W - 1) as f64;
            let t = transmittance_to_space(atmo, beta, h, mu);
            out.extend_from_slice(&t);
            out.push(1.0);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// GPU side — one instanced pipeline + a fullscreen sky pass.
// ---------------------------------------------------------------------------

/// The shared unit grid: `(N+3)²` verts (N cells + a skirt ring) +
/// `u16` indices — every terrain/ocean instance draws this mesh.
pub const GRID_CELLS: u32 = 16;

/// Build the shared grid: verts `(i, j) ∈ 0..(N+3)`, uv clamped to
/// `[0,1]` with the outer ring flagged `skirt`; indices cover every
/// quad including the skirt wall. Deterministic constants only.
pub fn unit_grid() -> (Vec<GridVertex>, Vec<u16>) {
    let n = GRID_CELLS as usize;
    let side = n + 3;
    let mut verts = Vec::with_capacity(side * side);
    for j in 0..side {
        for i in 0..side {
            let edge = i == 0 || j == 0 || i == side - 1 || j == side - 1;
            let u = (((i as f32) - 1.0) / n as f32).clamp(0.0, 1.0);
            let v = (((j as f32) - 1.0) / n as f32).clamp(0.0, 1.0);
            verts.push(GridVertex {
                uv: [u, v],
                skirt: if edge { 1.0 } else { 0.0 },
                pad: 0.0,
            });
        }
    }
    let mut indices = Vec::with_capacity((n + 2) * (n + 2) * 6);
    for j in 0..(side - 1) {
        for i in 0..(side - 1) {
            let a = (j * side + i) as u16;
            let (b, c, d) = (a + 1, a + side as u16, a + side as u16 + 1);
            indices.extend_from_slice(&[a, c, b, b, c, d]);
        }
    }
    (verts, indices)
}

/// A data texture's shape/format — creation + upload parameters in one
/// value (keeps [`TerrainRenderer::write_tex`] under the arg limit).
struct TexUpload {
    /// Debug label.
    label: &'static str,
    /// wgpu format.
    format: wgpu::TextureFormat,
    /// Width (px).
    w: u32,
    /// Height (px).
    h: u32,
    /// Bytes per texel.
    bpp: u32,
}

impl TexUpload {
    fn create(&self, device: &wgpu::Device) -> wgpu::Texture {
        self.create_layers(device, 1, 1)
    }

    /// Create with an array depth and mip count (real-art textures).
    fn create_layers(&self, device: &wgpu::Device, layers: u32, mips: u32) -> wgpu::Texture {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some(self.label),
            size: wgpu::Extent3d {
                width: self.w,
                height: self.h,
                depth_or_array_layers: layers,
            },
            mip_level_count: mips,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        })
    }
}

/// GPU owner for the R2 pass: the terrain/ocean instanced pipeline, the
/// fullscreen sky pipeline, the uniform buffer, the three data
/// textures (heightfield `R32Float`, transmittance `RGBA32Float`, biome
/// `R8Uint`), and the real-art set (albedo/normal `RGBA8` arrays, the
/// water normal map + the filtering sampler) — each re-uploaded only
/// when its packet key changes.
pub struct TerrainRenderer {
    terrain_pipe: wgpu::RenderPipeline,
    /// Ocean pipeline — same shaders/mesh as `terrain_pipe`, depth
    /// test-only, drawn after the flight stream (see `pipelines`).
    ocean_pipe: wgpu::RenderPipeline,
    sky_pipe: wgpu::RenderPipeline,
    bgl: wgpu::BindGroupLayout,
    bind_group: wgpu::BindGroup,
    uniforms: wgpu::Buffer,
    grid_v: wgpu::Buffer,
    grid_i: wgpu::Buffer,
    grid_index_count: u32,
    grid_uploaded: bool,
    instances: GpuBuffer,
    /// Byte range of the ocean slice inside `instances` (terrain ends
    /// at `inst_bytes`, the upload at `inst_end`) — set by `draw`,
    /// consumed by `draw_ocean`.
    inst_bytes: wgpu::BufferAddress,
    inst_end: wgpu::BufferAddress,
    height_tex: wgpu::Texture,
    lut_tex: wgpu::Texture,
    biome_tex: wgpu::Texture,
    /// Real-art textures (bindings 4–6) + the shared sampler (7).
    alb_tex: wgpu::Texture,
    nrm_tex: wgpu::Texture,
    water_tex: wgpu::Texture,
    sampler: wgpu::Sampler,
    uploaded_height_key: Option<u64>,
    uploaded_atmo: Option<AtmoDesc>,
    uploaded_biome_key: Option<u64>,
    uploaded_art_key: Option<u64>,
}

impl TerrainRenderer {
    /// The shared bind-group layout: 0 uniforms, 1 heightfield,
    /// 2 transmittance LUT, 3 biome slots — all `textureLoad`
    /// (non-filterable formats keep every backend happy). Bindings 4–6
    /// are the real-art set (albedo/normal `D2Array`, water normal `D2`,
    /// all filterable `RGBA8`) plus the filtering sampler at 7 — they
    /// carry 1×1 placeholders until a packet uploads art.
    fn bind_group_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
        let tex = |binding: u32, vis, sample_type| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: vis,
            ty: wgpu::BindingType::Texture {
                sample_type,
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let tex_arr = |binding: u32| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2Array,
                multisampled: false,
            },
            count: None,
        };
        device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("terrain"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                tex(
                    1,
                    wgpu::ShaderStages::VERTEX_FRAGMENT,
                    wgpu::TextureSampleType::Float { filterable: false },
                ),
                tex(
                    2,
                    wgpu::ShaderStages::VERTEX_FRAGMENT,
                    wgpu::TextureSampleType::Float { filterable: false },
                ),
                tex(
                    3,
                    wgpu::ShaderStages::FRAGMENT,
                    wgpu::TextureSampleType::Uint,
                ),
                tex_arr(4),
                tex_arr(5),
                tex(
                    6,
                    wgpu::ShaderStages::FRAGMENT,
                    wgpu::TextureSampleType::Float { filterable: true },
                ),
                wgpu::BindGroupLayoutEntry {
                    binding: 7,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        })
    }

    /// The three passes sharing the layout: instanced terrain, the
    /// depth-test-only ocean variant (drawn after the flight stream),
    /// and the fullscreen sky (depth always-pass, no writes).
    fn pipelines(
        device: &wgpu::Device,
        layout: &wgpu::PipelineLayout,
        shader: &wgpu::ShaderModule,
        surface_format: wgpu::TextureFormat,
        depth_state: &wgpu::DepthStencilState,
    ) -> (
        wgpu::RenderPipeline,
        wgpu::RenderPipeline,
        wgpu::RenderPipeline,
    ) {
        let pipe = |label, vs, fs, buffers, targets, depth: wgpu::DepthStencilState| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(layout),
                vertex: wgpu::VertexState {
                    module: shader,
                    entry_point: Some(vs),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers,
                },
                fragment: Some(wgpu::FragmentState {
                    module: shader,
                    entry_point: Some(fs),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets,
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: Some(depth),
                multisample: crate::msaa_state(),
                multiview_mask: None,
                cache: None,
            })
        };
        let terrain_targets = [Some(wgpu::ColorTargetState {
            format: surface_format,
            blend: Some(wgpu::BlendState::ALPHA_BLENDING),
            write_mask: wgpu::ColorWrites::ALL,
        })];
        let terrain = pipe(
            "terrain",
            "vs_terrain",
            "fs_terrain",
            &[Some(GridVertex::LAYOUT), Some(TerrainInstance::LAYOUT)],
            &terrain_targets,
            depth_state.clone(),
        );
        // The ocean shares shaders/mesh but rides a second pipeline:
        // depth-TEST-only. It draws AFTER the flight vertex stream so a
        // hull below the waterline stays visible through the blend
        // instead of being rejected by depth the ocean wrote before the
        // craft existed (C11-F3). The `dir0_kind.w` discriminator still
        // selects the ocean shading branch in `fs_terrain`.
        let ocean = pipe(
            "ocean",
            "vs_terrain",
            "fs_terrain",
            &[Some(GridVertex::LAYOUT), Some(TerrainInstance::LAYOUT)],
            &terrain_targets,
            wgpu::DepthStencilState {
                depth_write_enabled: Some(false),
                ..depth_state.clone()
            },
        );
        let sky_targets = [Some(wgpu::ColorTargetState {
            format: surface_format,
            blend: None,
            write_mask: wgpu::ColorWrites::ALL,
        })];
        let sky = pipe(
            "sky",
            "vs_sky",
            "fs_sky",
            &[],
            &sky_targets,
            wgpu::DepthStencilState {
                depth_write_enabled: Some(false),
                depth_compare: Some(wgpu::CompareFunction::Always),
                ..depth_state.clone()
            },
        );
        (terrain, ocean, sky)
    }

    /// Build pipelines, the shared grid, the uniform buffer, and 1×1
    /// placeholder textures (a packet overrides them on first draw —
    /// the bind group starts valid regardless).
    pub fn new(device: &wgpu::Device, surface_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("terrain"),
            source: wgpu::ShaderSource::Wgsl(include_str!("terrain.wgsl").into()),
        });
        let bgl = Self::bind_group_layout(device);
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("terrain"),
            bind_group_layouts: &[Some(&bgl)],
            immediate_size: 0,
        });
        let depth_state = wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth32Float,
            depth_write_enabled: Some(true),
            depth_compare: Some(wgpu::CompareFunction::Greater),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        };
        let (terrain_pipe, ocean_pipe, sky_pipe) =
            Self::pipelines(device, &layout, &shader, surface_format, &depth_state);

        let (verts, indices) = unit_grid();
        let grid_v = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("terrain-grid-v"),
            size: (verts.len() * std::mem::size_of::<GridVertex>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let grid_i = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("terrain-grid-i"),
            size: (indices.len() * 2) as u64,
            usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("terrain-uniforms"),
            size: std::mem::size_of::<TerrainUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let (height_tex, lut_tex, biome_tex) = Self::init_data_tex(device);
        let (alb_tex, nrm_tex, water_tex, sampler) = Self::init_art_tex(device);
        let bind_group = Self::bind_group(
            device,
            &bgl,
            &uniforms,
            &height_tex,
            &lut_tex,
            &biome_tex,
            &alb_tex,
            &nrm_tex,
            &water_tex,
            &sampler,
        );
        Self {
            terrain_pipe,
            ocean_pipe,
            sky_pipe,
            bgl,
            bind_group,
            uniforms,
            grid_v,
            grid_i,
            grid_index_count: indices.len() as u32,
            grid_uploaded: false,
            instances: GpuBuffer::new(
                device,
                "terrain-instances",
                (std::mem::size_of::<TerrainInstance>() * 4096) as u64,
            ),
            inst_bytes: 0,
            inst_end: 0,
            height_tex,
            lut_tex,
            biome_tex,
            alb_tex,
            nrm_tex,
            water_tex,
            sampler,
            uploaded_height_key: None,
            uploaded_atmo: None,
            uploaded_biome_key: None,
            uploaded_art_key: None,
        }
    }

    /// The three data textures' 1×1 placeholders (recreated on real
    /// uploads — `write_tex` swaps them on shape change).
    fn init_data_tex(device: &wgpu::Device) -> (wgpu::Texture, wgpu::Texture, wgpu::Texture) {
        let placeholder = |format: wgpu::TextureFormat| TexUpload {
            label: "terrain-placeholder",
            format,
            w: 1,
            h: 1,
            bpp: 0,
        };
        (
            placeholder(wgpu::TextureFormat::R32Float).create(device),
            placeholder(wgpu::TextureFormat::Rgba32Float).create(device),
            placeholder(wgpu::TextureFormat::R8Uint).create(device),
        )
    }

    /// Real-art placeholders — 1×1 textures at the right dimension
    /// (the class arrays get 4 layers so `D2Array` views stay
    /// structurally identical to real uploads) + the wrap sampler the
    /// packs sample through. The sampler asks for 8× anisotropy:
    /// standing on the surface, nearly every visible texel is at a
    /// grazing angle, where plain trilinear picks the mip for the
    /// worst footprint axis and smears the ground along the view
    /// direction. wgpu clamps the request to the device limit (and to
    /// 1 where anisotropy is unsupported — e.g. WebGPU is allowed to
    /// ignore it), so this is a free upgrade, never a validation
    /// failure.
    fn init_art_tex(
        device: &wgpu::Device,
    ) -> (wgpu::Texture, wgpu::Texture, wgpu::Texture, wgpu::Sampler) {
        let ph = |format: wgpu::TextureFormat| TexUpload {
            label: "terrain-art-placeholder",
            format,
            w: 1,
            h: 1,
            bpp: 0,
        };
        let arr = |format: wgpu::TextureFormat| ph(format).create_layers(device, 4, 1);
        let one = |format: wgpu::TextureFormat| ph(format).create(device);
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("terrain-art"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            address_mode_w: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            anisotropy_clamp: 8,
            ..Default::default()
        });
        (
            arr(wgpu::TextureFormat::Rgba8UnormSrgb),
            arr(wgpu::TextureFormat::Rgba8Unorm),
            one(wgpu::TextureFormat::Rgba8Unorm),
            sampler,
        )
    }

    #[allow(clippy::too_many_arguments)] // one bind group = one entry list
    fn bind_group(
        device: &wgpu::Device,
        bgl: &wgpu::BindGroupLayout,
        uniforms: &wgpu::Buffer,
        height_tex: &wgpu::Texture,
        lut_tex: &wgpu::Texture,
        biome_tex: &wgpu::Texture,
        alb_tex: &wgpu::Texture,
        nrm_tex: &wgpu::Texture,
        water_tex: &wgpu::Texture,
        sampler: &wgpu::Sampler,
    ) -> wgpu::BindGroup {
        let height_view = height_tex.create_view(&wgpu::TextureViewDescriptor::default());
        let lut_view = lut_tex.create_view(&wgpu::TextureViewDescriptor::default());
        let biome_view = biome_tex.create_view(&wgpu::TextureViewDescriptor::default());
        let alb_view = alb_tex.create_view(&wgpu::TextureViewDescriptor::default());
        let nrm_view = nrm_tex.create_view(&wgpu::TextureViewDescriptor::default());
        let water_view = water_tex.create_view(&wgpu::TextureViewDescriptor::default());
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("terrain"),
            layout: bgl,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniforms.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&height_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&lut_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&biome_view),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::TextureView(&alb_view),
                },
                wgpu::BindGroupEntry {
                    binding: 5,
                    resource: wgpu::BindingResource::TextureView(&nrm_view),
                },
                wgpu::BindGroupEntry {
                    binding: 6,
                    resource: wgpu::BindingResource::TextureView(&water_view),
                },
                wgpu::BindGroupEntry {
                    binding: 7,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        })
    }

    fn rebind(&mut self, device: &wgpu::Device) {
        self.bind_group = Self::bind_group(
            device,
            &self.bgl,
            &self.uniforms,
            &self.height_tex,
            &self.lut_tex,
            &self.biome_tex,
            &self.alb_tex,
            &self.nrm_tex,
            &self.water_tex,
            &self.sampler,
        );
    }

    /// Create (on shape change) + upload a data texture in one step.
    fn write_tex(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        old: &wgpu::Texture,
        desc: TexUpload,
        data: &[u8],
    ) -> wgpu::Texture {
        let tex = if old.width() == desc.w && old.height() == desc.h && old.format() == desc.format
        {
            old.clone()
        } else {
            desc.create(device)
        };
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            data,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(desc.w * desc.bpp),
                rows_per_image: Some(desc.h),
            },
            wgpu::Extent3d {
                width: desc.w,
                height: desc.h,
                depth_or_array_layers: 1,
            },
        );
        tex
    }

    /// Upload a [`TerrainTex`] (real art): recreate the texture when
    /// shape changed, then write every (mip level, array layer) slice.
    /// `data` is mip-major — each level holds `layers` images of
    /// `(w>>l)×(h>>l)×4` bytes. Rows narrower than the 256 B copy
    /// alignment get repadded into a staging buffer per level.
    fn write_art_tex(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        tex: &TerrainTex,
        format: wgpu::TextureFormat,
        label: &'static str,
    ) -> wgpu::Texture {
        let desc = TexUpload {
            label,
            format,
            w: tex.w,
            h: tex.h,
            bpp: 4,
        };
        let out = desc.create_layers(device, tex.layers, tex.mips);
        let mut src = 0usize;
        for level in 0..tex.mips {
            let lw = (tex.w >> level).max(1) as usize;
            let lh = (tex.h >> level).max(1) as usize;
            let level_bytes = tex.layers as usize * lw * lh * 4;
            // `bytes_per_row` must meet COPY_BYTES_PER_ROW_ALIGNMENT
            // (256) — repack narrow-mip rows into a padded staging
            // buffer (level sizes are small; this is a one-time cost).
            let row = lw * 4;
            let pad = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize;
            let padded_row = row.div_ceil(pad) * pad;
            let data: Vec<u8> = if padded_row == row {
                tex.data[src..src + level_bytes].to_vec()
            } else {
                let mut v = vec![0u8; tex.layers as usize * padded_row * lh];
                for layer in 0..tex.layers as usize {
                    for y in 0..lh {
                        let s = src + (layer * lh + y) * row;
                        let d = (layer * lh + y) * padded_row;
                        v[d..d + row].copy_from_slice(&tex.data[s..s + row]);
                    }
                }
                v
            };
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &out,
                    mip_level: level,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_row as u32),
                    rows_per_image: Some(lh as u32),
                },
                wgpu::Extent3d {
                    width: lw as u32,
                    height: lh as u32,
                    depth_or_array_layers: tex.layers,
                },
            );
            src += level_bytes;
        }
        out
    }

    /// Keyed upload of the packet's [`TerrainArt`] (all three members
    /// ride one content key). Returns whether the bind group must be
    /// recreated.
    fn upload_art(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        packet: &TerrainPacket,
    ) -> bool {
        let art_key = packet.art.as_ref().map(|a| a.key);
        if art_key == self.uploaded_art_key {
            return false;
        }
        if let Some(a) = &packet.art {
            let (ln, sg) = (
                wgpu::TextureFormat::Rgba8Unorm,
                wgpu::TextureFormat::Rgba8UnormSrgb,
            );
            self.alb_tex = Self::write_art_tex(device, queue, &a.albedo, sg, "terrain-albedo");
            self.nrm_tex = Self::write_art_tex(device, queue, &a.normal, ln, "terrain-normal");
            self.water_tex = Self::write_art_tex(device, queue, &a.water, ln, "terrain-water");
        }
        self.uploaded_art_key = art_key;
        true
    }

    /// Upload the packet's changing pieces (uniforms, instance rows,
    /// any stale textures) and record: sky → opaque terrain, into the
    /// caller's open pass sharing the flight depth target (reverse-Z).
    /// The ocean is the separate [`Self::draw_ocean`] phase — it runs
    /// after the flight stream.
    #[allow(clippy::too_many_lines)] // uploads + two draws; splitting fragments the data flow
    pub fn draw(
        &mut self,
        pass: &mut wgpu::RenderPass<'_>,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        packet: &TerrainPacket,
    ) {
        queue.write_buffer(&self.uniforms, 0, bytemuck::bytes_of(&packet.uniforms));
        // The shared grid is immutable — upload once, lazily.
        if !self.grid_uploaded {
            let (verts, indices) = unit_grid();
            queue.write_buffer(&self.grid_v, 0, bytemuck::cast_slice(&verts));
            queue.write_buffer(&self.grid_i, 0, bytemuck::cast_slice(&indices));
            self.grid_uploaded = true;
        }
        let mut rebind = false;
        if let Some(hf) = &packet.heightfield
            && self.uploaded_height_key != Some(hf.key)
        {
            self.height_tex = Self::write_tex(
                device,
                queue,
                &self.height_tex,
                TexUpload {
                    label: "terrain-height",
                    format: wgpu::TextureFormat::R32Float,
                    w: hf.width,
                    h: hf.height,
                    bpp: 4,
                },
                bytemuck::cast_slice(&hf.data),
            );
            self.uploaded_height_key = Some(hf.key);
            rebind = true;
        }
        if packet.atmo != self.uploaded_atmo {
            let (w, h, data) = match packet.atmo {
                Some(a) => (
                    LUT_W as u32,
                    LUT_H as u32,
                    bake_transmittance(a, RAYLEIGH_BETA),
                ),
                None => (1, 1, vec![1.0f32; 4]),
            };
            self.lut_tex = Self::write_tex(
                device,
                queue,
                &self.lut_tex,
                TexUpload {
                    label: "terrain-lut",
                    format: wgpu::TextureFormat::Rgba32Float,
                    w,
                    h,
                    bpp: 16,
                },
                bytemuck::cast_slice(&data),
            );
            self.uploaded_atmo = packet.atmo;
            rebind = true;
        }
        // Identity by content hash — the field is immutable once built;
        // keying on `BiomeField::key` (like `uploaded_height_key`)
        // can't ABA on a recycled `Arc` address.
        let biome_key = packet.biome.as_ref().map(|b| b.key);
        if biome_key != self.uploaded_biome_key {
            if let Some(b) = &packet.biome {
                self.biome_tex = Self::write_tex(
                    device,
                    queue,
                    &self.biome_tex,
                    TexUpload {
                        label: "terrain-biome",
                        format: wgpu::TextureFormat::R8Uint,
                        w: b.width,
                        h: b.height,
                        bpp: 1,
                    },
                    &b.indices,
                );
            }
            self.uploaded_biome_key = biome_key;
            rebind = true;
        }
        // The real-art set — one key for the three-member pack; the
        // `uniforms.art.x` flag is what actually arms the shader path,
        // so a stale placeholder only costs samples when the packet
        // claims art it doesn't have (which the view never builds).
        if self.upload_art(device, queue, packet) {
            rebind = true;
        }
        if rebind {
            self.rebind(device);
        }

        pass.set_bind_group(0, &self.bind_group, &[]);
        if packet.sky {
            pass.set_pipeline(&self.sky_pipe);
            pass.draw(0..3, 0..1);
        }
        // Terrain + ocean share the mesh — ONE upload, laid out
        // terrain-then-ocean so `draw` slices the terrain range and
        // `draw_ocean` slices the rest (the ocean draws in a second
        // phase after the flight stream, so the two stages never
        // race one buffer).
        let n_terrain = packet.terrain.len();
        let n_inst = n_terrain + packet.ocean.len();
        if n_inst > 0 {
            let mut inst = Vec::with_capacity(n_inst);
            inst.extend_from_slice(&packet.terrain);
            inst.extend_from_slice(&packet.ocean);
            let _ = self.instances.upload(device, queue, &inst);
            self.inst_bytes =
                (n_terrain * std::mem::size_of::<TerrainInstance>()) as wgpu::BufferAddress;
            self.inst_end =
                (n_inst * std::mem::size_of::<TerrainInstance>()) as wgpu::BufferAddress;
        }
        if n_terrain > 0 {
            pass.set_pipeline(&self.terrain_pipe);
            pass.set_vertex_buffer(0, self.grid_v.slice(..));
            pass.set_vertex_buffer(1, self.instances.buffer.slice(0..self.inst_bytes));
            pass.set_index_buffer(self.grid_i.slice(..), wgpu::IndexFormat::Uint16);
            pass.draw_indexed(0..self.grid_index_count, 0, 0..n_terrain as u32);
        }
    }

    /// The ocean phase — call from the same open pass AFTER the flight
    /// vertex stream (app order: `draw` → flight `draw` → `draw_ocean`).
    /// The ocean pipeline tests depth but never writes it: submerged
    /// hull fragments keep their already-rasterized pixels instead of
    /// being occluded by water drawn before them, and the alpha blend
    /// still tints them through the surface. A no-op when `draw` was
    /// not called this frame or no ocean instances were uploaded.
    pub fn draw_ocean(&mut self, pass: &mut wgpu::RenderPass<'_>, packet: &TerrainPacket) {
        if packet.ocean.is_empty() {
            return;
        }
        pass.set_pipeline(&self.ocean_pipe);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_vertex_buffer(0, self.grid_v.slice(..));
        pass.set_vertex_buffer(
            1,
            self.instances.buffer.slice(self.inst_bytes..self.inst_end),
        );
        pass.set_index_buffer(self.grid_i.slice(..), wgpu::IndexFormat::Uint16);
        pass.draw_indexed(0..self.grid_index_count, 0, 0..packet.ocean.len() as u32);
    }
}

impl GridVertex {
    const LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<GridVertex>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32, 2 => Float32],
    };
}

impl TerrainInstance {
    const LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<TerrainInstance>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Instance,
        attributes: &wgpu::vertex_attr_array![
            3 => Float32x4,
            4 => Float32x4,
            5 => Float32x4,
            6 => Float32x4,
            7 => Float32x4
        ],
    };
}

#[cfg(test)]
// Tests signal failure by panicking — §2.13's no-panic rule covers
// shipping fallible paths, not test code.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// The R2 determinism contract: same seed + direction ⇒ bit-identical
    /// height, every run. Also seeds must actually differ.
    #[test]
    fn noise_is_deterministic_and_seeded() {
        let dir = DVec3::new(0.36, 0.48, 0.8);
        let a = height_normalized(601, dir);
        let b = height_normalized(601, dir);
        assert_eq!(a.to_bits(), b.to_bits());
        assert_ne!(height_normalized(601, dir), height_normalized(602, dir));
        // Field sanity: values stay in range and actually vary.
        let mut lo = f64::INFINITY;
        let mut hi = -f64::INFINITY;
        let mut below = 0usize;
        let n = 512usize;
        for i in 0..n {
            let t = 2.0 * core::f64::consts::PI * i as f64 / n as f64;
            let v = DVec3::new(libm::cos(t), libm::sin(t), 0.25);
            let d = v / v.norm();
            let h = height_normalized(601, d);
            lo = lo.min(h);
            hi = hi.max(h);
            if h < 0.0 {
                below += 1;
            }
        }
        assert!(lo >= -1.0 && hi <= 1.0 && lo < hi);
        assert!(below > 0 && below < n); // land AND sea exist
    }

    /// LOD: a ground-level eye subdivides near the nadir far deeper
    /// than a high-orbit eye, and planet-scale coverage always holds.
    #[test]
    fn lod_subdivides_with_distance() {
        let cfg = LodConfig::default();
        let r = 600_000.0;
        let amp = r * 0.004; // the authored Terrin-class amplitude
        let low = select_patches(DVec3::new(r + 100.0, 0.0, 0.0), r, amp, &cfg);
        let high = select_patches(DVec3::new(r * 5.0, 0.0, 0.0), r, amp, &cfg);
        assert!(!low.is_empty() && !high.is_empty());
        let max_low = low.iter().map(|p| p.level).max().unwrap();
        let max_high = high.iter().map(|p| p.level).max().unwrap();
        assert!(max_low > max_high, "low eye should subdivide deeper");
        // Far side culled at ground level: no patch centred past ~120°.
        for p in &low {
            let (dir0, _size) = patch_dir0_size(*p);
            assert!(dir0.dot(DVec3::X) > -0.5);
        }
        // Budget honoured.
        assert!(low.len() <= cfg.max_patches + 64);
    }

    /// §2.3 bake: the f64→f32 patch origin keeps sub-mm precision at
    /// planetary distance when reconstructed.
    #[test]
    fn patch_origin_bake_preserves_precision() {
        let r = 600_000.0;
        let eye = DVec3::new(r + 50.0, 0.0, 0.0);
        let id = PatchId {
            face: 0,
            level: 8,
            x: 137,
            y: 201,
        };
        let inst = patch_instance(id, 0.0, eye, r);
        // Reconstruct the patch origin: origin_rel_eye + eye should
        // reproduce dir0·R to within f32's bound at ~6e5 m (~4 cm).
        let origin = DVec3::new(
            f64::from(inst.origin_rel_eye[0]) + eye.x,
            f64::from(inst.origin_rel_eye[1]) + eye.y,
            f64::from(inst.origin_rel_eye[2]) + eye.z,
        );
        let (dir0, _s) = patch_dir0_size(id);
        let err = (origin - dir0 * r).norm();
        assert!(err < 0.1, "origin reconstruction error {err} m");
        // Cube-space data stays unit-magnitude.
        assert!(inst.corner[..3].iter().all(|c| c.abs() <= 1.0));
    }

    /// CPU mirror of the WGSL `trans` bilinear: the LUT is baked
    /// *endpoint*-centered (`col/(W−1)`, `row/(H−1)`), so the texel
    /// coordinate is `·(d − 1)` — unlike `height_at`'s texel-center
    /// `·d − 0.5`. Keeping this mirror in the test is what locks the
    /// bake and the shader to the same convention.
    fn lut_trans(lut: &[f32], top_m: f64, h: f64, mu: f64) -> [f32; 3] {
        let uu = (mu * 0.5 + 0.5) * (LUT_W - 1) as f64;
        let vv = (h / top_m).clamp(0.0, 1.0) * (LUT_H - 1) as f64;
        let (i0, j0) = (uu.floor() as i64, vv.floor() as i64);
        let (fu, fv) = (uu - i0 as f64, vv - j0 as f64);
        let texel = |i: i64, j: i64| -> [f32; 3] {
            let i = i.clamp(0, LUT_W as i64 - 1) as usize;
            let j = j.clamp(0, LUT_H as i64 - 1) as usize;
            let k = (j * LUT_W + i) * 4;
            [lut[k], lut[k + 1], lut[k + 2]]
        };
        let (wi0, wi1) = (1.0 - fu, fu);
        let (wj0, wj1) = (1.0 - fv, fv);
        let mut out = [0.0f32; 3];
        for (j, wj) in [(j0, wj0), (j0 + 1, wj1)] {
            for (i, wi) in [(i0, wi0), (i0 + 1, wi1)] {
                let t = texel(i, j);
                for c in 0..3 {
                    out[c] += t[c] * (wi * wj) as f32;
                }
            }
        }
        out
    }

    /// The transmittance LUT: unity in space, decreasing with depth,
    /// zero through the ground — and it matches the vertical-ray
    /// closed form `exp(−β·H·e^(−h/H))`.
    #[test]
    fn transmittance_lut_is_sane() {
        let atmo = AtmoDesc {
            radius_m: 600_000.0,
            top_m: 70_000.0,
            scale_height_m: 5_600.0,
        };
        let lut = bake_transmittance(atmo, RAYLEIGH_BETA);
        assert_eq!(lut.len(), LUT_W * LUT_H * 4);
        let t = |row: usize, col: usize| -> [f32; 3] {
            let i = (row * LUT_W + col) * 4;
            [lut[i], lut[i + 1], lut[i + 2]]
        };
        // Top row (space): ~1 everywhere.
        assert!(t(LUT_H - 1, LUT_W / 2).iter().all(|&v| v > 0.99));
        // Ground row, vertical (μ=1, last col): matches closed form.
        let closed = libm::exp(-RAYLEIGH_BETA[0] * atmo.scale_height_m);
        let got = f64::from(t(0, LUT_W - 1)[0]);
        assert!((got - closed).abs() < 0.01, "{got} vs {closed}");
        // Straight down (μ=−1, col 0) hits the datum → 0.
        assert_eq!(t(LUT_H / 2, 0), [0.0, 0.0, 0.0]);

        // The shader↔bake mapping (R16-F3): sampling the shader's
        // `trans` formula AT each baked grid point must return that
        // texel exactly — an off-by-half-texel convention would smear
        // every endpoint (and is exactly the bias this guards).
        for row in (0..LUT_H).step_by(7) {
            for col in (0..LUT_W).step_by(9) {
                let h = atmo.top_m * row as f64 / (LUT_H - 1) as f64;
                let mu = -1.0 + 2.0 * col as f64 / (LUT_W - 1) as f64;
                let s = lut_trans(&lut, atmo.top_m, h, mu);
                let e = t(row, col);
                for c in 0..3 {
                    assert!(
                        (s[c] - e[c]).abs() < 1e-6,
                        "grid ({row},{col}): sampled {s:?} vs texel {e:?}"
                    );
                }
            }
        }
        // And the endpoints the mapping must preserve verbatim:
        // μ=−1 stays the ground-blocked 0, h=top stays ≈1.
        assert_eq!(lut_trans(&lut, atmo.top_m, 0.0, -1.0), [0.0, 0.0, 0.0]);
        assert!(
            lut_trans(&lut, atmo.top_m, atmo.top_m, 0.0)
                .iter()
                .all(|&v| v > 0.99)
        );
    }
}
