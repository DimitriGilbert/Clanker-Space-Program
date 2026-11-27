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
use crate::shadow::{ShadowCore, ShadowDraw, ShadowUniforms};

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
    /// across — 13 on a 600 km body ≈ 146 m, ~9 m vertex spacing on
    /// the 16-cell grid). Hard bound: `PatchId` cell indices are
    /// `u16`, so a level-16 cell is the largest representable —
    /// `x * 2 + dx` wraps past `max_level = 16`.
    pub max_level: u8,
    /// Split while `dist(eye, patch) < split_factor · patch_size`.
    pub split_factor: f64,
    /// Hard cap on emitted patches (BFS order ⇒ coarse coverage wins).
    pub max_patches: usize,
}

impl Default for LodConfig {
    fn default() -> Self {
        Self {
            // M5 §5.2-R2 relief density: 13 halves the finest patch
            // (~293 m → ~146 m) so the Mountain profile's 2048×1024
            // bake (~1.8 km texels at a 600 km body) resolves its
            // ridges instead of smearing them across a patch.
            max_level: 13,
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

/// §2.3 far-field routing bound: a conservative upper bound
/// on the distance from a patch's baked centre (`dir0·R`) to ANY
/// point of its surface — the cube-space half-diagonal `0.5·|du+dv|`
/// scaled to metres, plus the relief amplitude (displacement only
/// rises above the datum; skirts drop inward and their uv is clamped
/// inside the cell, so they stay within the reach too). Routing sends
/// a patch to the far pass only when
/// `dot(centre − eye, fwd) − extent ≥ FAR_SPLIT` — the NEAREST point
/// ALONG THE VIEW AXIS still clears the split (forward depth is what
/// the far pass's clip test `split ≤ clip.w` measures, and it is
/// `radial·cos(off-axis)` — up to ~25 % shorter than the radial
/// distance at the frame's side reaches; forward is 1-Lipschitz, so
/// the bound covers every vertex). No legitimately-routed far quad is
/// ever trimmed away — every leaf is drawn WHOLE by exactly one pass,
/// and the far clip remains a pure staleness safety net that never
/// removes visible surface.
pub fn patch_extent_m(id: PatchId, radius_m: f64, amplitude_m: f64) -> f64 {
    let (_corner, du, dv) = patch_cube(id);
    (du + dv).norm() * 0.5 * radius_m + amplitude_m
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
    /// `z` = per-tile de-repetition kill flag (1.0 makes the shader
    /// sample the pre-4b plain periodic tiling; render-gate
    /// `DUMP_DEREPETITION=0` A/B dumps only — production writes 0),
    /// `w` unused.
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
/// `data/art/terrain.csar` (native; wasm ships `terrain_web.csar`)
/// by `csp-view` (see `assets/ASSETS.md` for provenance). All three
/// members ride one upload key: the pack is all-or-nothing. The night
/// sky is procedural (`fs_sky`'s shell-lattice starfield) — no star
/// texture ships.
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
    /// Frame uniforms (the near pass's projection, near 0.1).
    pub uniforms: TerrainUniforms,
    /// §2.3's far-field uniforms: a byte-copy of `uniforms` whose
    /// `view_proj` carries the far pass's clip.z row `[0,0,0,FAR_SPLIT]`
    /// (see [`crate::flight::FAR_SPLIT_M`] for the algebra — xy and w
    /// rows are identical, so far geometry keeps its screen position
    /// and only the depth remaps). Consumed by
    /// [`TerrainRenderer::draw_far`]; a `Default` row only ever
    /// matters for packets that also carry no far instances.
    pub uniforms_far: TerrainUniforms,
    /// Baked heightfield (absent ⇒ no terrain drawn).
    pub heightfield: Option<Arc<HeightField>>,
    /// Biome tint field (absent ⇒ height-ramp palette only).
    pub biome: Option<Arc<BiomeField>>,
    /// Atmosphere signature — `None` draws the vacuum path.
    pub atmo: Option<AtmoDesc>,
    /// Terrain leaf instances (heightfield-displaced), ordered
    /// near-then-far: `[0..terrain_far_start)` draw in the near pass,
    /// the rest in the far pass.
    pub terrain: Vec<TerrainInstance>,
    /// Index of the first FAR terrain instance (§2.3 routing — patch
    /// centre at or beyond [`crate::flight::FAR_SPLIT_M`]).
    pub terrain_far_start: u32,
    /// Ocean leaf instances (datum-clamped surface), near-then-far
    /// like `terrain`.
    pub ocean: Vec<TerrainInstance>,
    /// Index of the first FAR ocean instance.
    pub ocean_far_start: u32,
    /// Draw the sky backdrop pass (§2.3: the sky belongs to the FAR
    /// pass — it writes no depth and is the backdrop of everything).
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
/// `R8Uint`), the real-art set (albedo/normal `RGBA8` arrays, the
/// water normal map + the filtering sampler) — each re-uploaded only
/// when its packet key changes — plus the R1 shadow-cascade core
/// ([`ShadowCore`]; armed since 3b — the frame loop calls
/// [`Self::draw_shadow`] before the scene pass whenever the flight
/// packet's cascade row is solved).
pub struct TerrainRenderer {
    terrain_pipe: wgpu::RenderPipeline,
    /// Ocean pipeline — same shaders/mesh as `terrain_pipe`, depth
    /// test-only, drawn after the flight stream (see `pipelines`).
    ocean_pipe: wgpu::RenderPipeline,
    sky_pipe: wgpu::RenderPipeline,
    bgl: wgpu::BindGroupLayout,
    bind_group: wgpu::BindGroup,
    /// §2.3's far-field set: same textures, the far uniform row —
    /// swapped in for the far instance draws (the pipelines
    /// themselves are shared; only the clip.z row differs).
    bind_group_far: wgpu::BindGroup,
    uniforms: wgpu::Buffer,
    uniforms_far: wgpu::Buffer,
    grid_v: wgpu::Buffer,
    grid_i: wgpu::Buffer,
    grid_index_count: u32,
    grid_uploaded: bool,
    instances: GpuBuffer,
    /// Instance-buffer layout, §2.3 near/far partitioned (one upload,
    /// four contiguous segments):
    /// `[0..inst_far)` near terrain, `[inst_far..inst_bytes)` far
    /// terrain, `[inst_bytes..inst_ocean_far)` near ocean,
    /// `[inst_ocean_far..inst_end)` far ocean. Set by
    /// [`Self::upload_instances`], sliced by `draw`/`draw_far`/
    /// `draw_ocean`; `draw_shadow` culls per cascade (G3,
    /// [`crate::shadow::cascade_runs`]) and draws kept runs out of
    /// the whole `[0..inst_bytes)` terrain span — near-list only at
    /// the home body (the far-list exclusion proof lives at
    /// `cascade_runs`).
    inst_far: wgpu::BufferAddress,
    /// Byte range of the ocean slice start (terrain end) — set by
    /// `draw`, consumed by `draw_ocean`.
    inst_bytes: wgpu::BufferAddress,
    inst_ocean_far: wgpu::BufferAddress,
    inst_end: wgpu::BufferAddress,
    /// Content key of the last instance upload. `sync_resources` runs
    /// up to three times per frame (shadow → far → near phases) over
    /// the SAME packet — identical bytes — so the 2nd/3rd staging
    /// writes are pure overhead; keying on content folds them into
    /// one upload per frame while a genuinely new selection (camera
    /// move) always re-uploads.
    inst_key: u64,
    /// G3+ fold memo — the KEY COMPUTATION above was itself the
    /// remaining 3×/frame cost (~30.5k `mix64` over ~244 KB of
    /// instances per sync, ×3): within one frame the packet is a
    /// single immutable borrow, so (frame stamp + slice ptr/len +
    /// near/far boundary counts) identity proves the bytes unchanged
    /// and the 2nd/3rd syncs reuse the folded key outright. The
    /// stamp is bumped once per frame by [`Self::draw_shadow`] — the
    /// documented frame leader (the app and the render gate both
    /// lead every terrain frame with it, armed or not) — which is
    /// what separates "same packet later in this frame" from "a
    /// fresh packet next frame that recycled the allocation": the
    /// stamp differs, the fold reruns, the ABA trap never fires.
    /// INVALIDATION CONTRACT: a caller that renders terrain phases
    /// WITHOUT a preceding `draw_shadow` in the same frame (no such
    /// caller exists today) and hands back a same-shape packet at a
    /// recycled address would ride a stale key — such a caller must
    /// bump the stamp itself (or lead with `draw_shadow`).
    fold_memo: Option<InstanceFoldMemo>,
    /// The frame stamp `fold_memo` keys on (see there); bumped once
    /// per frame by `draw_shadow`'s body.
    frame_stamp: u64,
    /// Test-only observable: how many times `draw_shadow` actually ran
    /// the [`crate::shadow::cascade_runs`] solve (the invalidation
    /// test's counter — `FlightGeomCache`'s `smooth_bakes` precedent).
    /// Stays 0 across disarmed/empty-terrain frames: those never read
    /// run lists, so the solve is skipped for them.
    #[cfg(test)]
    cull_solves: u32,
    /// Cascade-map resources (the `SU` uniform entry at binding 8,
    /// the receiving group(1), + the depth passes).
    shadow: ShadowCore,
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

/// The identity half of [`TerrainRenderer::fold_memo`]: everything
/// about a packet's instance lists that determines the fold INPUT,
/// short of the bytes themselves — sound only combined with the
/// frame stamp (same frame ⇒ one immutable packet ⇒ same bytes).
/// Raw slice addresses are carried as `usize` so the renderer stays
/// `Send` (raw pointers would poison it).
#[derive(Clone, Copy, Debug)]
struct InstanceFoldMemo {
    /// The frame the fold ran in (`TerrainRenderer::frame_stamp`).
    stamp: u64,
    /// `packet.terrain` slice address + length.
    terrain: (usize, usize),
    /// `packet.ocean` slice address + length.
    ocean: (usize, usize),
    /// Near-count boundaries (`min`-clamped, exactly what
    /// `upload_instances` segments by).
    terrain_near: u32,
    ocean_near: u32,
    /// The folded content key this identity produced.
    key: u64,
}

impl InstanceFoldMemo {
    /// Same fold INPUTS (identity only — the key rides along).
    fn same_inputs(&self, other: &Self) -> bool {
        self.stamp == other.stamp
            && self.terrain == other.terrain
            && self.ocean == other.ocean
            && self.terrain_near == other.terrain_near
            && self.ocean_near == other.ocean_near
    }
}

impl TerrainRenderer {
    /// The shared bind-group layout: 0 uniforms, 1 heightfield,
    /// 2 transmittance LUT, 3 biome slots — all `textureLoad`
    /// (non-filterable formats keep every backend happy). Bindings 4–6
    /// are the real-art set (albedo/normal `D2Array`, water normal `D2`,
    /// all filterable `RGBA8`) plus the filtering sampler at 7 — they
    /// carry 1×1 placeholders until a packet uploads art. Binding 8 is
    /// the R1 cascade `SU` uniform (phase 3a — both the receiving
    /// `fs_terrain` and the depth-pass vertices `vs_shadow0/1` read
    /// it; the cascade depth array + comparison sampler live in their
    /// own group(1), which the shadow passes never bind — see
    /// `ShadowCore`).
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
                // The cascade uniform — core WebGPU types, no features.
                wgpu::BindGroupLayoutEntry {
                    binding: 8,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
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
        //
        // Premultiplied: the surface is a thin layer over the seabed or
        // hull already in the target, and `fs_terrain` tone-maps the
        // layer's own light (`col × alpha`) — with straight alpha it
        // could only tone-map the unweighted layer colour and weight it
        // after, which lifts a 2 % sky reflection into 2 % of the
        // tone-mapped sky.
        let ocean_targets = [Some(wgpu::ColorTargetState {
            format: surface_format,
            blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
            write_mask: wgpu::ColorWrites::ALL,
        })];
        let ocean = pipe(
            "ocean",
            "vs_terrain",
            "fs_terrain",
            &[Some(GridVertex::LAYOUT), Some(TerrainInstance::LAYOUT)],
            &ocean_targets,
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
    #[allow(clippy::too_many_lines)] // one device-resource construction; splitting fragments the descriptor set
    pub fn new(device: &wgpu::Device, surface_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("terrain"),
            source: wgpu::ShaderSource::Wgsl(include_str!("terrain.wgsl").into()),
        });
        let bgl = Self::bind_group_layout(device);
        // The shadow core owns the cascade group(1) layout (depth
        // array + comparison sampler) — the visible pipelines list it
        // second; the shadow depth pipelines ride group(0) alone so
        // their passes never bind the texture they render into.
        let shadow = ShadowCore::new(
            device,
            &shader,
            &bgl,
            &[Some(GridVertex::LAYOUT), Some(TerrainInstance::LAYOUT)],
        );
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("terrain"),
            bind_group_layouts: &[Some(&bgl), Some(shadow.sample_layout())],
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
        // §2.3's far uniform row — same size/layout, written with the
        // far clip.z matrix by `sync_resources` every frame.
        let uniforms_far = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("terrain-uniforms-far"),
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
            &shadow,
        );
        let bind_group_far = Self::bind_group(
            device,
            &bgl,
            &uniforms_far,
            &height_tex,
            &lut_tex,
            &biome_tex,
            &alb_tex,
            &nrm_tex,
            &water_tex,
            &sampler,
            &shadow,
        );
        Self {
            terrain_pipe,
            ocean_pipe,
            sky_pipe,
            bgl,
            bind_group,
            bind_group_far,
            uniforms,
            uniforms_far,
            grid_v,
            grid_i,
            grid_index_count: indices.len() as u32,
            grid_uploaded: false,
            instances: GpuBuffer::new(
                device,
                "terrain-instances",
                (std::mem::size_of::<TerrainInstance>() * 4096) as u64,
            ),
            inst_far: 0,
            inst_bytes: 0,
            inst_ocean_far: 0,
            inst_end: 0,
            inst_key: 0,
            fold_memo: None,
            frame_stamp: 0,
            #[cfg(test)]
            cull_solves: 0,
            shadow,
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
        shadow: &ShadowCore,
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
                wgpu::BindGroupEntry {
                    binding: 8,
                    resource: shadow.uniforms_ref().as_entire_binding(),
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
            &self.shadow,
        );
        // The far set re-binds the SAME textures — only its uniform
        // buffer differs, so both groups must track every texture
        // swap together or the far pass would sample stale data.
        self.bind_group_far = Self::bind_group(
            device,
            &self.bgl,
            &self.uniforms_far,
            &self.height_tex,
            &self.lut_tex,
            &self.biome_tex,
            &self.alb_tex,
            &self.nrm_tex,
            &self.water_tex,
            &self.sampler,
            &self.shadow,
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

    /// The shared grid is immutable — upload once, lazily (the first
    /// of `draw`/`draw_shadow` to run pays it).
    fn ensure_grid(&mut self, queue: &wgpu::Queue) {
        if !self.grid_uploaded {
            let (verts, indices) = unit_grid();
            queue.write_buffer(&self.grid_v, 0, bytemuck::cast_slice(&verts));
            queue.write_buffer(&self.grid_i, 0, bytemuck::cast_slice(&indices));
            self.grid_uploaded = true;
        }
    }

    /// Terrain + ocean share the mesh — ONE upload per frame, laid out
    /// near-terrain / far-terrain / near-ocean / far-ocean (§2.3's
    /// partition) so `draw` slices the near ranges, `draw_far` the far
    /// ranges, and `draw_ocean` the near ocean (the ocean draws in a
    /// later phase after the flight stream, so the stages never race
    /// one buffer). The packet's lists already arrive near-then-far;
    /// the boundary counts are read defensively (clamped, explicitly
    /// re-segmented) so a mis-ordered packet can never desynchronize
    /// the byte ranges from the draw slicing. `sync_resources` runs
    /// for the shadow, far and near phases over the same packet, so
    /// the upload itself is keyed on CONTENT (a `mix64` fold) — and
    /// since G3+ the FOLD is memoized per frame (`fold_memo`: the
    /// first sync of a frame folds, the repeats reuse the key; the
    /// concat + fold was ~3×30.5k `mix64` over ~244 KB per frame),
    /// while a genuinely new selection (camera move) always
    /// re-uploads.
    fn upload_instances(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        packet: &TerrainPacket,
    ) {
        let sz = std::mem::size_of::<TerrainInstance>() as wgpu::BufferAddress;
        let n_terrain = packet.terrain.len();
        let n_terrain_near = (packet.terrain_far_start as usize).min(n_terrain);
        let n_ocean = packet.ocean.len();
        let n_ocean_near = (packet.ocean_far_start as usize).min(n_ocean);
        let n_inst = n_terrain + n_ocean;
        // The byte ranges refresh unconditionally (cheap field writes
        // a same-content packet sets idempotently); zero them on the
        // empty packet so no later draw slices a previous frame's bytes.
        self.inst_far = (n_terrain_near as wgpu::BufferAddress) * sz;
        self.inst_bytes = (n_terrain as wgpu::BufferAddress) * sz;
        self.inst_ocean_far = self.inst_bytes + (n_ocean_near as wgpu::BufferAddress) * sz;
        self.inst_end = (n_inst as wgpu::BufferAddress) * sz;
        // The fold memo: within THIS frame (same stamp) the same
        // slices are the same immutable packet — reuse the key and
        // skip the concat + fold entirely. See `fold_memo`'s doc for
        // the stamp's invalidation contract.
        let probe = InstanceFoldMemo {
            stamp: self.frame_stamp,
            terrain: (packet.terrain.as_ptr() as usize, n_terrain),
            ocean: (packet.ocean.as_ptr() as usize, n_ocean),
            terrain_near: n_terrain_near as u32,
            ocean_near: n_ocean_near as u32,
            key: 0,
        };
        if let Some(memo) = self.fold_memo
            && memo.same_inputs(&probe)
        {
            self.inst_key = memo.key;
            return;
        }
        if n_inst == 0 {
            self.inst_key = 0;
            self.fold_memo = None;
            return;
        }
        let mut inst = Vec::with_capacity(n_inst);
        inst.extend_from_slice(&packet.terrain[..n_terrain_near]);
        inst.extend_from_slice(&packet.terrain[n_terrain_near..]);
        inst.extend_from_slice(&packet.ocean[..n_ocean_near]);
        inst.extend_from_slice(&packet.ocean[n_ocean_near..]);
        let bytes: &[u8] = bytemuck::cast_slice(&inst);
        let mut key = mix64(bytes.len() as u64);
        for c in bytes.chunks(8) {
            let mut w = 0u64;
            for (k, b) in c.iter().rev().enumerate() {
                w |= u64::from(*b) << (8 * k);
            }
            key = mix64(key ^ w);
        }
        if key != self.inst_key {
            let _ = self.instances.upload(device, queue, &inst);
            self.inst_key = key;
        }
        self.fold_memo = Some(InstanceFoldMemo { key, ..probe });
    }

    /// Upload the packet's changing pieces and refresh the bind
    /// group — the shared prefix of [`Self::draw_shadow`] (which runs
    /// FIRST in the frame) and [`Self::draw`]. Keyed (content-hash /
    /// shape) so the second call in a frame is just the uniform +
    /// instance re-write. Without this the cascade passes would
    /// displace against the previous frame's heightfield — or the
    /// 1×1 placeholders on the very first frame — and rasterize a
    /// stale or empty map while the visible pass draws the real
    /// surface (de-registered casters/receivers by whole ridges).
    #[allow(clippy::too_many_lines)] // uploads; splitting fragments the data flow
    fn sync_resources(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        packet: &TerrainPacket,
    ) {
        queue.write_buffer(&self.uniforms, 0, bytemuck::bytes_of(&packet.uniforms));
        // §2.3: the far row rides beside the near one every frame —
        // the far pass draws from the same sync, never a stale
        // matrix after a camera move.
        queue.write_buffer(
            &self.uniforms_far,
            0,
            bytemuck::bytes_of(&packet.uniforms_far),
        );
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
        self.upload_instances(device, queue, packet);
    }

    /// Upload the packet's changing pieces (uniforms, instance rows,
    /// any stale textures) and record the §2.3 NEAR terrain draw —
    /// `[0..terrain_far_start)` instances through the near projection
    /// — into the caller's open pass sharing the flight depth target
    /// (reverse-Z). The sky and the far instances draw in the frame's
    /// FIRST pass via [`Self::draw_far`]; the near ocean is the
    /// separate [`Self::draw_ocean`] phase after the flight stream.
    pub fn draw(
        &mut self,
        pass: &mut wgpu::RenderPass<'_>,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        packet: &TerrainPacket,
    ) {
        self.ensure_grid(queue);
        self.sync_resources(device, queue, packet);

        let sz = std::mem::size_of::<TerrainInstance>() as wgpu::BufferAddress;
        let n_near = ((self.inst_far / sz) as u32).min(packet.terrain.len() as u32);
        if n_near == 0 {
            return;
        }
        pass.set_bind_group(0, &self.bind_group, &[]);
        // Cascade receiving set (group 1: depth array + comparison
        // sampler). Disarmed sessions sample a zeroed map through a
        // zeroed `SU` — the armed flag keeps the path dead.
        pass.set_bind_group(1, self.shadow.sample_bind(), &[]);
        pass.set_pipeline(&self.terrain_pipe);
        pass.set_vertex_buffer(0, self.grid_v.slice(..));
        pass.set_vertex_buffer(1, self.instances.buffer.slice(0..self.inst_far));
        pass.set_index_buffer(self.grid_i.slice(..), wgpu::IndexFormat::Uint16);
        pass.draw_indexed(0..self.grid_index_count, 0, 0..n_near);
    }

    /// §2.3's far-field phase — record into the frame's FIRST render
    /// pass (the far pass, sharing the flight depth attachment with
    /// its own 0.0 clear): the sky backdrop first (it writes no depth
    /// — `Always` compare, no write — so it can never occlude, and it
    /// reads no `view_proj`, which is why it draws under the NEAR
    /// uniform group), then the far terrain instances and the far
    /// ocean through the FAR uniform row (`z = FAR_SPLIT/d`). Routing
    /// (the packet's `*_far_start` counts, built with
    /// [`patch_extent_m`]) sends only patches whose forward-nearest
    /// point clears the split here, so the far pass's clip test
    /// `split ≤ clip.w` is a pure staleness safety net — it never
    /// trims visible surface, because no legitimately-routed far quad
    /// reaches nearer than the split along the view axis. The near
    /// pass that follows clears the depth again and draws on top:
    /// routed-far quads lie wholly beyond the split, so near-over-far
    /// is the correct winner everywhere except a near-routed
    /// STRADDLER's beyond-split fringe
    /// (the grazing-band approximation documented at the routing site
    /// in `csp-view`), and no submission order inside either pass can
    /// change a cross-pass winner. No-op draws (no sky, empty far
    /// ranges) record nothing — a gameplay-range frame costs one
    /// empty pass, not a second terrain draw.
    pub fn draw_far(
        &mut self,
        pass: &mut wgpu::RenderPass<'_>,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        packet: &TerrainPacket,
    ) {
        self.ensure_grid(queue);
        self.sync_resources(device, queue, packet);

        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_bind_group(1, self.shadow.sample_bind(), &[]);
        if packet.sky {
            pass.set_pipeline(&self.sky_pipe);
            pass.draw(0..3, 0..1);
        }
        let sz = std::mem::size_of::<TerrainInstance>() as wgpu::BufferAddress;
        let n_far_terrain =
            (((self.inst_bytes - self.inst_far) / sz) as u32).min(packet.terrain.len() as u32);
        if n_far_terrain > 0 {
            pass.set_bind_group(0, &self.bind_group_far, &[]);
            pass.set_pipeline(&self.terrain_pipe);
            pass.set_vertex_buffer(0, self.grid_v.slice(..));
            pass.set_vertex_buffer(
                1,
                self.instances.buffer.slice(self.inst_far..self.inst_bytes),
            );
            pass.set_index_buffer(self.grid_i.slice(..), wgpu::IndexFormat::Uint16);
            pass.draw_indexed(0..self.grid_index_count, 0, 0..n_far_terrain);
        }
        let n_far_ocean =
            (((self.inst_end - self.inst_ocean_far) / sz) as u32).min(packet.ocean.len() as u32);
        if n_far_ocean > 0 {
            // Same depth-test-only policy as the near ocean phase —
            // far water tints what the far terrain pass already
            // rasterized beneath it without writing depth. Buffer 0
            // (the shared grid) is re-set: an ocean-only far packet
            // never runs the terrain branch above, and the ocean
            // pipeline requires it (a validation error otherwise).
            pass.set_pipeline(&self.ocean_pipe);
            pass.set_bind_group(0, &self.bind_group_far, &[]);
            pass.set_vertex_buffer(0, self.grid_v.slice(..));
            pass.set_vertex_buffer(
                1,
                self.instances
                    .buffer
                    .slice(self.inst_ocean_far..self.inst_end),
            );
            pass.set_index_buffer(self.grid_i.slice(..), wgpu::IndexFormat::Uint16);
            pass.draw_indexed(0..self.grid_index_count, 0, 0..n_far_ocean);
        }
    }

    /// The R1 shadow phase (PLAN §5.2): rasterize the packet's
    /// terrain instances — and, when `cast` carries the staged flight
    /// stream (3b), the craft/launch-complex depth draw — into the
    /// two cascade depth layers, BEFORE the scene pass records its
    /// draws — `fs_terrain` and the flight `fs_main` sample the
    /// layers through group(1) during [`Self::draw`] / the flight
    /// `draw`. Call order in the frame loop: `draw_shadow` → `draw`
    /// → flight `draw` → `draw_ocean`. The `SU` uniform is written
    /// EVERY call, armed or not: a skipped disarm would leave a stale
    /// armed buffer driving receivers through dead matrices after
    /// sunset or a mode switch. A packet with NO terrain instances
    /// (the sky-only heightfield-bake wait, M5 §5.2-R2) is disarmed
    /// in the buffer regardless of the handed row — the passes would
    /// early-return without clearing the depth array, and receivers
    /// would sample its stale contents (zeros on a fresh session, the
    /// previous body's map after an SOI switch) through live
    /// matrices, silently killing the craft's sun term. Beyond the
    /// write this is a no-op unless the (effective) row is armed AND
    /// the packet carries terrain instances (the zeroed `SU` keeps
    /// `fs_terrain` on the `sun_occlusion` ray-march, bit-identical
    /// to the pre-cascade build) — and the G3 cull solve itself is
    /// gated on the same predicate, so a disarmed frame pays neither
    /// the passes nor the projections. Ocean instances deliberately do
    /// not cast (water receives shadows only — the ray-march
    /// fallback's semantics).
    ///
    /// G3: the armed path culls per cascade — each layer draws only
    /// the instance runs whose patch could intersect that cascade's
    /// ortho box ([`crate::shadow::cascade_runs`]; the pre-cull build
    /// rasterized every instance, near and far, into both 2048²
    /// layers). The cull is conservative, so an in-range scene's map
    /// is byte-identical to the unculled draw (pinned by device
    /// test), and an all-culled layer leaves its map at the fully-lit
    /// clear ([`ShadowCore::draw_terrain`]'s empty-layer contract).
    pub fn draw_shadow(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        packet: &TerrainPacket,
        shadow: &ShadowUniforms,
        cast: Option<(wgpu::Buffer, u32)>,
    ) {
        // P1: solve only when the passes will read the runs. The
        // effective row is exactly `shadow_passes`' rule below (an
        // unarmed row, or the implicit disarm an empty terrain list
        // forces) — computing it here skips the 2×N keep-projections
        // that night/sky-only frames with live terrain otherwise pay
        // for run lists nothing draws from. The disarmed path hands
        // empty runs (`shadow_passes` returns before touching them)
        // and still bumps the stamp + writes the uniform row.
        let armed = shadow.armed() && !packet.terrain.is_empty();
        let runs = if armed {
            #[cfg(test)]
            {
                self.cull_solves = self.cull_solves.wrapping_add(1);
            }
            crate::shadow::cascade_runs(
                shadow,
                &packet.terrain,
                packet.terrain_far_start,
                &packet.uniforms.planet,
            )
        } else {
            [Vec::new(), Vec::new()]
        };
        self.shadow_passes(
            encoder,
            device,
            queue,
            packet,
            shadow,
            cast,
            [&runs[0], &runs[1]],
        );
    }

    /// The test-only UNCULLED control — G3's byte-identity pin needs
    /// the pre-cull behavior (one full-batch draw per layer, both
    /// passes recorded unconditionally) from the same build: the A/B
    /// pair `draw_shadow` vs this must produce byte-identical
    /// cascade maps (the cull may only drop instances that project
    /// no fragment).
    #[cfg(test)]
    pub(crate) fn draw_shadow_unculled(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        packet: &TerrainPacket,
        shadow: &ShadowUniforms,
        cast: Option<(wgpu::Buffer, u32)>,
    ) {
        let full = crate::shadow::InstanceRuns::from([(0u32, packet.terrain.len() as u32)]);
        self.shadow_passes(encoder, device, queue, packet, shadow, cast, [&full, &full]);
    }

    /// `draw_shadow`'s body past the cull solve (shared with the
    /// test-only unculled control so the A/B differs in NOTHING but
    /// the run lists). Also the frame leader: the FIRST line bumps
    /// `frame_stamp` regardless of the armed row or packet contents,
    /// so the per-frame `fold_memo` in `upload_instances` sees a new
    /// stamp exactly once per terrain frame (see its invalidation
    /// contract).
    #[allow(clippy::too_many_arguments)] // the encoder/device/queue plumbing triple + the phase's own packet/row/cast/runs
    fn shadow_passes(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        packet: &TerrainPacket,
        shadow: &ShadowUniforms,
        cast: Option<(wgpu::Buffer, u32)>,
        runs: [&crate::shadow::InstanceRuns; crate::shadow::CASCADE_COUNT],
    ) {
        self.frame_stamp = self.frame_stamp.wrapping_add(1);
        // The sky-only guard: no terrain instances ⇒ the effective row
        // is the disarm row, whatever the caller solved (see the doc —
        // the never-cleared depth array through live matrices is the
        // silent craft-darkening failure the guard exists to make
        // unproducible).
        let disarmed = ShadowUniforms::default();
        let row = if packet.terrain.is_empty() {
            &disarmed
        } else {
            shadow
        };
        self.shadow.write_uniforms(queue, row);
        if !row.armed() {
            return;
        }
        self.ensure_grid(queue);
        // The depth vertex stage displaces through the SAME uniforms
        // + heightfield the visible pass will read a moment later —
        // sync them now (this call leads the frame), or the map would
        // rasterize against stale/placeholder data (see
        // `sync_resources`).
        self.sync_resources(device, queue, packet);
        self.shadow.draw_terrain(
            encoder,
            ShadowDraw {
                bind_group: &self.bind_group,
                grid_v: self.grid_v.slice(..),
                grid_i: self.grid_i.slice(..),
                instances: self.instances.buffer.slice(0..self.inst_bytes),
                index_count: self.grid_index_count,
                runs,
                flight: cast,
            },
        );
    }

    /// 3b cascade hand-off (R1): the `SU` uniform buffer + the
    /// receiving group(1) layout + bind group — everything the flight
    /// renderer needs to cast into and receive from the same cascades
    /// this renderer owns. `FlightRenderer::new` consumes the tuple.
    pub fn cascade_resources(&self) -> (&wgpu::Buffer, &wgpu::BindGroupLayout, &wgpu::BindGroup) {
        self.shadow.cascade_resources()
    }

    /// The cascade depth-array texture — diagnostic readback handle
    /// (the array carries `COPY_SRC` for exactly this). The device
    /// test suite uses it to pin that casts actually LAND in the map:
    /// the silent-discard failure mode leaves every layer at the 1.0
    /// clear and no validation error is ever raised.
    #[cfg(test)]
    pub(crate) fn cascade_texture(&self) -> &wgpu::Texture {
        self.shadow.cascade_texture()
    }

    /// The ocean phase — call from the near pass AFTER the flight
    /// vertex stream (app order: `draw_far` → `draw` → flight `draw`
    /// → `draw_ocean`). §2.3 slices the NEAR ocean range here; the
    /// far ocean draws inside [`Self::draw_far`]. The ocean pipeline
    /// tests depth but never writes it: submerged hull fragments keep
    /// their already-rasterized pixels instead of being occluded by
    /// water drawn before them, and the alpha blend still tints them
    /// through the surface. A no-op when `draw` was not called this
    /// frame or no near ocean instances were uploaded.
    pub fn draw_ocean(&mut self, pass: &mut wgpu::RenderPass<'_>, packet: &TerrainPacket) {
        let sz = std::mem::size_of::<TerrainInstance>() as wgpu::BufferAddress;
        let n_near =
            (((self.inst_ocean_far - self.inst_bytes) / sz) as u32).min(packet.ocean.len() as u32);
        if n_near == 0 {
            return;
        }
        pass.set_pipeline(&self.ocean_pipe);
        pass.set_bind_group(0, &self.bind_group, &[]);
        // Self-sufficient cascade set (draw normally sets it earlier
        // in the same pass; re-setting is cheap and keeps this phase
        // valid standalone — the ocean pipeline's layout declares
        // group(1) too, since `fs_terrain` samples the cascades for
        // water as well).
        pass.set_bind_group(1, self.shadow.sample_bind(), &[]);
        pass.set_vertex_buffer(0, self.grid_v.slice(..));
        pass.set_vertex_buffer(
            1,
            self.instances
                .buffer
                .slice(self.inst_bytes..self.inst_ocean_far),
        );
        pass.set_index_buffer(self.grid_i.slice(..), wgpu::IndexFormat::Uint16);
        pass.draw_indexed(0..self.grid_index_count, 0, 0..n_near);
    }
}

impl GridVertex {
    pub(crate) const LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
        array_stride: std::mem::size_of::<GridVertex>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32, 2 => Float32],
    };
}

impl TerrainInstance {
    pub(crate) const LAYOUT: wgpu::VertexBufferLayout<'static> = wgpu::VertexBufferLayout {
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

    /// C16-F3's rule for what the build cannot share: the water block
    /// is one text in `terrain.wgsl` and `flight.wgsl` — from its
    /// heading to the end of `water_path` — so a seabed and a hull on
    /// it are absorbed by the same numbers. The build has no WGSL
    /// include; this is the include.
    #[test]
    fn water_block_is_one_text_in_both_shaders() {
        fn block(src: &str) -> &str {
            let from = src.find("// --- water ---").expect("a water block");
            let last = from + src[from..].find("fn water_path").expect("`water_path`");
            let to = last + src[last..].find("\n}\n").expect("`water_path`'s end") + 3;
            &src[from..to]
        }
        let terrain = block(include_str!("terrain.wgsl"));
        let flight = block(include_str!("flight.wgsl"));
        assert_eq!(terrain, flight);
        assert!(terrain.starts_with("// --- water ---") && terrain.ends_with("\n}\n"));
    }

    /// §2.3's far-field draw on a real device (the pin the split-ring
    /// gate's diagnosis demanded): a 600 km flat-datum body viewed
    /// (a) from 2 Mm straight down — every leaf wholly beyond the
    /// split — and (b) from 800 km pitched 1.05 rad — the split ring
    /// crosses the disc, both bands populated. For each vantage the
    /// disc renders through `draw_far` (far pass, far uniform row)
    /// and, as the control, the SAME far-routed instances through
    /// `draw_far` with the NEAR uniform row: the far row must paint
    /// every pixel the near one does — byte-identical — which is
    /// exactly what a broken far row (a collapsed displacement, a
    /// dead bind group, a mis-sliced range, an over-eager clip)
    /// cannot do. Skips without an adapter (CI sets
    /// CSP_RENDER_GATE_REQUIRED to make the skip a failure — the
    /// render gate's contract).
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    #[allow(clippy::too_many_lines)] // one device frame recipe per arm; splitting hides the A/B
    fn far_field_draw_paints_the_disc_on_device() {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let Ok(adapter) =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                compatible_surface: None,
                ..wgpu::RequestAdapterOptions::default()
            }))
        else {
            assert!(
                !std::env::var("CSP_RENDER_GATE_REQUIRED").is_ok_and(|v| !v.is_empty() && v != "0"),
                "far-field device test: CSP_RENDER_GATE_REQUIRED is set but no headless \
                 GPU adapter exists"
            );
            eprintln!("far-field device test: SKIP — no headless GPU adapter");
            return;
        };
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("far-field-test"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            ..wgpu::DeviceDescriptor::default()
        }))
        .unwrap();
        let radius = 600_000.0;
        let lod = LodConfig {
            max_level: 8,
            split_factor: 2.0,
            max_patches: 512,
        };
        let field = HeightField::bake_profiled(7, csp_core::terrain::ReliefProfile::Legacy);
        // (alt above datum, pitch below the horizontal); case (a)
        // looks straight down the nadir, case (b) across the ring.
        for (label, alt, pitch) in [
            (
                "straight-down 2 Mm",
                2_000_000.0,
                std::f64::consts::FRAC_PI_2,
            ),
            ("split-ring 800 km", 800_000.0, 1.05),
        ] {
            // Camera basis, the gate probe's pole construction: up =
            // +z, north = +y, fwd pitched `pitch` below the horizon.
            let eye = DVec3::new(0.0, 0.0, radius + alt);
            let (sp, cp) = pitch.sin_cos();
            let fwd = DVec3::new(0.0, cp, -sp);
            let z_out = -fwd;
            let right = DVec3::new(1.0, 0.0, 0.0);
            let upv = z_out.cross(right);
            let view = [
                [right.x, right.y, right.z, 0.0],
                [upv.x, upv.y, upv.z, 0.0],
                [z_out.x, z_out.y, z_out.z, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ];
            let aspect = 1.0;
            let f = 1.0 / (0.9f64 * 0.5).tan();
            let proj = [
                [f / aspect, 0.0, 0.0, 0.0],
                [0.0, f, 0.0, 0.0],
                [0.0, 0.0, 0.0, 0.1],
                [0.0, 0.0, -1.0, 0.0],
            ];
            let mut near_vp = [[0.0f64; 4]; 4];
            for r in 0..4 {
                for c in 0..4 {
                    near_vp[r][c] = proj[r][0] * view[0][c]
                        + proj[r][1] * view[1][c]
                        + proj[r][2] * view[2][c]
                        + proj[r][3] * view[3][c];
                }
            }
            let mut far_vp = near_vp;
            far_vp[2] = [0.0, 0.0, 0.0, crate::flight::FAR_SPLIT_M];
            let wgsl_m = |m: &[[f64; 4]; 4]| {
                let mut o = [[0.0f32; 4]; 4];
                for r in 0..4 {
                    for c in 0..4 {
                        o[c][r] = m[r][c] as f32;
                    }
                }
                o
            };
            // Leaves + the build_packet routing rule (forward-nearest
            // point ≥ split), near-then-far.
            let leaves = select_patches(eye, radius, 0.0, &lod);
            let mut near_l = Vec::new();
            let mut far_l = Vec::new();
            for &id in &leaves {
                let inst = patch_instance(id, 0.0, eye, radius);
                let o = inst.origin_rel_eye;
                let fwd_d =
                    f64::from(o[0]) * fwd.x + f64::from(o[1]) * fwd.y + f64::from(o[2]) * fwd.z;
                if fwd_d - patch_extent_m(id, radius, 0.0) >= crate::flight::FAR_SPLIT_M {
                    far_l.push(inst);
                } else {
                    near_l.push(inst);
                }
            }
            let terrain_far_start = near_l.len() as u32;
            let mut terrain = near_l;
            terrain.extend(far_l);
            assert!(!terrain.is_empty(), "{label}: leaves must exist");
            if alt >= 2_000_000.0 {
                assert_eq!(
                    terrain_far_start, 0,
                    "{label}: every leaf routes far (the case's premise)"
                );
            } else {
                assert!(
                    terrain_far_start > 0 && (terrain_far_start as usize) < terrain.len(),
                    "{label}: both bands must be populated (the split ring crosses the disc)"
                );
            }
            let base = TerrainUniforms {
                view_proj: wgsl_m(&near_vp),
                eye_rel_body: [eye.x as f32, eye.y as f32, eye.z as f32, 0.0],
                sun_dir: [0.0, 0.0, 1.0, 0.0],
                planet: [radius as f32, 0.0, 0.0, 1.0],
                ..TerrainUniforms::default()
            };
            let mut far = base;
            far.view_proj = wgsl_m(&far_vp);
            let packet = TerrainPacket {
                uniforms: base,
                uniforms_far: far,
                heightfield: Some(std::sync::Arc::new(field.clone())),
                terrain,
                terrain_far_start,
                sky: false,
                ..TerrainPacket::default()
            };
            // One renderer + render per arm (fresh instance-buffer
            // keys, no cross-talk); the control swaps ONLY the far
            // uniform row for the near one.
            let render = |use_near_row: bool| -> Vec<u8> {
                let mut renderer =
                    TerrainRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
                let mut p = packet.clone();
                if use_near_row {
                    p.uniforms_far = p.uniforms;
                }
                let msaa = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("far-test-msaa"),
                    size: wgpu::Extent3d {
                        width: 128,
                        height: 128,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: crate::MSAA_SAMPLES,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8UnormSrgb,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                    view_formats: &[],
                });
                let target = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("far-test-target"),
                    size: wgpu::Extent3d {
                        width: 128,
                        height: 128,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8UnormSrgb,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                    view_formats: &[],
                });
                let msaa_view = msaa.create_view(&wgpu::TextureViewDescriptor::default());
                let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());
                // A local reverse-Z depth attachment, the flight pass's
                // own conventions (Depth32Float, 4×, clear 0.0).
                let depth = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("far-test-depth"),
                    size: wgpu::Extent3d {
                        width: 128,
                        height: 128,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: crate::MSAA_SAMPLES,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Depth32Float,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                    view_formats: &[],
                });
                let depth_view = depth.create_view(&wgpu::TextureViewDescriptor::default());
                let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("far-test"),
                });
                {
                    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("far-test-pass"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &msaa_view,
                            resolve_target: Some(&target_view),
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                                store: wgpu::StoreOp::Store,
                            },
                            depth_slice: None,
                        })],
                        depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                            view: &depth_view,
                            depth_ops: Some(wgpu::Operations {
                                load: wgpu::LoadOp::Clear(0.0),
                                store: wgpu::StoreOp::Discard,
                            }),
                            stencil_ops: None,
                        }),
                        timestamp_writes: None,
                        occlusion_query_set: None,
                        multiview_mask: None,
                    });
                    renderer.draw_far(&mut pass, &device, &queue, &p);
                }
                let buf = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("far-test-readback"),
                    size: 128 * 128 * 4,
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                });
                encoder.copy_texture_to_buffer(
                    wgpu::TexelCopyTextureInfo {
                        texture: &target,
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::TexelCopyBufferInfo {
                        buffer: &buf,
                        layout: wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(128 * 4),
                            rows_per_image: Some(128),
                        },
                    },
                    wgpu::Extent3d {
                        width: 128,
                        height: 128,
                        depth_or_array_layers: 1,
                    },
                );
                queue.submit([encoder.finish()]);
                let (tx, rx) = std::sync::mpsc::channel();
                buf.map_async(wgpu::MapMode::Read, .., move |r| {
                    let _ = tx.send(r);
                });
                device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
                rx.recv().unwrap().expect("map_async failed");
                let img = buf.get_mapped_range(..).unwrap().to_vec();
                buf.unmap();
                img
            };
            let far_img = render(false);
            let near_row_img = render(true);
            let painted = |img: &[u8]| {
                img.chunks_exact(4)
                    .filter(|p| (0..3).any(|c| p[c] > 8))
                    .count()
            };
            let far_px = painted(&far_img);
            let control_px = painted(&near_row_img);
            eprintln!("far-field device [{label}]: far={far_px} px, near-row control={control_px}");
            assert!(
                control_px > 500,
                "{label}: the control arm must paint the disc ({control_px} px) — rig broke"
            );
            assert!(
                far_px > 500,
                "{label}: draw_far painted only {far_px} px (control {control_px}) — the far \
                 pass lost its surface"
            );
            let diffs = far_img
                .chunks_exact(4)
                .zip(near_row_img.chunks_exact(4))
                .filter(|(a, b)| (0..3).any(|c| a[c].abs_diff(b[c]) > 8))
                .count();
            assert_eq!(
                diffs, 0,
                "{label}: the far row's disc differs from the near-row control in {diffs} px"
            );
        }
    }

    /// P1's disarmed-row gate: `draw_shadow` must pay the G3 cull
    /// solve ONLY on frames whose (effective) row is armed and whose
    /// packet carries live terrain — a night frame or the sky-only
    /// heightfield bake previously burned 2×N keep-projections for
    /// run lists the passes never read. `cull_solves` is the
    /// observable. The stamp + uniform-write half must keep running on
    /// every call (the frame-leader contract): a skipped disarm write
    /// is the stale-armed-buffer failure the every-call write exists
    /// to prevent. Skips without an adapter (CSP_RENDER_GATE_REQUIRED
    /// makes the skip a failure — the render gate's contract).
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    #[allow(clippy::too_many_lines)] // one device-frame recipe (adapter + packet + three arms); splitting hides the gate
    fn disarmed_row_skips_the_cull_solve() {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let Ok(adapter) =
            pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                compatible_surface: None,
                ..wgpu::RequestAdapterOptions::default()
            }))
        else {
            assert!(
                !std::env::var("CSP_RENDER_GATE_REQUIRED").is_ok_and(|v| !v.is_empty() && v != "0"),
                "disarmed-row device test: CSP_RENDER_GATE_REQUIRED is set but no headless \
                 GPU adapter exists"
            );
            eprintln!("disarmed-row device test: SKIP — no headless GPU adapter");
            return;
        };
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("disarmed-row-test"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            ..wgpu::DeviceDescriptor::default()
        }))
        .unwrap();
        // A packet with LIVE terrain instances + a heightfield (the
        // armed arm's full path: solve → sync → cascade passes).
        let radius = 600_000.0;
        let eye = DVec3::new(0.0, 0.0, radius + 800_000.0);
        let lod = LodConfig {
            max_level: 8,
            split_factor: 2.0,
            max_patches: 512,
        };
        let field = HeightField::bake_profiled(7, csp_core::terrain::ReliefProfile::Legacy);
        let terrain: Vec<TerrainInstance> = select_patches(eye, radius, 0.0, &lod)
            .iter()
            .map(|&id| patch_instance(id, 0.0, eye, radius))
            .collect();
        assert!(!terrain.is_empty(), "the packet must carry live terrain");
        let packet = TerrainPacket {
            uniforms: TerrainUniforms {
                planet: [radius as f32, 0.0, 0.0, 1.0],
                ..TerrainUniforms::default()
            },
            heightfield: Some(std::sync::Arc::new(field)),
            terrain,
            terrain_far_start: 0,
            sky: false,
            ..TerrainPacket::default()
        };
        let sun = DVec3::new(0.3, 0.2, 0.9);
        let armed = crate::shadow::solve_cascades(
            &crate::shadow::ShadowCamera {
                sun_dir: sun / sun.norm(),
                forward: DVec3::new(0.0, 0.0, -1.0),
                up: DVec3::new(0.0, 1.0, 0.0),
                fov_y: 1.2,
                aspect: 1.6,
                near_m: 0.1,
            },
            &crate::shadow::CascadeConfig::default(),
        )
        .uniforms;
        assert!(armed.armed(), "the rig's solve must produce an armed row");
        let mut renderer = TerrainRenderer::new(&device, wgpu::TextureFormat::Rgba8UnormSrgb);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("disarmed-row-test"),
        });
        let stamp_before = renderer.frame_stamp;
        // (1) Disarmed row, live terrain — the night frame: no solve.
        renderer.draw_shadow(
            &mut encoder,
            &device,
            &queue,
            &packet,
            &ShadowUniforms::default(),
            None,
        );
        assert_eq!(
            renderer.cull_solves, 0,
            "a disarmed row must not run the cull solve"
        );
        assert!(
            renderer.frame_stamp.wrapping_sub(stamp_before) >= 1,
            "the frame-leader stamp must still bump on a disarmed call"
        );
        // (2) Armed row, EMPTY terrain — the sky-only wait frame: the
        // implicit disarm must skip the solve too.
        let mut sky_only = packet.clone();
        sky_only.terrain.clear();
        renderer.draw_shadow(&mut encoder, &device, &queue, &sky_only, &armed, None);
        assert_eq!(
            renderer.cull_solves, 0,
            "an armed row over an empty terrain list must not run the cull solve"
        );
        // (3) Armed row, live terrain — the daytime frame: solve once,
        // and the full pass path records without a validation fault.
        renderer.draw_shadow(&mut encoder, &device, &queue, &packet, &armed, None);
        assert_eq!(
            renderer.cull_solves, 1,
            "an armed row with live terrain solves"
        );
        // A repeat in a fresh frame solves again (the gate is per-call,
        // not a latch).
        renderer.draw_shadow(&mut encoder, &device, &queue, &packet, &armed, None);
        assert_eq!(renderer.cull_solves, 2);
        queue.submit([encoder.finish()]);
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    }

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
