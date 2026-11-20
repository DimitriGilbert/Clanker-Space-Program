//! Shadow-cascade core — the R1 debt (PLAN §5.2 row R1: "shadow
//! cascades"; every other term of that row shipped at M3–M4, which
//! is why §8 of `docs/UI-AND-RENDERING.md` could say "no shadow
//! maps anywhere"). Terrain-overhaul phase 3a landed the
//! renderer-side core; phase 3b arms it: the `csp-view` flight
//! extract solves [`solve_cascades`] per frame (fed by the chase
//! camera + `FlightEnv` sun, riding the flight packet's
//! [`crate::flight::FlightPacket::shadow`] row) and the app frame
//! loop calls [`crate::terrain::TerrainRenderer::draw_shadow`]
//! before the scene pass. The default [`ShadowUniforms`] stays the
//! disarm row: night/grazing sun (see the view's arm threshold) and
//! every non-flight mode render exactly as before.
//!
//! # What lives here
//!
//! - [`solve_cascades`] — the PURE per-frame solve (no GPU types):
//!   two sun-aligned orthographic cascades fitted around the chase
//!   camera's frustum slices, f64 throughout like every camera solve
//!   in the view stack, with a **texel-snapped translation over a
//!   ladder-quantized extent** so sub-texel camera motion of any
//!   kind reproduces bit-identical matrices (the classic CSM
//!   shimmer, killed at source). Inline tests pin corner containment,
//!   snap stability, and bit determinism.
//! - [`ShadowUniforms`] — the GPU product: two light-view-proj
//!   matrices mapping `(world − eye)` — the §2.3 camera-relative
//!   frame every terrain vertex/fragment already carries — into
//!   cascade clip `[0,1]³`, per-cascade bias rows, and the
//!   armed/split/far/band row. All-zero = disarmed.
//! - [`ShadowCore`] — the GPU side (`pub(crate)`, owned by
//!   [`crate::terrain::TerrainRenderer`]): one `Depth32Float` array
//!   texture (2 layers) + a `LessEqual` **comparison sampler** (core
//!   WebGPU — no feature flags, so the `Features::empty()` contract
//!   holds on lavapipe and wasm alike), two depth-only pipelines
//!   whose vertex stage re-runs the terrain displacement (they share
//!   `terrain_displace` with `vs_terrain`, so the depth pass moves
//!   vertices identically to the visible pass — a diverging
//!   displacement would de-register map and receiver by whole
//!   texels), the per-layer render passes, and — since 3b — the
//!   craft-cast twins of those pipelines (`flight.wgsl`'s
//!   `vs_shadow0/1` over the flight vertex stream) that put hulls and
//!   launch-site structures into the same cascade map.
//!
//! # Cascade design (the constants below carry their own rationale)
//!
//! Two cascades cover the ~0–2 km and ~2–20 km eye-distance bands;
//! beyond 20 km — and whenever disarmed — `fs_terrain`'s
//! `sun_occlusion` ray-march stays the far-field fallback. Ocean
//! quads RECEIVE cascade shadows but never cast them (the ray-march
//! fallback never let water shadow the seabed either; light loss
//! through water is the depth-tint term's job). Craft casting is
//! 3b's shipped extension: the flight vertex stream's depth-only
//! draw rides the same cascade passes (see [`ShadowDraw::flight`]).
//!
//! # Why a dedicated uniform buffer (not `TerrainUniforms` fields)
//!
//! `csp-view` constructs `TerrainPacket`/`TerrainUniforms` with
//! exhaustive struct literals, so a field addition there is a 3b
//! contract change. The cascade data instead rides its own `SU`
//! uniform at terrain group(0) binding 8, plus a dedicated
//! group(1) (depth array + comparison sampler) that only the
//! visible scene pass binds — see [`ShadowCore::sample_layout`] for
//! why the depth texture must stay out of every bind group the
//! shadow passes set. All written only by this crate — 3b's blast
//! radius stays inside the frame loop.

use csp_core::DVec3;

// ---------------------------------------------------------------------------
// Constants — the cascade budget.
// ---------------------------------------------------------------------------

/// Cascade layers (and texture-array layers): near band + far band.
pub const CASCADE_COUNT: usize = 2;
/// Shadow map edge, texels. Memory: 2 × 2048² × 4 B = 33.5 MB of
/// depth texture, allocated once at renderer init. 2048² puts
/// cascade 0 (a ~3.4 km box at the flight fov/aspect) at ~1.4–2 m
/// per texel depending on sun/camera geometry (the √2-ladder rung at
/// or above the raw fit) — a landed craft's shadow still reads at
/// boot level; 1536² (~1.9–2.8 m/texel, 18.9 MB) saves 14 MB but
/// visibly smears exactly the range the blob shadow used to cover,
/// so the finer map wins while the tier stays Baseline (a plain
/// texture, no features).
pub const SHADOW_MAP_RES: u32 = 2048;
/// Cascade 0 far bound (m): the boot/rover/chase band where a craft
/// or ridge line shadows the ground right around the camera.
pub const CASCADE_SPLIT_M: f64 = 2_000.0;
/// Cascade 1 far bound (m): mountain-range shadows seen from low
/// flight. Beyond this the `sun_occlusion` ray-march (reach ~5 km
/// past the receiver) remains the fallback.
pub const CASCADE_FAR_M: f64 = 20_000.0;
/// Extra light-space depth pulled toward the sun beyond each
/// cascade's slice (m): casters BETWEEN the sun and the slice that
/// sit outside it (an up-sun ridge behind the camera) still
/// rasterize into the map. At the M5 relief ceiling (ridge p99 tan
/// 0.36, crest-guarded to ~tan 0.75) a 2.4 km peak throws a ~3.5 km
/// shadow at 35° sun, so 4 km catches the common cases; horizon-hug
/// sun keeps the ray-march's long soft shadows as before.
pub const CASTER_PULLBACK_M: f64 = 4_000.0;
/// Light-space depth pad on the anti-sun end of each cascade's range
/// (m): a receiver selected by eye-DISTANCE sits at view-Z ≤ its
/// distance, i.e. short of its cascade's slice — looking up-sun, such
/// a receiver's reference depth lands past the slice's far z, and the
/// pad keeps it inside the rendered range (the sun-side end carries
/// the much larger [`CASTER_PULLBACK_M`]).
pub const RECEIVER_Z_PAD_M: f64 = 250.0;
/// Cascade-boundary blend band, as a fraction of [`CASCADE_SPLIT_M`]
/// (300 m at the default split): the band cross-fades cascade 0 into
/// cascade 1 so the resolution step at the boundary reads as a soft
/// gradient instead of a seam.
pub const BLEND_BAND_FRAC: f64 = 0.15;
/// Normal-offset bias in texels: the receiver's SAMPLE POSITION is
/// nudged along `+n` (its shading normal) by
/// `texel · this · (1 + tanθ)` before projection. Sign: for a lit
/// fragment `n·sun > 0` while the map's depth axis is the sun's
/// travel direction (−sun), so `lt·n < 0` — the offset moves the
/// sample AWAY from the stored surface into the sun direction,
/// lowering its projected depth in the same direction the reference
/// bias subtracts. Slopes up to ~37° (tan 0.75) need ≳ 1 texel of
/// offset to clear their own footprint in the map; 1.5 texels leaves
/// headroom for the quadtree's 146 m patch discretization at a cost
/// of ~a texel of peter-panning (contact shadows lift slightly) —
/// the cheaper direction to err, versus acne, at this tier.
pub const NORMAL_OFFSET_TEXELS: f64 = 1.5;
/// Slope-scaled comparison-reference bias in texels-of-depth: the
/// reference depth is REDUCED by `texel/span · this · (1 + tanθ)` —
/// toward the sun, since map depth is forward-Z-along-travel
/// (closer to the sun = smaller stored value, lit ⇔ `ref ≤ stored`,
/// and the receiver's own surface sits in the map at ≈ its own
/// depth; subtracting is what lets flat ground pass its own texel).
/// Grazing receivers (large tanθ) get proportionally more slack
/// against the ortho quantization of sloped surfaces. 2 texels
/// matches the offset bias's slope curve. The flat-ground direction
/// is pinned by test (`bias_moves_reference_toward_the_sun`).
pub const SLOPE_BIAS_TEXELS: f64 = 2.0;
/// The world-texel ladder spacing (2^(n/2) m, n ∈ ℤ). Quantizing the
/// ortho fit onto this ladder — not just its translation — is what
/// makes sub-texel camera motion bit-stable: rotation perturbs the
/// required extents continuously, so a floating extent would leak
/// that motion straight into the matrix scale entries every frame.
/// The ladder trades ≤ ~1.4× texel coarseness for extents that only
/// step when the required texel crosses a whole ladder rung.
const LADDER_SQRT2: f64 = std::f64::consts::SQRT_2;

// ---------------------------------------------------------------------------
// The pure solve.
// ---------------------------------------------------------------------------

/// The camera the cascades must cover, in the **inertial body-centred
/// frame** the terrain pass renders in (the same convention as
/// `TerrainUniforms::{sun_dir, eye_rel_body}` and the `view_proj`
/// applied to `world − eye`). No `eye` field: in the §2.3
/// eye-relative form the frustum slice is `f·d ± r·(d·tx) ± u·(d·ty)`
/// — directions and half-angles only — so a 600 km body-centred eye
/// never enters the math and never rounds through f32.
#[derive(Clone, Copy, Debug)]
pub struct ShadowCamera {
    /// Unit direction TO the sun (inertial body frame) — matches
    /// `TerrainUniforms::sun_dir` and the light matrices below.
    pub sun_dir: DVec3,
    /// Unit view direction (eye → scene) — the chase camera's
    /// `−z_out` column.
    pub forward: DVec3,
    /// Unit camera up (the chase solve's local-level up; used only
    /// to orient the frustum, orthonormalized defensively).
    pub up: DVec3,
    /// Vertical field of view (rad) — flight's `FLIGHT_FOV_Y`.
    pub fov_y: f64,
    /// Width/height.
    pub aspect: f64,
    /// Near plane (m) — flight's 0.1; also cascade 0's slice near.
    pub near_m: f64,
}

/// Solve tunables. Defaults are the documented cascade budget above;
/// `world_scale` multiplies both split depths so 3b can tighten
/// coverage on small bodies (a 3 km asteroid wants metres, not
/// tens of kilometres).
#[derive(Clone, Copy, Debug)]
pub struct CascadeConfig {
    /// Cascade 0 far bound before scaling (m).
    pub split_m: f64,
    /// Cascade 1 far bound before scaling (m).
    pub far_m: f64,
    /// Body-size scale on the split depths (1.0 at the home body).
    pub world_scale: f64,
    /// Caster pullback toward the sun (m) — see
    /// [`CASTER_PULLBACK_M`].
    pub pullback_m: f64,
}

impl Default for CascadeConfig {
    fn default() -> Self {
        Self {
            split_m: CASCADE_SPLIT_M,
            far_m: CASCADE_FAR_M,
            world_scale: 1.0,
            pullback_m: CASTER_PULLBACK_M,
        }
    }
}

/// The GPU-facing product of the solve. All fields 16-byte aligned;
/// matrices column-major (`m[col][row]`) like
/// `TerrainUniforms::view_proj`. `Default` (all zeros) is DISARMED —
/// `fs_terrain` short-circuits to the ray-march exactly as before.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
pub struct ShadowUniforms {
    /// Cascade 0/1 light-view-proj: `(world − eye)` → cascade clip
    /// `[0,1]³`. Forward-Z **inside the map** (smaller depth = closer
    /// to the sun; the map clears to 1.0 = "nothing closer = lit") —
    /// independent of the main pass's reverse-Z convention, which
    /// this attachment never touches.
    pub vp: [[[f32; 4]; 4]; 2],
    /// Per-cascade row: `x` world texel (m), `y` normal-offset bias
    /// (m), `z` reference bias (normalized depth units), `w` pad.
    pub cascade: [[f32; 4]; 2],
    /// `x` armed (1 = sample cascades, 0 = ray-march fallback),
    /// `y` split distance (m), `z` coverage-far distance (m),
    /// `w` blend band (m).
    pub pars: [f32; 4],
}

impl ShadowUniforms {
    /// Whether `fs_terrain` should take the cascade path.
    pub fn armed(&self) -> bool {
        self.pars[0] >= 0.5
    }
}

/// Solve diagnostics the unit tests (and 3b's tuning) consume; the
/// GPU only ever sees [`ShadowUniforms`].
#[derive(Clone, Copy, Debug)]
pub struct Cascades {
    /// The uploadable product.
    pub uniforms: ShadowUniforms,
    /// Per-cascade world half-extent of the ortho box (m) — the
    /// square side is `2 · half`, one texel `2 · half / RES`.
    pub half_extent_m: [f64; CASCADE_COUNT],
    /// Per-cascade world texel (m).
    pub texel_m: [f64; CASCADE_COUNT],
}

/// Snap `v` to multiples of `step` (f64 round — deterministic, no
/// transcendentals).
fn snap_to(v: f64, step: f64) -> f64 {
    (v / step).round() * step
}

/// The smallest ladder texel `2^(n/2) ≥ raw` (see [`LADDER_SQRT2`]).
/// A short multiplicative walk from 1 m — deterministic bit-for-bit
/// (IEEE multiply), ~30 iterations over any sane texel (1 mm … 1 km).
fn ladder_texel(raw: f64) -> f64 {
    let mut q = 1.0;
    if raw >= 1.0 {
        while q < raw {
            q *= LADDER_SQRT2;
        }
    } else {
        while q / LADDER_SQRT2 >= raw {
            q /= LADDER_SQRT2;
        }
    }
    q
}

/// Orthonormal `right`/`up` completing `forward` (right-handed,
/// matching the chase solve: `right = forward × up`,
/// `up' = right × forward`). Falls back to a world axis when `up` is
/// (near-)parallel to `forward` so a degenerate input degrades to a
/// valid frame instead of NaNs.
fn camera_frame(forward: DVec3, up: DVec3) -> (DVec3, DVec3) {
    let f = if forward.norm_squared() > 1.0e-24 {
        forward / forward.norm()
    } else {
        DVec3::X
    };
    let mut right = f.cross(up);
    if right.norm_squared() < 1.0e-12 {
        let ref_axis = if f.dot(DVec3::Z).abs() < 0.999 {
            DVec3::Z
        } else {
            DVec3::X
        };
        right = f.cross(ref_axis);
    }
    let right = right / right.norm();
    (right, right.cross(f))
}

/// The sun-fixed light basis `(right, up, travel)`: `travel` points
/// along the light's direction (away from the sun), `right`/`up`
/// complete it. A pure function of `sun_dir` — the basis is fixed
/// per sun direction with no per-frame re-rolling, so a static sun
/// yields a static frame (the no-jitter half of stability; the
/// texel snap below is the other half).
fn light_basis(sun_dir: DVec3) -> (DVec3, DVec3, DVec3) {
    let s = if sun_dir.norm_squared() > 1.0e-24 {
        sun_dir / sun_dir.norm()
    } else {
        DVec3::X
    };
    let travel = s * -1.0;
    let ref_axis = if travel.dot(DVec3::Z).abs() < 0.999 {
        DVec3::Z
    } else {
        DVec3::X
    };
    let up = ref_axis - travel * ref_axis.dot(travel);
    let up = up / up.norm();
    let right = up.cross(travel);
    (right / right.norm(), up, travel)
}

/// Solve both cascades. Pure f64, deterministic (same inputs → same
/// bits, pinned by test); the only f32 anywhere is the final cast of
/// matrix/bias entries into [`ShadowUniforms`].
///
/// Per cascade: the 8 corners of the camera-frustum slice
/// `[near, split]` / `[split, far]` are projected into the light
/// basis, the ortho box is squared in x/y (the texture is square —
/// a rectangular fit would waste texels), extended toward the sun by
/// the caster pullback and past the far side by the receiver pad,
/// fitted onto the [`LADDER_SQRT2`] texel ladder, and its **center
/// snapped to whole texels** in all three axes. Because the solve is
/// eye-relative (§2.3 above) the frustum geometry is invariant under
/// pure camera translation outright, and because both the extent
/// (ladder) and the translation (snap) are quantized, sub-texel
/// camera motion of ANY kind — rotation included — reproduces the
/// same matrix bits; the classic CSM shimmer is killed at source.
/// The trade: rotation refits the box only when the required texel
/// crosses a ladder rung (the map's density steps there), and the
/// ladder costs ≤ ~1.4× coarser texels than an exact float fit.
pub fn solve_cascades(cam: &ShadowCamera, cfg: &CascadeConfig) -> Cascades {
    let tan_half_y = libm::tan(cam.fov_y * 0.5).max(1.0e-6);
    let aspect = cam.aspect.max(1.0e-6);
    let near = cam.near_m.max(0.01);
    let scale = cfg.world_scale.max(1.0e-3);
    let split = (cfg.split_m * scale).max(near * 2.0);
    let far = (cfg.far_m * scale).max(split * 2.0);

    let f = if cam.forward.norm_squared() > 1.0e-24 {
        cam.forward / cam.forward.norm()
    } else {
        DVec3::X
    };
    let (cr, cu) = camera_frame(f, cam.up);
    let (lr, lu, lt) = light_basis(cam.sun_dir);

    // The 8 slice corners, camera-relative (§2.3: directions and
    // small offsets only — the body-centred eye never appears).
    let corners = |d0: f64, d1: f64| -> [DVec3; 8] {
        let mut out = [DVec3::ZERO; 8];
        let mut k = 0usize;
        for d in [d0, d1] {
            let hx = d * tan_half_y * aspect;
            let hy = d * tan_half_y;
            for sy in [-1.0f64, 1.0] {
                for sx in [-1.0f64, 1.0] {
                    out[k] = f * d + cr * (sx * hx) + cu * (sy * hy);
                    k += 1;
                }
            }
        }
        out
    };

    let mut out = Cascades {
        uniforms: ShadowUniforms {
            pars: [
                1.0,
                split as f32,
                far as f32,
                (BLEND_BAND_FRAC * split) as f32,
            ],
            ..ShadowUniforms::default()
        },
        half_extent_m: [0.0; CASCADE_COUNT],
        texel_m: [0.0; CASCADE_COUNT],
    };
    let slice_bounds = [(near, split), (split, far)];
    for (c, &(d0, d1)) in slice_bounds.iter().enumerate() {
        let pts = corners(d0, d1);
        let (mut xlo, mut xhi) = (f64::INFINITY, f64::NEG_INFINITY);
        let (mut ylo, mut yhi) = (f64::INFINITY, f64::NEG_INFINITY);
        let (mut zlo, mut zhi) = (f64::INFINITY, f64::NEG_INFINITY);
        for p in pts {
            let x = lr.dot(p);
            let y = lu.dot(p);
            let z = lt.dot(p);
            xlo = xlo.min(x);
            xhi = xhi.max(x);
            ylo = ylo.min(y);
            yhi = yhi.max(y);
            zlo = zlo.min(z);
            zhi = zhi.max(z);
        }
        // Square in x/y + the caster/receiver margins in z.
        let half_raw = ((xhi - xlo).max(yhi - ylo) * 0.5).max(1.0e-3);
        let zmin_raw = zlo - cfg.pullback_m;
        let zmax_raw = zhi + RECEIVER_Z_PAD_M;
        // Ladder-quantized texel (see LADDER_SQRT2): `half` becomes
        // an exact `texel · RES/2`, so both the scale and the snap
        // step are piecewise-constant in the required fit.
        let texel = ladder_texel(2.0 * half_raw / f64::from(SHADOW_MAP_RES));
        let half = texel * f64::from(SHADOW_MAP_RES) / 2.0;
        // The z span rides the ladder too (+ one texel, so the
        // snapped center's half-texel of slack can never expose the
        // raw range's ends): an unquantized span would leak sub-texel
        // camera motion into the z scale entry exactly the way a
        // floating `half` does for x/y.
        let span = ladder_texel((zmax_raw - zmin_raw).max(1.0) + texel);

        // Texel snap: quantize the box center so sub-texel camera
        // motion cannot move a single matrix entry.
        let cx = snap_to((xlo + xhi) * 0.5, texel);
        let cy = snap_to((ylo + yhi) * 0.5, texel);
        let cz = snap_to((zmin_raw + zmax_raw) * 0.5, texel);

        // Row-major f64 rows of `(world − eye) → cascade clip`, then
        // the same `m[row][col] → out[col][row]` column-major cast
        // the view stack uses (`m4_wgsl` in csp-view).
        let sx = 1.0 / half;
        let sz = 1.0 / span;
        let rows = [
            [lr.x * sx, lr.y * sx, lr.z * sx, -cx * sx],
            [lu.x * sx, lu.y * sx, lu.z * sx, -cy * sx],
            [lt.x * sz, lt.y * sz, lt.z * sz, -(cz - span * 0.5) * sz],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let mut m = [[0.0f32; 4]; 4];
        for r in 0..4 {
            for col in 0..4 {
                m[col][r] = rows[r][col] as f32;
            }
        }
        out.uniforms.vp[c] = m;
        // Bias magnitudes — the receiving shader applies the SIGNS:
        // offset along +n and reference bias SUBTRACTED from the
        // projected depth (both toward the sun; see the two bias
        // constants' docs and the WGSL sign chain).
        out.uniforms.cascade[c] = [
            texel as f32,
            (NORMAL_OFFSET_TEXELS * texel) as f32,
            (SLOPE_BIAS_TEXELS * texel / span) as f32,
            0.0,
        ];
        out.half_extent_m[c] = half;
        out.texel_m[c] = texel;
    }
    out
}

// ---------------------------------------------------------------------------
// G3 — the CPU-side per-cascade caster cull (pure; no GPU types).
// ---------------------------------------------------------------------------

/// Kept-instance runs per cascade layer: `(first_instance, count)`
/// pairs over the terrain instance buffer's own indexing — contiguous
/// draws over the SAME upload, no re-staging.
pub(crate) type InstanceRuns = Vec<(u32, u32)>;

/// Cap on kept-runs per layer. Kept instances cluster in quadtree BFS
/// order (the sub-camera face's deep tail), so real frames land in a
/// handful of runs; the cap bounds the pathological alternation to
/// `CULL_MAX_RUNS` draws per layer by widening (never narrowing) the
/// last run — a capped frame degrades toward the unculled draw, never
/// past it.
const CULL_MAX_RUNS: usize = 32;
/// Gap (instances) a run is widened across to swallow a lone culled
/// instance — one extra clipped draw beats a run-boundary draw call.
const CULL_MERGE_GAP: u32 = 8;

/// The exact f32 arithmetic of the depth vertex stage's
/// `su.vp[c] * vec4(wrel, 1.0)` (column-major `m[col][row]`, ortho —
/// `w` is identically 1, kept for the mirror's sake). Running the CPU
/// test through the SAME matrix bits the GPU uses is what makes the
/// cull a bound on the real rasterization, not on a re-derivation of
/// it.
fn clip_xform(m: &[[f32; 4]; 4], p: [f32; 3]) -> [f32; 4] {
    let v = [p[0], p[1], p[2], 1.0f32];
    let mut out = [0.0f32; 4];
    for (r, o) in out.iter_mut().enumerate() {
        let mut acc = 0.0f32;
        for c in 0..4 {
            acc += m[c][r] * v[c];
        }
        *o = acc;
    }
    out
}

/// Per-axis world-metres → clip-units scales of a cascade matrix:
/// math rows `lr·(1/half)`, `lu·(1/half)`, `lt·(1/span)` are unit
/// vectors times their scale, so each row's f64 magnitude IS the
/// scale. Reconstructed from the f32 matrix the GPU reads (the solve
/// wrote exactly these rows), not from the solve's f64 internals —
/// the cull must bound what the shipped uniforms do.
fn clip_scales(m: &[[f32; 4]; 4]) -> [f64; 3] {
    let row =
        |r: usize| DVec3::new(f64::from(m[0][r]), f64::from(m[1][r]), f64::from(m[2][r])).norm();
    [row(0), row(1), row(2)]
}

/// Conservative keep test for one caster against cascade `c`'s clip
/// volume: the patch's every displaced vertex lies within `extent_m`
/// of its baked centre (the [`crate::terrain::patch_extent_m`]
/// Lipschitz bound, reconstructed from the instance row below), so
/// keep ⇔ the centre's clip interval ± `extent·scale` meets the
/// WebGPU clip volume `x,y ∈ [−1,1]`, `z ∈ [0,1]` on ALL THREE axes —
/// the patch's bounding sphere intersecting the ortho box. An
/// instance that fails this projects no fragment
/// into the layer (all of its vertices clip on at least one axis,
/// every triangle with it dies whole), so dropping it cannot change a
/// single stored texel. `+1.0` m of slack covers the vertex stage's
/// f32 rounding of `wrel` (~mm at the 90 km horizon magnitudes)
/// beside the extent's own f32-derived inputs.
fn cascade_keeps(m: &[[f32; 4]; 4], origin_rel_eye: [f32; 3], extent_m: f64) -> bool {
    let clip = clip_xform(m, origin_rel_eye);
    let [sx, sy, sz] = clip_scales(m);
    let (x, y, z) = (f64::from(clip[0]), f64::from(clip[1]), f64::from(clip[2]));
    let e = extent_m + 1.0;
    x.abs() <= 1.0 + e * sx && y.abs() <= 1.0 + e * sy && (-e * sz) <= z && z <= 1.0 + e * sz
}

/// Upper bound on the distance from the EYE (the rel-eye frame's
/// origin) to any point of a cascade's ortho box — the box's farthest
/// corner, reconstructed from the matrix: light-space rows give the
/// box half-extents (`1/|row|`) and centre (`−t/|row|` in x/y; the z
/// translation sits at the box's near end), and the light basis is
/// orthonormal so `|p|²` is the sum of squared light coordinates.
fn cascade_box_radius(m: &[[f32; 4]; 4]) -> f64 {
    let [sx, sy, sz] = clip_scales(m);
    // A disarmed/zero row has no box; callers only reach here armed.
    let (sx, sy, sz) = (sx.max(1.0e-12), sy.max(1.0e-12), sz.max(1.0e-12));
    let half_x = 1.0 / sx;
    let half_y = 1.0 / sy;
    let span = 1.0 / sz;
    let cx = -f64::from(m[3][0]) / sx;
    let cy = -f64::from(m[3][1]) / sy;
    // z translation = −(cz − span/2)·sz → the box's NEAR end in m.
    let z_near = -f64::from(m[3][2]) / sz;
    let z_far = z_near + span;
    let z = z_near.abs().max(z_far.abs());
    libm::sqrt((cx.abs() + half_x).powi(2) + (cy.abs() + half_y).powi(2) + z.powi(2))
}

/// The cull's reach bound for one instance row —
/// [`crate::terrain::patch_extent_m`]'s formula reconstructed from the
/// row the GPU actually displaces (its f32 cube spans cast up), plus
/// the row's own skirt depth (the skirt ring drops INWARD along
/// `dir`, which shrinks its distance from the baked centre — the §2.3
/// doc's argument — but crediting it costs nothing and removes the
/// need to lean on that argument here) and the defensive
/// `max(terrain, ocean) datum` for a mis-kind'd row (ocean radius is
/// `planet.z`; real packets never carry ocean rows in the terrain
/// list, a defensive producer bug would).
fn instance_extent_m(
    inst: &crate::terrain::TerrainInstance,
    radius_m: f64,
    ocean_radius_m: f64,
    amplitude_m: f64,
) -> f64 {
    let du = DVec3::new(
        f64::from(inst.du[0]),
        f64::from(inst.du[1]),
        f64::from(inst.du[2]),
    );
    let dv = DVec3::new(
        f64::from(inst.dv[0]),
        f64::from(inst.dv[1]),
        f64::from(inst.dv[2]),
    );
    let radius = radius_m.max(ocean_radius_m);
    (du + dv).norm() * 0.5 * radius + amplitude_m + f64::from(inst.origin_rel_eye[3]).max(0.0)
}

/// Record one kept instance into the run list (widen across small
/// gaps, cap the run count by widening further — both directions
/// CONSERVATIVE: a widened run re-draws instances the per-instance
/// test placed outside the box, which the GPU then clips exactly as
/// the unculled pass did).
fn push_keep(runs: &mut InstanceRuns, i: u32) {
    if let Some(&mut (start, ref mut count)) = runs.last_mut()
        && i < start + *count + CULL_MERGE_GAP
    {
        *count = i + 1 - start;
    } else {
        runs.push((i, 1));
    }
    while runs.len() > CULL_MAX_RUNS && runs.len() >= 2 {
        let last = runs.len() - 1;
        let (s1, c1) = runs[last];
        let (s0, _) = runs[last - 1];
        runs[last - 1].1 = s1 + c1 - s0;
        runs.truncate(last);
    }
}

/// Solve both cascades' kept-instance runs over the packet's terrain
/// list (near-then-far; `far_start` the §2.3 routing boundary;
/// `planet` the uniform row — `x` datum radius, `z` ocean datum,
/// `w` relief amplitude, the same values the depth vertex stage
/// displaces through). Pure, deterministic (ascending instance
/// order), armed-`su` only — the disarmed path never records passes,
/// so it never calls this.
///
/// # Far-list exclusion (why only the near list is tested at the
/// home body)
///
/// Every FAR instance satisfies the §2.3 routing invariant
/// `fwd·origin − extent ≥ FAR_SPLIT_M` (1 Mm — the rule the view's
/// routing applies and [`crate::flight::FAR_SPLIT_M`] pins): `fwd` is
/// unit, so EVERY point of such a patch — centre, relief, skirt —
/// carries `fwd·p ≥ 1 Mm` and hence `|p| ≥ 1 Mm` from the eye. The
/// cascade boxes are fitted around the chase frustum's `[near, 20 km]`
/// slices (plus a 4 km sun pullback, a 250 m receiver pad and the
/// ≤ √2 ladder), so both boxes sit far inside a
/// [`cascade_box_radius`] of ~75 km ≪ 1 Mm: no far patch's bounding
/// sphere can reach either box, and testing them would be ~1000
/// no-op projections per frame. That bound is COMPUTED here, not
/// assumed — a `world_scale` large enough to grow a box past the
/// split (a gas-giant config) flips the gate and the far list joins
/// the test, keeping the cull correct for any config while the home
/// body pays near-list cost only.
pub(crate) fn cascade_runs(
    su: &ShadowUniforms,
    terrain: &[crate::terrain::TerrainInstance],
    far_start: u32,
    planet: &[f32; 4],
) -> [InstanceRuns; CASCADE_COUNT] {
    let radius_m = f64::from(planet[0]);
    let ocean_radius_m = f64::from(planet[2]);
    let amplitude_m = f64::from(planet[3]);
    let far_start = (far_start as usize).min(terrain.len());
    let box_r = cascade_box_radius(&su.vp[0]).max(cascade_box_radius(&su.vp[1]));
    // The gate above: far patches live ≥ FAR_SPLIT from the eye, the
    // boxes within `box_r` — disjoint spheres can't intersect.
    let test_len = if box_r < crate::flight::FAR_SPLIT_M {
        far_start
    } else {
        terrain.len()
    };
    let mut out = [Vec::new(), Vec::new()];
    for (c, runs) in out.iter_mut().enumerate() {
        for (i, inst) in terrain[..test_len].iter().enumerate() {
            if cascade_keeps(
                &su.vp[c],
                [
                    inst.origin_rel_eye[0],
                    inst.origin_rel_eye[1],
                    inst.origin_rel_eye[2],
                ],
                instance_extent_m(inst, radius_m, ocean_radius_m, amplitude_m),
            ) {
                push_keep(runs, i as u32);
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// GPU side — the depth-only cascade passes.
// ---------------------------------------------------------------------------

/// What [`ShadowCore::draw_terrain`] needs for one frame's cascade
/// passes: the terrain group(0) bind group (uniforms + heightfield —
/// the depth vertex stage reads them to displace identically to the
/// visible pass) and the shared grid/instance buffer slices.
pub(crate) struct ShadowDraw<'a> {
    /// The terrain bind group (bindings 0/1/8 are read).
    pub bind_group: &'a wgpu::BindGroup,
    /// Shared unit-grid vertices.
    pub grid_v: wgpu::BufferSlice<'a>,
    /// Shared unit-grid indices.
    pub grid_i: wgpu::BufferSlice<'a>,
    /// The terrain (never ocean) instance rows.
    pub instances: wgpu::BufferSlice<'a>,
    /// Grid index count (`grid_index_count`).
    pub index_count: u32,
    /// G3 per-layer kept-instance runs over `instances`
    /// ([`cascade_runs`]). An empty run list draws no terrain into
    /// that layer — the pass still clears (see
    /// [`ShadowCore::draw_terrain`]'s empty-layer contract).
    pub runs: [&'a InstanceRuns; CASCADE_COUNT],
    /// 3b craft cast: the flight vertex buffer + vertex count (the
    /// stream `FlightRenderer::stage_cascade_cast` uploaded this
    /// frame). `None`/0 casts terrain only. The cast pipelines ride
    /// the SAME terrain group(0) — binding 8's `SU` uniform is what
    /// their vertex stage reads — so no bind group change is needed
    /// between the terrain and craft draws inside a layer's pass.
    pub flight: Option<(wgpu::Buffer, u32)>,
}

/// GPU owner of the cascade resources: the 2-layer depth array, the
/// per-layer attachment views, the comparison sampler, the `SU`
/// uniform buffer, the receiving group(1), and the two depth-only
/// pipelines (`vs_shadow0`/`vs_shadow1` — one per layer; wgpu has no
/// layered rendering without features, so each layer is its own pass
/// with its own constant matrix index). Built once by
/// `TerrainRenderer::new`; nothing here needs `Features::empty()`
/// violated — depth textures, comparison sampling and depth-only
/// pipelines are all core WebGPU. The texture/array-view/sampler are
/// not fields: wgpu resources are Arc-shared, and the layer views,
/// group(1) bind group and pipelines between them keep every one of
/// those alive for exactly as long as this struct lives.
pub(crate) struct ShadowCore {
    pipes: [wgpu::RenderPipeline; 2],
    /// 3b craft-cast twins (`flight.wgsl` `vs_shadow0/1` over the
    /// `FlightVertex` layout): depth-only like the terrain pair, one
    /// per layer, riding the terrain group(0) for the `SU` matrices.
    flight_pipes: [wgpu::RenderPipeline; 2],
    /// The craft-cast group(0) bind group over the `SU` uniform
    /// (binding 8 — see `flight_shadow.wgsl`'s header for why the
    /// cast keeps a dedicated minimal layout/bind group).
    flight_bind: wgpu::BindGroup,
    /// Per-layer attachment views (2D, single layer).
    layers: [wgpu::TextureView; 2],
    /// The depth array handle (views keep it alive anyway; kept for
    /// the test readback — `cfg(test)` so release builds carry no
    /// dead field).
    #[cfg(test)]
    cascade_tex: wgpu::Texture,
    /// G3 empty-layer tracking: the layer currently holds the 1.0
    /// clear and no draw has written it since — set by a recorded
    /// (drawless) clear pass, cleared by any pass that draws. Starts
    /// `false`: a fresh `Depth32Float` texture is UNDEFINED (zeros on
    /// most drivers), and zeros through a live armed row are exactly
    /// the silent craft-darkening failure the sky-only guard exists
    /// to prevent — the first armed frame must clear. This is what
    /// lets an all-culled layer skip its pass outright: the receivers
    /// still read a fully-lit map, byte-identical to a fresh clear.
    cleared: [bool; CASCADE_COUNT],
    /// The `SU` cascade uniform (terrain group(0) binding 8).
    uniforms: wgpu::Buffer,
    /// The RECEIVING bind group's layout (group(1)): depth array +
    /// comparison sampler. Deliberately a group of its own: a usage
    /// scope merges every bind group SET in a render pass — used or
    /// not — so the depth texture must not appear in any bind group
    /// the shadow passes set while that same texture is their depth
    /// attachment. Group(1) is bound only by the visible scene pass.
    sample_bgl: wgpu::BindGroupLayout,
    sample_bind: wgpu::BindGroup,
}

/// The cascade depth array + its views: `RES²` × 2 layers,
/// `Depth32Float`, attachment + sampleable. Layer views are the
/// per-pass depth attachments; the array view (depth aspect) is what
/// group(1) samples through. The texture handle itself stays alive
/// through the views' Arcs.
fn depth_array(
    device: &wgpu::Device,
) -> ([wgpu::TextureView; 2], wgpu::TextureView, wgpu::Texture) {
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("terrain-shadow-depth"),
        size: wgpu::Extent3d {
            width: SHADOW_MAP_RES,
            height: SHADOW_MAP_RES,
            depth_or_array_layers: CASCADE_COUNT as u32,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Depth32Float,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT
            | wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let layer_view = |layer: u32| {
        tex.create_view(&wgpu::TextureViewDescriptor {
            label: Some("terrain-shadow-layer"),
            dimension: Some(wgpu::TextureViewDimension::D2),
            base_array_layer: layer,
            array_layer_count: Some(1),
            ..wgpu::TextureViewDescriptor::default()
        })
    };
    let sample_view = tex.create_view(&wgpu::TextureViewDescriptor {
        label: Some("terrain-shadow-array"),
        dimension: Some(wgpu::TextureViewDimension::D2Array),
        aspect: wgpu::TextureAspect::DepthOnly,
        ..wgpu::TextureViewDescriptor::default()
    });
    let out = tex.clone();
    ([layer_view(0), layer_view(1)], sample_view, out)
}

/// Comparison sampler, LessEqual: a receiver is lit when its biased
/// reference depth ≤ the nearest stored caster depth. Linear
/// filtering turns the compare into hardware 2×2 PCF where
/// supported — free edge softening on the map.
fn comparison_sampler(device: &wgpu::Device) -> wgpu::Sampler {
    device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some("terrain-shadow-cmp"),
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        address_mode_w: wgpu::AddressMode::ClampToEdge,
        mag_filter: wgpu::FilterMode::Linear,
        min_filter: wgpu::FilterMode::Linear,
        mipmap_filter: wgpu::MipmapFilterMode::Nearest,
        compare: Some(wgpu::CompareFunction::LessEqual),
        ..Default::default()
    })
}

/// The receiving group(1) — its layout + the bind group over the
/// array/sampler. See `ShadowCore.sample_bgl` for why this set is a
/// group of its own rather than more group(0) bindings.
fn sample_group(
    device: &wgpu::Device,
    sampler: &wgpu::Sampler,
    sample_view: &wgpu::TextureView,
) -> (wgpu::BindGroupLayout, wgpu::BindGroup) {
    let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("terrain-shadow-sample"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Depth,
                    view_dimension: wgpu::TextureViewDimension::D2Array,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                count: None,
            },
        ],
    });
    let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("terrain-shadow-sample"),
        layout: &bgl,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(sample_view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    });
    (bgl, bind)
}

/// One depth-only pipeline: no fragment stage, single-sample (the
/// cascade map is its own attachment — the MSAA surface contract
/// belongs to the visible pipelines), forward-Z `Less` against a 1.0
/// clear (nothing rasterized closer ⇒ receiver lit). The terrain
/// entries (`vs_shadow0/1`) ride the terrain group(0) alone — never
/// the depth texture in a binding; the craft-cast entries
/// (`flight.wgsl`'s twins of the same names) ride it too, reading
/// only binding 8 (`SU`) in the vertex stage.
fn depth_pipeline(
    device: &wgpu::Device,
    shader: &wgpu::ShaderModule,
    entry: &'static str,
    terrain_bgl: &wgpu::BindGroupLayout,
    vertex_buffers: &[Option<wgpu::VertexBufferLayout<'_>>],
) -> wgpu::RenderPipeline {
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("terrain-shadow"),
        bind_group_layouts: &[Some(terrain_bgl)],
        immediate_size: 0,
    });
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("terrain-shadow"),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some(entry),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: vertex_buffers,
        },
        fragment: None,
        // Two-sided like the visible flight pass (it never culls): a
        // hull's back faces must reach the map or the craft would
        // cast a hollow shadow at grazing sun.
        primitive: wgpu::PrimitiveState {
            cull_mode: None,
            ..wgpu::PrimitiveState::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth32Float,
            depth_write_enabled: Some(true),
            depth_compare: Some(wgpu::CompareFunction::Less),
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        }),
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    })
}

impl ShadowCore {
    /// Create the resources + pipelines. `terrain_bgl` is the terrain
    /// bind-group layout (bindings 0–7 plus the `SU` uniform at 8) —
    /// the shadow pipelines ride the SAME group(0) so the depth pass
    /// reads the very uniforms/heightfield the visible pass does,
    /// and never sees the depth texture in a binding. Since 3b the
    /// craft-cast pair is built here too, from `flight.wgsl` over the
    /// [`crate::flight::FlightVertex`] layout — the documented 3a
    /// extension point, shipped.
    pub(crate) fn new(
        device: &wgpu::Device,
        shader: &wgpu::ShaderModule,
        terrain_bgl: &wgpu::BindGroupLayout,
        vertex_buffers: &[Option<wgpu::VertexBufferLayout<'_>>],
    ) -> Self {
        let (layers, sample_view, tex_handle) = depth_array(device);
        #[cfg(test)]
        let cascade_tex = tex_handle;
        #[cfg(not(test))]
        let _ = tex_handle;
        let sampler = comparison_sampler(device);
        let (sample_bgl, sample_bind) = sample_group(device, &sampler, &sample_view);
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("terrain-shadow-uniforms"),
            size: std::mem::size_of::<ShadowUniforms>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let pipes = [
            depth_pipeline(device, shader, "vs_shadow0", terrain_bgl, vertex_buffers),
            depth_pipeline(device, shader, "vs_shadow1", terrain_bgl, vertex_buffers),
        ];
        let flight_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("flight-shadow"),
            source: wgpu::ShaderSource::Wgsl(include_str!("flight_shadow.wgsl").into()),
        });
        let flight_buffers = [Some(crate::flight::FlightVertex::LAYOUT)];
        let flight_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("flight-shadow"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 8,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let flight_pipes = [
            depth_pipeline(
                device,
                &flight_shader,
                "vs_shadow0",
                &flight_bgl,
                &flight_buffers,
            ),
            depth_pipeline(
                device,
                &flight_shader,
                "vs_shadow1",
                &flight_bgl,
                &flight_buffers,
            ),
        ];
        let flight_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("flight-shadow"),
            layout: &flight_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 8,
                resource: uniforms.as_entire_binding(),
            }],
        });
        Self {
            pipes,
            flight_pipes,
            flight_bind,
            layers,
            cleared: [false; CASCADE_COUNT],
            #[cfg(test)]
            cascade_tex,
            uniforms,
            sample_bgl,
            sample_bind,
        }
    }

    /// The `SU` uniform for the terrain bind group's binding 8.
    pub(crate) fn uniforms_ref(&self) -> &wgpu::Buffer {
        &self.uniforms
    }

    /// The receiving group(1) layout — the terrain/ocean/sky
    /// pipelines' pipeline layout lists it second.
    pub(crate) fn sample_layout(&self) -> &wgpu::BindGroupLayout {
        &self.sample_bgl
    }

    /// The receiving group(1) bind group (depth array + comparison
    /// sampler) — `draw` sets it beside the terrain group(0).
    pub(crate) fn sample_bind(&self) -> &wgpu::BindGroup {
        &self.sample_bind
    }

    /// The cascade depth-array texture (readback handle — the array
    /// carries `COPY_SRC` for diagnostics/tests; see
    /// `TerrainRenderer::cascade_texture`).
    #[cfg(test)]
    pub(crate) fn cascade_texture(&self) -> &wgpu::Texture {
        &self.cascade_tex
    }

    /// 3b cascade hand-off: everything the flight renderer binds to
    /// cast into and receive from the cascades — the `SU` uniform
    /// buffer (its group(0) binding 8, mirroring the terrain's) plus
    /// the receiving group(1) layout + bind group its visible pass
    /// sets beside the light. One tuple so
    /// [`crate::terrain::TerrainRenderer::cascade_resources`] stays a
    /// single forwarding call.
    pub(crate) fn cascade_resources(
        &self,
    ) -> (&wgpu::Buffer, &wgpu::BindGroupLayout, &wgpu::BindGroup) {
        (&self.uniforms, &self.sample_bgl, &self.sample_bind)
    }

    /// Upload the frame's [`ShadowUniforms`]. Callers write it EVERY
    /// armed-or-not flight frame (3b): a skipped disarm write would
    /// leave a stale armed buffer driving `fs_terrain`'s cascade path
    /// with dead matrices after sunset.
    pub(crate) fn write_uniforms(&self, queue: &wgpu::Queue, u: &ShadowUniforms) {
        queue.write_buffer(&self.uniforms, 0, bytemuck::bytes_of(u));
    }

    /// Record both cascade depth passes — one per array layer,
    /// drawing only that layer's kept-instance runs
    /// ([`ShadowDraw::runs`], G3's per-cascade cull — the unculled
    /// build drew the whole terrain batch into BOTH 2048² layers,
    /// 1.11 M displaced vertex invocations to fill <20 % of the maps
    /// at the pad), then (3b) the flight vertex stream through the
    /// craft-cast twin: hulls, debris and the launch complex land in
    /// the same map the terrain casts into — still one non-indexed
    /// craft draw per layer, exactly how the visible flight pass
    /// renders the stream (a single draw over a few hundred verts;
    /// no cull worth its CPU cost there).
    ///
    /// # Empty-layer contract
    ///
    /// A layer with no terrain runs and no craft cast must leave its
    /// map FULLY LIT for the receivers — i.e. at the 1.0 clear
    /// (`LessEqual`: every biased reference `∈ [0,1]` passes against
    /// 1.0). Skipping the pass outright would normally betray that
    /// (the map keeps last frame's casts), so the skip is gated on
    /// the CPU-side [`Self::cleared`] tracking: the first empty frame
    /// records the clear pass (and marks the layer cleared), later
    /// empty frames skip entirely — receivers read a genuinely
    /// all-1.0 map either way, byte-identical to the unculled build's
    /// cleared layer. The `SU` write itself happens in
    /// `TerrainRenderer::draw_shadow` before this and is untouched:
    /// emptiness never disarms anything.
    pub(crate) fn draw_terrain(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        draw: ShadowDraw<'_>,
    ) {
        let flight = draw.flight.filter(|&(_, n)| n > 0);
        let stride = std::mem::size_of::<crate::terrain::TerrainInstance>() as wgpu::BufferAddress;
        for (li, layer) in self.layers.iter().enumerate() {
            let draws_terrain = draw.runs[li].iter().any(|&(_, count)| count > 0);
            if !draws_terrain && flight.is_none() && self.cleared[li] {
                // All-culled layer whose map already sits at the
                // clear — no pass, no clear/store traffic, receivers
                // still fully lit (see the empty-layer contract).
                continue;
            }
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("terrain-shadow"),
                color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: layer,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            let mut drew = false;
            if draws_terrain {
                pass.set_pipeline(&self.pipes[li]);
                pass.set_bind_group(0, draw.bind_group, &[]);
                pass.set_vertex_buffer(0, draw.grid_v);
                pass.set_index_buffer(draw.grid_i, wgpu::IndexFormat::Uint16);
                for &(start, count) in draw.runs[li] {
                    // Narrower draw over the SAME upload: the run's
                    // slice of the shared instance buffer, instance
                    // range starting at 0 of the slice. (A zero-count
                    // run — only constructible by a degenerate input
                    // — records nothing.)
                    if count == 0 {
                        continue;
                    }
                    let lo = wgpu::BufferAddress::from(start) * stride;
                    let hi = wgpu::BufferAddress::from(start + count) * stride;
                    pass.set_vertex_buffer(1, draw.instances.slice(lo..hi));
                    pass.draw_indexed(0..draw.index_count, 0, 0..count);
                }
                drew = true;
            }
            if let Some((buf, n)) = &flight {
                // Own minimal group(0) (binding 8 only — see
                // `flight_bgl`); the vertex buffer slot rebinds to the
                // flight stream's stride because the pipeline's
                // layout changed.
                pass.set_pipeline(&self.flight_pipes[li]);
                pass.set_bind_group(0, &self.flight_bind, &[]);
                pass.set_vertex_buffer(0, buf.slice(..));
                pass.draw(0..*n, 0..1);
                drew = true;
            }
            self.cleared[li] = !drew;
        }
    }
}

// ---------------------------------------------------------------------------
// Tests — the solve-math contract (pure f64/f32, no GPU needed).
// ---------------------------------------------------------------------------

#[cfg(test)]
// Tests signal failure by panicking — §2.13's no-panic rule covers
// shipping fallible paths, not test code.
mod tests {
    // §2.13's no-panic rule covers shipping fallible paths; tests
    // signal failure by panicking.
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    fn unit(v: DVec3) -> DVec3 {
        v / v.norm()
    }

    /// The flight camera the cascades must cover: FLIGHT_FOV_Y 0.9,
    /// 16:9, near 0.1, a chase-style frame with local-level up,
    /// mid-afternoon sun.
    fn cam() -> ShadowCamera {
        ShadowCamera {
            sun_dir: unit(DVec3::new(0.36, -0.24, 0.80)),
            forward: DVec3::X,
            up: DVec3::Z,
            fov_y: 0.9,
            aspect: 16.0 / 9.0,
            near_m: 0.1,
        }
    }

    /// The 8 camera-frustum slice corners for `[d0, d1]`, exactly as
    /// the solve builds them (camera-relative, §2.3).
    fn slice_corners(cam: &ShadowCamera, d0: f64, d1: f64) -> [DVec3; 8] {
        let f = unit(cam.forward);
        let (cr, cu) = camera_frame(f, cam.up);
        let tan_y = libm::tan(cam.fov_y * 0.5);
        let mut out = [DVec3::ZERO; 8];
        let mut k = 0usize;
        for d in [d0, d1] {
            for sy in [-1.0f64, 1.0] {
                for sx in [-1.0f64, 1.0] {
                    out[k] = f * d + cr * (sx * d * tan_y * cam.aspect) + cu * (sy * d * tan_y);
                    k += 1;
                }
            }
        }
        out
    }

    /// `m · (p, 1)` through the f32 column-major matrix — the exact
    /// arithmetic the WGSL `vp * vec4(wrel, 1.0)` performs.
    fn xform(m: &[[f32; 4]; 4], p: DVec3) -> [f32; 4] {
        let v = [p.x as f32, p.y as f32, p.z as f32, 1.0f32];
        let mut out = [0.0f32; 4];
        for r in 0..4 {
            let mut acc = 0.0f32;
            for c in 0..4 {
                acc += m[c][r] * v[c];
            }
            out[r] = acc;
        }
        out
    }

    /// Every f32 entry of two matrices must match bit-for-bit.
    fn bits_equal(a: &[[[f32; 4]; 4]; 2], b: &[[[f32; 4]; 4]; 2]) -> bool {
        a.iter()
            .zip(b.iter())
            .flat_map(|(x, y)| x.iter().zip(y.iter()))
            .flat_map(|(x, y)| x.iter().zip(y.iter()))
            .all(|(x, y)| x.to_bits() == y.to_bits())
    }

    /// Bounds containment: every slice corner of every cascade lands
    /// inside its own `[0,1]³` cascade-clip box through the f32
    /// matrix — the contract `fs_terrain`'s coverage assumption
    /// rests on. Tolerance covers the texel snap (center moves ≤
    /// half a texel in world units ⇒ ≤ `1/RES` in clip) plus the
    /// f32 cast.
    #[test]
    fn slice_corners_are_contained() {
        let cfg = CascadeConfig::default();
        let sol = solve_cascades(&cam(), &cfg);
        let eps = 4.0 / f64::from(SHADOW_MAP_RES);
        for (c, (d0, d1)) in [
            (0usize, (0.1f64, cfg.split_m)),
            (1, (cfg.split_m, cfg.far_m)),
        ] {
            for p in slice_corners(&cam(), d0, d1) {
                let v = xform(&sol.uniforms.vp[c], p);
                let (x, y, z) = (f64::from(v[0]), f64::from(v[1]), f64::from(v[2]));
                assert!(
                    x.abs() <= 1.0 + eps && y.abs() <= 1.0 + eps,
                    "cascade {c}: xy out ({x}, {y}) for {p:?}"
                );
                assert!(
                    (-1.0e-3..=1.0 + 1.0e-3).contains(&z),
                    "cascade {c}: z out {z} for {p:?}"
                );
            }
        }
    }

    /// The texel snap + ladder: a sub-texel camera motion (a 1e-5 rad
    /// yaw nudge — corners move ≤ 0.2 m at the 20 km slice far plane,
    /// ≤ ~2% of either cascade's half-texel) reproduces both matrices
    /// bit-for-bit — extents stayed on their ladder rung and the
    /// snapped center didn't step — while a nudge three orders bigger
    /// (33 m at 20 km, several texels) must move something.
    #[test]
    fn texel_snap_holds_then_steps() {
        let cfg = CascadeConfig::default();
        let base = solve_cascades(&cam(), &cfg);
        let nudged = solve_cascades(
            &ShadowCamera {
                forward: unit(DVec3::new(1.0, 1.0e-5, 0.0)),
                ..cam()
            },
            &cfg,
        );
        assert!(
            bits_equal(&base.uniforms.vp, &nudged.uniforms.vp),
            "sub-texel camera motion changed the cascade matrices"
        );
        let stepped = solve_cascades(
            &ShadowCamera {
                forward: unit(DVec3::new(1.0, 2.0e-3, 0.0)),
                ..cam()
            },
            &cfg,
        );
        assert!(
            !bits_equal(&base.uniforms.vp, &stepped.uniforms.vp),
            "a multi-texel nudge must cross a snap/ladder boundary"
        );
    }

    /// Determinism: same inputs → bit-identical uniforms (the
    /// presentation-side twin of the sim's hash contract — a shimmer
    /// bug here is a determinism bug visually).
    #[test]
    fn solve_is_bit_deterministic() {
        let a = solve_cascades(&cam(), &CascadeConfig::default());
        let b = solve_cascades(&cam(), &CascadeConfig::default());
        assert!(bits_equal(&a.uniforms.vp, &b.uniforms.vp));
        assert_eq!(a.uniforms.cascade, b.uniforms.cascade);
        assert_eq!(a.uniforms.pars, b.uniforms.pars);
        assert_eq!(a.half_extent_m, b.half_extent_m);
    }

    /// Disarmed default: the zeroed `ShadowUniforms` (what the buffer
    /// holds when no frame ever calls `draw_shadow`) reports
    /// unarmed, and the armed solve sets the documented band rows.
    #[test]
    fn armed_flag_contract() {
        assert!(!ShadowUniforms::default().armed());
        let cfg = CascadeConfig::default();
        let sol = solve_cascades(&cam(), &cfg);
        assert!(sol.uniforms.armed());
        assert_eq!(sol.uniforms.pars[1], cfg.split_m as f32);
        assert_eq!(sol.uniforms.pars[2], cfg.far_m as f32);
        // Band = BLEND_BAND_FRAC · split.
        assert!((sol.uniforms.pars[3] - (BLEND_BAND_FRAC * cfg.split_m) as f32).abs() < 0.5);
        // Ortho affine: the math matrix's last row [0,0,0,1] lives at
        // the last ELEMENT of each stored column (storage is
        // column-major `m[col][row]`).
        for m in sol.uniforms.vp {
            assert_eq!((m[0][3], m[1][3], m[2][3], m[3][3]), (0.0, 0.0, 0.0, 1.0));
        }
    }

    /// Coverage bands: an on-axis point inside each band projects
    /// into its cascade's box, and a 15 km point is OUTSIDE cascade
    /// 0's (the split genuinely hands off). This is the property
    /// `fs_terrain`'s distance-based cascade selection leans on:
    /// dist-selected receivers are always inside the selected box.
    #[test]
    fn coverage_bands_split_correctly() {
        let cfg = CascadeConfig::default();
        let sol = solve_cascades(&cam(), &cfg);
        let eps = 4.0 / f64::from(SHADOW_MAP_RES);
        let inside = |c: usize, p: DVec3| -> bool {
            let v = xform(&sol.uniforms.vp[c], p);
            f64::from(v[0]).abs() <= 1.0 + eps
                && f64::from(v[1]).abs() <= 1.0 + eps
                && (0.0..=1.0 + 1.0e-3).contains(&f64::from(v[2]))
        };
        let f = cam().forward;
        assert!(inside(0, f * 1_000.0), "1 km on-axis must hit cascade 0");
        assert!(inside(1, f * 15_000.0), "15 km on-axis must hit cascade 1");
        assert!(
            !inside(0, f * 15_000.0),
            "15 km must fall outside cascade 0's box"
        );
    }

    /// Whether a value sits exactly on a `2^(n/2)` ladder rung
    /// (walks down by √2 to exactly 1.0).
    fn on_ladder(mut v: f64) -> bool {
        const EPS: f64 = 1.0e-9;
        while v > 1.0 + EPS {
            v /= LADDER_SQRT2;
        }
        (v - 1.0).abs() <= EPS
    }

    /// The bias sign contract (the acne blocker): a flat sun-facing
    /// receiver must compare LIT against its own stored texel. Map
    /// depth runs forward-Z-along-travel (closer to the sun =
    /// smaller), the sampler is LessEqual (lit ⇔ ref ≤ stored), and
    /// the receiver's own surface sits in the map at ≈ its own
    /// unbiased depth — so both bias terms must move the reference
    /// TOWARD the sun (smaller). This mirrors `cascade_one`'s exact
    /// f32 arithmetic: normal-offset along +n, then the slope bias
    /// SUBTRACTED from the projected depth.
    #[test]
    fn bias_moves_reference_toward_the_sun() {
        let cam = cam();
        let sol = solve_cascades(&cam, &CascadeConfig::default());
        let cb = sol.uniforms.cascade[0];
        // Flat ground on-axis at 1 km, normal = sun (tan_slope = 0).
        let p = cam.forward * 1_000.0;
        let n = unit(cam.sun_dir);
        let own = xform(&sol.uniforms.vp[0], p)[2];
        // Normal-offset alone: +n on a lit fragment lowers lt·p
        // (lt·n < 0), so the offset-only depth must already be
        // smaller than the receiver's own.
        let off = xform(&sol.uniforms.vp[0], p + n * f64::from(cb[1]))[2];
        assert!(
            off < own,
            "normal-offset must reduce depth: off {off} vs own {own}"
        );
        // The full biased reference (`cascade_one`'s expression):
        // clamp(cd.z − cb.z·(1 + tan_slope), 0, 1) — strictly below
        // the receiver's own depth, so `ref ≤ stored` passes on the
        // receiver's own texel (flat ground stays sunlit).
        let ref_d = (off - cb[2]).clamp(0.0, 1.0);
        assert!(
            ref_d <= own,
            "biased ref {ref_d} must be ≤ receiver's own depth {own}"
        );
        assert!(
            own - ref_d >= cb[2],
            "reference bias must account for the whole margin: \
             own {own}, ref {ref_d}, bias {}",
            cb[2]
        );
    }

    /// Texel arithmetic: every cascade's world texel sits exactly on
    /// a `2^(n/2)` ladder rung, `half = texel · RES/2` exactly, the
    /// texel covers the raw fit (`texel ≥ 2·half_raw/RES`, i.e.
    /// corners stay contained — pinned separately), and the bias
    /// rows follow the texel. At the flight camera this lands
    /// cascade 0 at √2 ≈ 1.41 m/texel (a diagonal light projection
    /// of the 16:9 frustum fits tighter than the axis-aligned
    /// ~1.68 m estimate) — the boot-level sharpness the 2048² budget
    /// buys (see [`SHADOW_MAP_RES`]).
    #[test]
    fn texel_budget_is_as_documented() {
        let sol = solve_cascades(&cam(), &CascadeConfig::default());
        for c in 0..CASCADE_COUNT {
            let t = sol.texel_m[c];
            assert!(on_ladder(t), "cascade {c} texel {t} off the √2 ladder");
            assert_eq!(sol.half_extent_m[c], t * f64::from(SHADOW_MAP_RES) / 2.0);
            assert_eq!(sol.uniforms.cascade[c][0], t as f32);
            assert_eq!(
                sol.uniforms.cascade[c][1],
                (NORMAL_OFFSET_TEXELS * t) as f32
            );
            assert!(
                (1.0..128.0).contains(&t),
                "cascade {c} texel {t} m outside budget"
            );
        }
        assert!(
            sol.texel_m[0] < 2.0 * LADDER_SQRT2,
            "cascade 0 sharper than budgeted?"
        );
        assert!(sol.texel_m[1] > sol.texel_m[0]);
    }

    // -----------------------------------------------------------------
    // G3 — the per-cascade cull's contract.
    // -----------------------------------------------------------------

    /// Which instance indices a run list draws.
    fn covered(runs: &InstanceRuns, len: usize) -> Vec<bool> {
        let mut v = vec![false; len];
        for &(s, c) in runs {
            for i in s..s + c {
                v[i as usize] = true;
            }
        }
        v
    }

    /// The `cam()`-relative point-caster row: zero cube spans and no
    /// skirt make the extent bound ≈ the +1 m pad, so the cull
    /// reduces to the point-in-box test at `origin`.
    fn point_caster(origin: DVec3) -> crate::terrain::TerrainInstance {
        crate::terrain::TerrainInstance {
            origin_rel_eye: [origin.x as f32, origin.y as f32, origin.z as f32, 0.0],
            ..crate::terrain::TerrainInstance::default()
        }
    }

    /// The brute-force surface of a patch, in the exact f64 form of
    /// `terrain_displace` (`origin + R·(dir − dir0) + dir·(h −
    /// skirt)`), sampled over a dense uv grid × the height clamp
    /// ends × skirt on/off — every GPU vertex of the patch is one of
    /// these up to interpolation, and interpolation stays inside the
    /// patch's extent ball.
    fn patch_samples(
        id: crate::terrain::PatchId,
        eye: DVec3,
        radius: f64,
        amp: f64,
    ) -> (Vec<DVec3>, DVec3, f64) {
        use crate::terrain::{patch_cube, patch_instance};
        let (corner, du, dv) = patch_cube(id);
        let c0 = corner + (du + dv) * 0.5;
        let dir0 = c0 / c0.norm();
        let origin = dir0 * radius - eye;
        let inst = patch_instance(id, 0.0, eye, radius);
        let skirt = f64::from(inst.origin_rel_eye[3]);
        let mut out = Vec::new();
        const N: usize = 25;
        for ji in 0..=N {
            for ii in 0..=N {
                let (u, v) = (ii as f64 / N as f64, ji as f64 / N as f64);
                let cube = corner + du * u + dv * v;
                let dir = cube / cube.norm();
                for h in [-amp, 0.0, amp] {
                    for sk in [0.0, skirt] {
                        out.push(origin + radius * (dir - dir0) + dir * (h - sk));
                    }
                }
            }
        }
        (out, origin, skirt)
    }

    /// f64 projection through the f32 matrix entries (a superset of
    /// the GPU's f32 result — the brute-force ground truth must err
    /// towards visibility, never away from it).
    fn xform_f64(m: &[[f32; 4]; 4], p: DVec3) -> [f64; 4] {
        let v = [p.x, p.y, p.z, 1.0];
        let mut out = [0.0f64; 4];
        for (r, o) in out.iter_mut().enumerate() {
            let mut acc = 0.0f64;
            for (c, vc) in v.iter().enumerate() {
                acc += f64::from(m[c][r]) * vc;
            }
            *o = acc;
        }
        out
    }

    /// The cull's two legs against brute force, over real patch
    /// geometry at every interesting scale (whole-face, mid, finest,
    /// face-corner, antipodal) and both cascades of a real solve:
    ///
    /// 1. the extent bound actually bounds — no sampled surface
    ///    point sits further from the baked centre than
    ///    `instance_extent_m` (+pad) claims (the `patch_extent_m`
    ///    Lipschitz claim + the skirt credit, pinned numerically);
    /// 2. visibility ⇒ keep — if ANY sampled point projects into a
    ///    cascade's clip volume, that cascade's test must keep the
    ///    instance (the cull may never remove a visible caster).
    ///
    /// Plus the non-vacuity pins: the sub-camera finest patch is
    /// kept by cascade 0, the antipodal whole face by neither.
    #[test]
    #[allow(clippy::too_many_lines)] // one brute-force recipe over the patch ladder; splitting hides the A/B
    fn cull_bound_covers_the_bruteforce_surface() {
        use crate::terrain::{PatchId, patch_instance};
        let radius = 600_000.0;
        let amp = 2_400.0;
        let eye = DVec3::new(0.0, 0.0, radius + 800.0);
        // Looking straight down at the sub-camera point (face 4),
        // afternoon sun — the pad frame's geometry.
        let sol = solve_cascades(
            &ShadowCamera {
                sun_dir: unit(DVec3::new(0.36, -0.24, 0.80)),
                forward: DVec3::new(0.0, 0.0, -1.0),
                up: DVec3::Y,
                fov_y: 0.9,
                aspect: 16.0 / 9.0,
                near_m: 0.1,
            },
            &CascadeConfig::default(),
        );
        let ids = [
            PatchId {
                face: 4,
                level: 0,
                x: 0,
                y: 0,
            },
            PatchId {
                face: 4,
                level: 4,
                x: 8,
                y: 8,
            },
            PatchId {
                face: 4,
                level: 9,
                x: 256,
                y: 256,
            },
            PatchId {
                face: 4,
                level: 13,
                x: 4096,
                y: 4096,
            },
            PatchId {
                face: 4,
                level: 13,
                x: 0,
                y: 0,
            },
            PatchId {
                face: 4,
                level: 6,
                x: 63,
                y: 0,
            },
            PatchId {
                face: 0,
                level: 5,
                x: 16,
                y: 16,
            },
            // The true far side (face 5 is the eye's antipode here):
            // a LEVEL-4 patch, because a whole-face patch's extent
            // (~half a circumference) legitimately wraps the boxes —
            // conservative keep, not a bug (the pin below wants a
            // caster that must drop, and fine far patches do).
            PatchId {
                face: 5,
                level: 4,
                x: 8,
                y: 8,
            },
        ];
        let mut any_visible = false;
        for id in ids {
            let (samples, origin, _skirt) = patch_samples(id, eye, radius, amp);
            let inst = patch_instance(id, 0.0, eye, radius);
            let centre = DVec3::new(
                f64::from(inst.origin_rel_eye[0]),
                f64::from(inst.origin_rel_eye[1]),
                f64::from(inst.origin_rel_eye[2]),
            );
            let extent = instance_extent_m(&inst, radius, 0.0, amp);
            // Leg 1: the bound bounds (against the f64 origin — the
            // f32 row's own rounding is what the +1 m pad covers).
            let reach = samples
                .iter()
                .map(|p| (*p - origin).norm())
                .fold(0.0f64, f64::max);
            assert!(
                reach <= extent + 1.0,
                "patch {id:?}: surface reach {reach} m exceeds the extent bound {extent} m"
            );
            // Leg 2: visibility ⇒ keep, per cascade.
            for c in 0..CASCADE_COUNT {
                let visible = samples.iter().any(|p| {
                    let v = xform_f64(&sol.uniforms.vp[c], *p);
                    v[0].abs() <= 1.0 && v[1].abs() <= 1.0 && (0.0..=1.0).contains(&v[2])
                });
                let keeps = cascade_keeps(
                    &sol.uniforms.vp[c],
                    [
                        inst.origin_rel_eye[0],
                        inst.origin_rel_eye[1],
                        inst.origin_rel_eye[2],
                    ],
                    extent,
                );
                assert!(
                    !visible || keeps,
                    "cascade {c} culled a visible caster: patch {id:?} at {centre:?}"
                );
                any_visible |= visible;
            }
            let _ = centre;
        }
        assert!(any_visible, "fixture must contain visible casters");
        // Non-vacuity: the sub-camera finest patch casts into
        // cascade 0 (the ground under the craft), the far-side level-4
        // patch casts into nothing.
        let sub = patch_instance(ids[3], 0.0, eye, radius);
        assert!(cascade_keeps(
            &sol.uniforms.vp[0],
            [
                sub.origin_rel_eye[0],
                sub.origin_rel_eye[1],
                sub.origin_rel_eye[2],
            ],
            instance_extent_m(&sub, radius, 0.0, amp),
        ));
        let anti = patch_instance(ids[7], 0.0, eye, radius);
        for c in 0..CASCADE_COUNT {
            assert!(!cascade_keeps(
                &sol.uniforms.vp[c],
                [
                    anti.origin_rel_eye[0],
                    anti.origin_rel_eye[1],
                    anti.origin_rel_eye[2],
                ],
                instance_extent_m(&anti, radius, 0.0, amp),
            ));
        }
    }

    /// The run lists: per-layer membership follows the box (the
    /// differential pin — 1 km on-axis is cascade 0's, 15 km only
    /// cascade 1's, both beyond `coverage_bands_split_correctly`),
    /// out-of-box casters drop, and the merge/cap machinery stays
    /// conservative (every kept instance covered, runs bounded).
    /// Interesting casters are spaced past `CULL_MERGE_GAP` so
    /// `covered` reads exact membership (a widened run reads as
    /// coverage of its swallowed filler — pinned by the merge arm
    /// below).
    #[test]
    fn runs_follow_the_boxes_and_stay_conservative() {
        let sol = solve_cascades(&cam(), &CascadeConfig::default());
        let f = unit(cam().forward);
        let lateral = unit(DVec3::new(0.0, 0.0, 1.0)); // camera up, ⊥ forward
        let filler = || point_caster(lateral * 500_000.0);
        // Indices 0/10/20/30: in-0, neither, in-1-only, in-0.
        let mut rows = Vec::new();
        for i in 0..40usize {
            rows.push(match i {
                0 => point_caster(f * 1_000.0),
                10 => point_caster(lateral * 500_000.0),
                20 => point_caster(f * 15_000.0),
                30 => point_caster(f * 1_100.0),
                _ => filler(),
            });
        }
        let runs = cascade_runs(&sol.uniforms, &rows, 40, &[600_000.0, 0.0, 0.0, 0.0]);
        let cov0 = covered(&runs[0], rows.len());
        let cov1 = covered(&runs[1], rows.len());
        for (name, cov) in [("cascade 0", &cov0), ("cascade 1", &cov1)] {
            assert!(!cov[10], "{name}: the off-axis caster must drop");
            assert!(cov[0] && cov[30], "{name}: the 1 km casters must stay");
        }
        assert!(!cov0[20], "cascade 0: 15 km is past its split");
        assert!(cov1[20], "cascade 1: 15 km is its band");
        // Merge: alternating keep/cull with gaps under
        // CULL_MERGE_GAP folds into one run that covers every keep.
        let alt: Vec<_> = (0..40u32)
            .map(|i| {
                if i % 2 == 0 {
                    point_caster(f * 1_000.0)
                } else {
                    point_caster(lateral * 500_000.0)
                }
            })
            .collect();
        let runs = cascade_runs(&sol.uniforms, &alt, 40, &[600_000.0, 0.0, 0.0, 0.0]);
        assert_eq!(runs[0].len(), 1, "alternating keeps merge into one run");
        assert!(covered(&runs[0], 40).iter().step_by(2).all(|b| *b));
        // Cap: keeps spaced past the merge gap still land in ≤
        // CULL_MAX_RUNS runs with every keep covered (the widened
        // tail swallows extras, never drops them).
        let sparse: Vec<_> = (0..400u32)
            .map(|i| {
                if i % 10 == 0 {
                    point_caster(f * (1_000.0 + 0.1 * f64::from(i)))
                } else {
                    point_caster(lateral * 500_000.0)
                }
            })
            .collect();
        let runs = cascade_runs(&sol.uniforms, &sparse, 400, &[600_000.0, 0.0, 0.0, 0.0]);
        assert!(runs[0].len() <= CULL_MAX_RUNS);
        let cov = covered(&runs[0], 400);
        for (i, b) in cov.iter().enumerate() {
            if i % 10 == 0 {
                assert!(b, "capped run list dropped a keep at {i}");
            }
        }
    }

    /// The far-list exclusion gate is COMPUTED, not assumed: at the
    /// home body both boxes sit far inside the 1 Mm split (so the far
    /// list never needs testing — see `cascade_runs`'s proof), while
    /// a gas-giant `world_scale` grows the box past the split and
    /// the far list joins the test. The differential pin: a caster
    /// parked at the GIANT box's own centre (constructed from the
    /// reconstructed light basis — inside that box by construction,
    /// ≥ the split along the camera forward so it legitimately routes
    /// far) is invisible to the default solve's runs and kept by the
    /// giant solve's.
    #[test]
    fn far_list_gate_follows_the_computed_box_radius() {
        let f = unit(cam().forward);
        let scale = 100.0f64;
        let giant = solve_cascades(
            &cam(),
            &CascadeConfig {
                world_scale: scale,
                ..CascadeConfig::default()
            },
        );
        // The giant box centre in world (rel-eye) coordinates:
        // rows `lr·sx` give the orthonormal light basis back.
        let centre_of = |m: &[[f32; 4]; 4]| -> DVec3 {
            let row =
                |r: usize| DVec3::new(f64::from(m[0][r]), f64::from(m[1][r]), f64::from(m[2][r]));
            let (r0, r1, r2) = (row(0), row(1), row(2));
            let (sx, sy, sz) = (r0.norm(), r1.norm(), r2.norm());
            let cx = -f64::from(m[3][0]) / sx;
            let cy = -f64::from(m[3][1]) / sy;
            let z_mid = -f64::from(m[3][2]) / sz + 0.5 / sz;
            r0 / sx * cx + r1 / sy * cy + r2 / sz * z_mid
        };
        let p = centre_of(&giant.uniforms.vp[1]);
        assert!(
            p.norm() > crate::flight::FAR_SPLIT_M,
            "fixture: the giant box centre must sit beyond the split (|p| {})",
            p.norm()
        );
        assert!(
            f.dot(p) - 1.0 >= crate::flight::FAR_SPLIT_M,
            "fixture: the caster must legitimately route far"
        );
        let rows = [point_caster(f * 1_000.0), point_caster(p)];
        // Default solve: box radius ≪ split → far list untested, and
        // the near keep is the only covered instance.
        let home = solve_cascades(&cam(), &CascadeConfig::default());
        assert!(cascade_box_radius(&home.uniforms.vp[1]) < crate::flight::FAR_SPLIT_M);
        let runs = cascade_runs(&home.uniforms, &rows, 1, &[600_000.0, 0.0, 0.0, 0.0]);
        assert_eq!(covered(&runs[1], 2), vec![true, false]);
        // Giant solve: box radius ≥ split → the far list joins, and
        // the far caster (at the box's own centre) is kept.
        assert!(cascade_box_radius(&giant.uniforms.vp[1]) >= crate::flight::FAR_SPLIT_M);
        let runs = cascade_runs(&giant.uniforms, &rows, 1, &[600_000.0, 0.0, 0.0, 0.0]);
        assert_eq!(
            covered(&runs[1], 2),
            vec![true, true],
            "a box past the split must test (and here keep) the far caster"
        );
    }

    /// A surfaceless adapter + device (LowPower prefers lavapipe —
    /// run on the weakest conformant implementation, like the render
    /// gate) plus the error flag any uncaptured wgpu error sets.
    /// `None` when no driver exists (the documented skip case).
    #[cfg(not(target_arch = "wasm32"))]
    fn headless_device() -> Option<(
        wgpu::Device,
        wgpu::Queue,
        std::sync::Arc<std::sync::atomic::AtomicBool>,
    )> {
        use std::sync::atomic::{AtomicBool, Ordering};
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::PRIMARY,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: None,
            ..Default::default()
        }))
        .ok()?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("shadow-test"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            ..Default::default()
        }))
        .ok()?;
        let errored = std::sync::Arc::new(AtomicBool::new(false));
        let flag = errored.clone();
        device.on_uncaptured_error(std::sync::Arc::new(move |_| {
            flag.store(true, Ordering::Relaxed);
        }));
        Some((device, queue, errored))
    }

    /// A tiny but real flight frame: baked heightfield, a handful of
    /// quadtree patches near the sub-camera point, a chase-style
    /// camera looking at the body centre, afternoon sun — plus the
    /// armed cascade solve for it.
    #[cfg(not(target_arch = "wasm32"))]
    fn tiny_frame() -> (crate::terrain::TerrainPacket, ShadowUniforms) {
        use crate::terrain::{LodConfig, TerrainUniforms, patch_instance, select_patches};
        use csp_core::terrain::HeightField;
        let field = std::sync::Arc::new(HeightField::bake(601, 64, 32));
        let eye = DVec3::new(600_800.0, 0.0, 0.0);
        let radius = 600_000.0;
        let amp = 2_400.0;
        let lod = LodConfig {
            max_level: 4,
            split_factor: 4.0,
            max_patches: 24,
        };
        let terrain: Vec<_> = select_patches(eye, radius, amp, &lod)
            .into_iter()
            .map(|id| patch_instance(id, 0.0, eye, radius))
            .collect();
        let sun = unit(DVec3::new(0.36, -0.24, 0.80));
        let packet = crate::terrain::TerrainPacket {
            uniforms: TerrainUniforms {
                eye_rel_body: [eye.x as f32, eye.y as f32, eye.z as f32, 0.0],
                sun_dir: [sun.x as f32, sun.y as f32, sun.z as f32, 0.0],
                planet: [radius as f32, 0.0, 0.0, amp as f32],
                ..TerrainUniforms::default()
            },
            heightfield: Some(field),
            terrain,
            sky: true,
            ..crate::terrain::TerrainPacket::default()
        };
        let su = solve_cascades(
            &ShadowCamera {
                sun_dir: sun,
                forward: DVec3::new(-1.0, 0.0, 0.0),
                up: DVec3::Z,
                fov_y: 0.9,
                aspect: 16.0 / 9.0,
                near_m: 0.1,
            },
            &CascadeConfig::default(),
        )
        .uniforms;
        (packet, su)
    }

    /// Whether this test is REQUIRED on this host —
    /// `CSP_SHADOW_GATE_REQUIRED` set (and not "0") turns a missing
    /// adapter into a failure instead of a skip: otherwise losing
    /// the Vulkan ICD silently degenerates the armed-path validation
    /// into a permanent green no-op (the same contract as the render
    /// gate's `CSP_RENDER_GATE_REQUIRED`).
    #[cfg(not(target_arch = "wasm32"))]
    fn shadow_gate_required() -> bool {
        std::env::var("CSP_SHADOW_GATE_REQUIRED").is_ok_and(|v| !v.is_empty() && v != "0")
    }

    /// The ARMED path on a real device: `draw_shadow` (terrain cast +
    /// 3b's craft cast) into the cascade layers, then the visible
    /// scene pass binding group(1) — exactly the frame order the app
    /// uses. Asserts zero wgpu errors through submit+poll, which is
    /// what catches the class of bug the pure tests can't see
    /// (usage-scope conflicts, bind group/pipeline layout mismatches,
    /// entry-point typos). Skips loudly when no adapter exists —
    /// unless [`CSP_SHADOW_GATE_REQUIRED`] is set, which makes the
    /// missing adapter a failure (CI installs lavapipe and sets the
    /// flag).
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    #[allow(clippy::too_many_lines)] // one armed frame recipe; splitting fragments the pass order it pins
    fn armed_cascade_passes_validate_on_device() {
        use crate::terrain::TerrainRenderer;
        let Some((device, queue, errored)) = headless_device() else {
            assert!(
                !shadow_gate_required(),
                "shadow device test: CSP_SHADOW_GATE_REQUIRED is set but no headless \
                 GPU adapter exists — install a Vulkan ICD (`mesa-vulkan-drivers` ships \
                 lavapipe's libvulkan_lvp.so) or unset the flag"
            );
            eprintln!(
                "shadow device test: SKIP — no headless GPU adapter \
                 (same contract as csp-view's render gate)"
            );
            return;
        };
        let (packet, su) = tiny_frame();
        // The 3b frame order: cascade depth passes (encoder), then the
        // visible scene pass over an MSAA color + reverse-Z depth
        // attachment pair like the app's.
        let format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let size = 64u32;
        let mut renderer = TerrainRenderer::new(&device, format);
        let mut flight =
            crate::flight::FlightRenderer::new(&device, format, (size, size), &renderer);
        // A minimal flight stream: one lit triangle (a caster — its
        // `world` is camera-relative like the real extract emits) and
        // one unlit (the zero-normal sky/plume shape the cast must
        // degenerate away rather than project at the eye).
        let lit = |w: [f32; 3]| crate::flight::FlightVertex {
            pos: [0.0, 0.0, 0.5],
            world: w,
            nrm: [0.0, 0.0, 1.0],
            color: [0.8, 0.8, 0.8, 1.0],
            mat: [0.2, 32.0, 1.0],
        };
        let unlit = crate::flight::FlightVertex {
            pos: [0.0, 0.0, 0.5],
            world: [0.0; 3],
            nrm: [0.0; 3],
            color: [0.1, 0.1, 0.2, 1.0],
            mat: [0.0; 3],
        };
        let fp = crate::flight::FlightPacket {
            vertices: vec![
                lit([-20.0, -20.0, -300.0]),
                lit([20.0, -20.0, -300.0]),
                lit([0.0, 20.0, -300.0]),
                unlit,
                unlit,
                unlit,
            ],
            shadow: su,
            ..crate::flight::FlightPacket::default()
        };
        let attachment = |format| {
            let tex = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("shadow-test-target"),
                size: wgpu::Extent3d {
                    width: size,
                    height: size,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: crate::MSAA_SAMPLES,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            tex.create_view(&wgpu::TextureViewDescriptor::default())
        };
        let msaa_view = attachment(format);
        let depth_view = attachment(wgpu::TextureFormat::Depth32Float);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("shadow-test"),
        });
        let cast = flight.stage_cascade_cast(&device, &queue, &fp);
        renderer.draw_shadow(&mut encoder, &device, &queue, &packet, &su, cast);
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("shadow-test-scene"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &msaa_view,
                    resolve_target: None,
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
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            renderer.draw(&mut pass, &device, &queue, &packet);
            flight.draw(&mut pass, &device, &queue, &fp);
        }
        // And the disarm contract on-device: a following zeroed `SU`
        // write is legal and silences the cascade path.
        renderer.draw_shadow(
            &mut encoder,
            &device,
            &queue,
            &packet,
            &ShadowUniforms::default(),
            None,
        );
        queue.submit([encoder.finish()]);
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
        assert!(
            !errored.load(std::sync::atomic::Ordering::Relaxed),
            "armed cascade frame produced wgpu validation errors"
        );

        // 3b readback half: pins that the CRAFT CAST actually lands in
        // the map. The silent-discard failure mode (the module-layout
        // bug this suite caught during arming) leaves every layer at
        // the 1.0 clear while raising no wgpu error at all — only a
        // texel readback can see it. Recipe: a terrain packet whose
        // single whole-face patch sits on the ANTIPODE of tiny_frame's
        // +X eye (face 1 — every vertex lands far outside the cascade
        // z window and clips, so the map starts provably empty), then
        // a lit craft triangle inside cascade 0's volume MUST mark
        // layer 0, and the no-cast control MUST stay clear.
        let eye = DVec3::new(600_800.0, 0.0, 0.0);
        let mut anti = packet.clone();
        anti.terrain = vec![crate::terrain::patch_instance(
            crate::terrain::PatchId {
                face: 1,
                level: 0,
                x: 0,
                y: 0,
            },
            0.0,
            eye,
            600_000.0,
        )];
        // A lit triangle hovering over the on-axis 1 km ground point,
        // offset within the camera plane so it survives the light
        // projection, plus one unlit triangle that must NOT cast.
        let base = DVec3::new(-1_000.0, 0.0, 0.0);
        let lit = |o: DVec3| crate::flight::FlightVertex {
            pos: [0.0, 0.0, 0.5],
            world: [o.x as f32, o.y as f32, o.z as f32],
            nrm: [0.0, 0.0, 1.0],
            color: [0.8, 0.8, 0.8, 1.0],
            mat: [0.2, 32.0, 1.0],
        };
        let unlit = crate::flight::FlightVertex {
            pos: [0.0, 0.0, 0.5],
            world: [0.0; 3],
            nrm: [0.0; 3],
            color: [0.1, 0.1, 0.2, 1.0],
            mat: [0.0; 3],
        };
        let craft_packet = crate::flight::FlightPacket {
            vertices: vec![
                lit(base),
                lit(base + DVec3::new(0.0, 200.0, 200.0)),
                lit(base + DVec3::new(0.0, -200.0, 200.0)),
                unlit,
                unlit,
                unlit,
            ],
            shadow: su,
            ..crate::flight::FlightPacket::default()
        };
        let mut cast_renderer = TerrainRenderer::new(&device, format);
        let mut cast_flight =
            crate::flight::FlightRenderer::new(&device, format, (size, size), &cast_renderer);
        let stage = cast_flight.stage_cascade_cast(&device, &queue, &craft_packet);
        let read_layer0 =
            |renderer: &mut TerrainRenderer, craft: Option<(wgpu::Buffer, u32)>| -> usize {
                let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("shadow-cast-readback"),
                });
                renderer.draw_shadow(&mut enc, &device, &queue, &anti, &su, craft);
                let row_bytes = (SHADOW_MAP_RES * 4) as u64;
                let buf = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("shadow-cast-readback"),
                    size: row_bytes * u64::from(SHADOW_MAP_RES),
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                });
                enc.copy_texture_to_buffer(
                    wgpu::TexelCopyTextureInfo {
                        texture: renderer.cascade_texture(),
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::DepthOnly,
                    },
                    wgpu::TexelCopyBufferInfo {
                        buffer: &buf,
                        layout: wgpu::TexelCopyBufferLayout {
                            offset: 0,
                            bytes_per_row: Some(row_bytes as u32),
                            rows_per_image: Some(SHADOW_MAP_RES),
                        },
                    },
                    wgpu::Extent3d {
                        width: SHADOW_MAP_RES,
                        height: SHADOW_MAP_RES,
                        depth_or_array_layers: 1,
                    },
                );
                queue.submit([enc.finish()]);
                let (tx, rx) = std::sync::mpsc::channel();
                buf.map_async(wgpu::MapMode::Read, .., move |r| {
                    let _ = tx.send(r);
                });
                let _ = device.poll(wgpu::PollType::wait_indefinitely());
                rx.recv().unwrap().expect("cast readback map failed");
                let data = buf.get_mapped_range(..).unwrap().to_vec();
                buf.unmap();
                data.chunks_exact(4)
                    .filter(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]) < 1.0)
                    .count()
            };
        // Control first: the antipodal terrain alone leaves layer 0
        // at the clear — otherwise the assertions below prove nothing.
        let clear = read_layer0(&mut cast_renderer, None);
        let covered = read_layer0(&mut cast_renderer, stage);
        assert_eq!(
            clear, 0,
            "the antipodal-terrain control should leave the cascade map empty"
        );
        assert!(
            covered > 100,
            "the craft cast landed nothing in the cascade map \
             (covered {covered} texels — the silent-discard regression)"
        );
    }

    /// A pad-like flight frame for the G3 device tests: real LOD walk
    /// (level 9) around the sub-camera point at 800 m altitude, the
    /// §2.3 near/far routing applied, and the matching armed solve.
    /// Returns the packet + `SU` + the camera forward (the routing
    /// rule's axis).
    #[cfg(not(target_arch = "wasm32"))]
    fn pad_frame() -> (crate::terrain::TerrainPacket, ShadowUniforms, DVec3) {
        use crate::terrain::{LodConfig, TerrainUniforms, patch_instance, select_patches};
        use csp_core::terrain::HeightField;
        let field = std::sync::Arc::new(HeightField::bake(601, 64, 32));
        let radius = 600_000.0;
        let amp = 2_400.0;
        let eye = DVec3::new(0.0, 0.0, radius + 800.0);
        let fwd = DVec3::new(0.0, 0.0, -1.0);
        let lod = LodConfig {
            max_level: 9,
            split_factor: 4.0,
            max_patches: 384,
        };
        let mut near_l = Vec::new();
        let mut far_l = Vec::new();
        for id in select_patches(eye, radius, amp, &lod) {
            let inst = patch_instance(id, 0.0, eye, radius);
            let o = inst.origin_rel_eye;
            let fwd_d = f64::from(o[0]) * fwd.x + f64::from(o[1]) * fwd.y + f64::from(o[2]) * fwd.z;
            // The §2.3 routing rule (the far-field site's expression,
            // relief included this time).
            if fwd_d - crate::terrain::patch_extent_m(id, radius, amp) >= crate::flight::FAR_SPLIT_M
            {
                far_l.push(inst);
            } else {
                near_l.push(inst);
            }
        }
        let terrain_far_start = near_l.len() as u32;
        let mut terrain = near_l;
        terrain.extend(far_l);
        let sun = unit(DVec3::new(0.36, -0.24, 0.80));
        let packet = crate::terrain::TerrainPacket {
            uniforms: TerrainUniforms {
                eye_rel_body: [eye.x as f32, eye.y as f32, eye.z as f32, 0.0],
                sun_dir: [sun.x as f32, sun.y as f32, sun.z as f32, 0.0],
                planet: [radius as f32, 0.0, 0.0, amp as f32],
                ..TerrainUniforms::default()
            },
            heightfield: Some(field),
            terrain,
            terrain_far_start,
            sky: false,
            ..crate::terrain::TerrainPacket::default()
        };
        let su = solve_cascades(
            &ShadowCamera {
                sun_dir: sun,
                forward: fwd,
                up: DVec3::Y,
                fov_y: 0.9,
                aspect: 16.0 / 9.0,
                near_m: 0.1,
            },
            &CascadeConfig::default(),
        )
        .uniforms;
        (packet, su, fwd)
    }

    /// Full-layer cascade-map readback (layer `z`, every texel's
    /// bytes) — the G3 tests' evidence channel.
    #[cfg(not(target_arch = "wasm32"))]
    fn read_cascade_layer(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        renderer: &crate::terrain::TerrainRenderer,
        layer: u32,
    ) -> Vec<u8> {
        let row_bytes = (SHADOW_MAP_RES * 4) as u64;
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("g3-cascade-readback"),
            size: row_bytes * u64::from(SHADOW_MAP_RES),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("g3-cascade-readback"),
        });
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: renderer.cascade_texture(),
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: 0,
                    y: 0,
                    z: layer,
                },
                aspect: wgpu::TextureAspect::DepthOnly,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row_bytes as u32),
                    rows_per_image: Some(SHADOW_MAP_RES),
                },
            },
            wgpu::Extent3d {
                width: SHADOW_MAP_RES,
                height: SHADOW_MAP_RES,
                depth_or_array_layers: 1,
            },
        );
        queue.submit([enc.finish()]);
        let (tx, rx) = std::sync::mpsc::channel();
        buf.map_async(wgpu::MapMode::Read, .., move |r| {
            let _ = tx.send(r);
        });
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
        rx.recv().unwrap().expect("g3 readback map failed");
        let data = buf.get_mapped_range(..).unwrap().to_vec();
        buf.unmap();
        data
    }

    /// G3's correctness pin: the per-cascade cull must be invisible
    /// in the maps. A culled `draw_shadow` and the unculled control
    /// (`draw_shadow_unculled` — the pre-G3 whole-batch draw) render
    /// the same packet through the same solve on two fresh renderers;
    /// BOTH cascade layers must read back byte-identical. The cull is
    /// allowed to drop only instances that project no fragment, so
    /// any byte diff is a removed visible caster (or a routing
    /// regression). The fixture first proves the cull actually culls
    /// here (coverage < instances), so the pin is not vacuous.
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn per_cascade_cull_matches_the_unculled_maps_bit_for_bit() {
        use crate::terrain::TerrainRenderer;
        let Some((device, queue, errored)) = headless_device() else {
            assert!(
                !shadow_gate_required(),
                "shadow device test: CSP_SHADOW_GATE_REQUIRED is set but no headless \
                 GPU adapter exists"
            );
            eprintln!("shadow device test: SKIP — no headless GPU adapter");
            return;
        };
        let (packet, su, _fwd) = pad_frame();
        assert!(su.armed());
        // Non-vacuity: both layers drop a solid share of the batch.
        let runs = cascade_runs(
            &su,
            &packet.terrain,
            packet.terrain_far_start,
            &packet.uniforms.planet,
        );
        for (c, r) in runs.iter().enumerate() {
            let kept: u32 = r.iter().map(|&(_, n)| n).sum();
            eprintln!(
                "g3 byte-identity fixture: cascade {c} keeps {kept}/{} instances \
                 in {} runs",
                packet.terrain.len(),
                r.len()
            );
            assert!(
                kept < packet.terrain.len() as u32,
                "cascade {c} culled nothing — the byte-identity pin is vacuous"
            );
        }
        let format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let render = |unculled: bool| -> Vec<Vec<u8>> {
            let mut renderer = TerrainRenderer::new(&device, format);
            let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("g3-ab"),
            });
            if unculled {
                renderer.draw_shadow_unculled(&mut enc, &device, &queue, &packet, &su, None);
            } else {
                renderer.draw_shadow(&mut enc, &device, &queue, &packet, &su, None);
            }
            queue.submit([enc.finish()]);
            let _ = device.poll(wgpu::PollType::wait_indefinitely());
            (0..CASCADE_COUNT)
                .map(|l| read_cascade_layer(&device, &queue, &renderer, l as u32))
                .collect()
        };
        let culled = render(false);
        let unculled = render(true);
        assert!(
            !errored.load(std::sync::atomic::Ordering::Relaxed),
            "the G3 A/B frames produced wgpu validation errors"
        );
        for (c, (a, b)) in culled.iter().zip(unculled.iter()).enumerate() {
            assert_eq!(
                a, b,
                "cascade {c}: the cull changed the map — a visible caster was removed"
            );
        }
    }

    /// G3's empty-layer contract on device: an armed row whose every
    /// terrain instance culls out (lateral far-field patches —
    /// near-routed by §2.3, far outside both boxes) must leave BOTH
    /// layers at the fully-lit clear. Lit-by-clear is the compare
    /// path's own semantics: every receiver's biased reference is
    /// clamped into `[0,1]` and `LessEqual` passes `ref ≤ 1.0`
    /// everywhere, so an all-1.0 map shades every receiver sunlit.
    /// Both the clear pass (first frame) and the tracked skip
    /// (second frame — no pass at all) must read back exactly 1.0 in
    /// every texel; the packet stays armed throughout, pinning that
    /// emptiness never disarms.
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn all_culled_layers_stay_armed_and_fully_lit() {
        use crate::terrain::{PatchId, TerrainRenderer, patch_instance};
        let Some((device, queue, errored)) = headless_device() else {
            assert!(
                !shadow_gate_required(),
                "shadow device test: CSP_SHADOW_GATE_REQUIRED is set but no headless \
                 GPU adapter exists"
            );
            eprintln!("shadow device test: SKIP — no headless GPU adapter");
            return;
        };
        let (mut packet, su, _fwd) = pad_frame();
        // Three level-4 patches on face 2 (+Y): the horizon ring at
        // ~850 km, near-routed (forward depth ~0), all-culled.
        let eye = DVec3::new(0.0, 0.0, 600_800.0);
        packet.terrain = vec![
            patch_instance(
                PatchId {
                    face: 2,
                    level: 4,
                    x: 8,
                    y: 8,
                },
                0.0,
                eye,
                600_000.0,
            ),
            patch_instance(
                PatchId {
                    face: 2,
                    level: 4,
                    x: 9,
                    y: 8,
                },
                0.0,
                eye,
                600_000.0,
            ),
            patch_instance(
                PatchId {
                    face: 3,
                    level: 4,
                    x: 8,
                    y: 7,
                },
                0.0,
                eye,
                600_000.0,
            ),
        ];
        packet.terrain_far_start = 0;
        let runs = cascade_runs(&su, &packet.terrain, 0, &packet.uniforms.planet);
        for (c, r) in runs.iter().enumerate() {
            assert!(
                r.is_empty(),
                "fixture: cascade {c} keeps lateral far-field patches?"
            );
        }
        let format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let mut renderer = TerrainRenderer::new(&device, format);
        for frame in 0..2 {
            let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("g3-empty"),
            });
            renderer.draw_shadow(&mut enc, &device, &queue, &packet, &su, None);
            queue.submit([enc.finish()]);
            let _ = device.poll(wgpu::PollType::wait_indefinitely());
            for l in 0..CASCADE_COUNT {
                let data = read_cascade_layer(&device, &queue, &renderer, l as u32);
                let lit = data
                    .chunks_exact(4)
                    .all(|t| f32::from_le_bytes([t[0], t[1], t[2], t[3]]) == 1.0);
                assert!(
                    lit,
                    "frame {frame} layer {l}: an all-culled pass left non-clear texels"
                );
            }
        }
        assert!(
            !errored.load(std::sync::atomic::Ordering::Relaxed),
            "the all-culled frames produced wgpu validation errors"
        );
    }

    /// G3+ upload-memo contract on device, over the full app frame
    /// recipe (stage cast → `draw_shadow` → far pass → near pass →
    /// ocean — `draw_shadow` leads, so the frame stamp bumps and
    /// syncs 2/3 ride the fold memo, and `draw` rides the staged-
    /// verts memo):
    ///
    /// 1. steady state — the SAME packet on consecutive frames must
    ///    dump byte-identical color (the memos changed nothing);
    /// 2. refresh — mutating the packet's instance and vertex Vecs
    ///    IN PLACE (same allocations: the exact input shape a stale
    ///    memo would swallow) must reach the screen and the cascade
    ///    map on the very next frame;
    /// 3. steady state again on the mutated content.
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    #[allow(clippy::too_many_lines)] // one full frame recipe; splitting fragments the phase order it pins
    fn upload_memos_preserve_dumps_and_refresh_content() {
        use crate::terrain::TerrainRenderer;
        let Some((device, queue, errored)) = headless_device() else {
            assert!(
                !shadow_gate_required(),
                "shadow device test: CSP_SHADOW_GATE_REQUIRED is set but no headless \
                 GPU adapter exists"
            );
            eprintln!("shadow device test: SKIP — no headless GPU adapter");
            return;
        };
        let (mut packet, su, _fwd) = pad_frame();
        // A real near-pass projection for the color dump (the
        // far-field device test's straight-down camera — matches
        // `pad_frame`'s solve). Reverse-Z rows, column-major cast.
        let eye = DVec3::new(0.0, 0.0, 600_800.0);
        let (right, upv, z_out) = (DVec3::X, DVec3::Y, DVec3::Z);
        let view = [
            [right.x, right.y, right.z, 0.0],
            [upv.x, upv.y, upv.z, 0.0],
            [z_out.x, z_out.y, z_out.z, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        let f = 1.0 / (0.9f64 * 0.5).tan();
        let proj = [
            [f / (16.0 / 9.0), 0.0, 0.0, 0.0],
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
        let wgsl_m = |m: &[[f64; 4]; 4]| {
            let mut o = [[0.0f32; 4]; 4];
            for r in 0..4 {
                for c in 0..4 {
                    o[c][r] = m[r][c] as f32;
                }
            }
            o
        };
        packet.uniforms.view_proj = wgsl_m(&near_vp);
        packet.uniforms_far.view_proj = wgsl_m(&near_vp);
        packet.sky = true;
        // The craft stream: a big lit triangle over the camera's
        // ground point (its color change must move pixels), plus the
        // unlit degenerate fillers.
        let mut fp = crate::flight::FlightPacket {
            shadow: su,
            ..crate::flight::FlightPacket::default()
        };
        let lit = |color: [f32; 4], w: [f32; 3]| crate::flight::FlightVertex {
            pos: [0.0, 0.0, 0.5],
            world: w,
            nrm: [0.0, 0.0, 1.0],
            color,
            mat: [0.2, 32.0, 1.0],
        };
        fp.vertices = vec![
            lit([0.8, 0.8, 0.8, 1.0], [-20.0, -20.0, -300.0]),
            lit([0.8, 0.8, 0.8, 1.0], [20.0, -20.0, -300.0]),
            lit([0.8, 0.8, 0.8, 1.0], [0.0, 20.0, -300.0]),
            crate::flight::FlightVertex {
                pos: [0.0, 0.0, 0.5],
                world: [0.0; 3],
                nrm: [0.0; 3],
                color: [0.1, 0.1, 0.2, 1.0],
                mat: [0.0; 3],
            },
        ];
        let format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let size = 64u32;
        let mut terrain = TerrainRenderer::new(&device, format);
        let mut flight =
            crate::flight::FlightRenderer::new(&device, format, (size, size), &terrain);
        let attachment = |usage, sample_count, fmt| {
            let tex = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("g3-memo-target"),
                size: wgpu::Extent3d {
                    width: size,
                    height: size,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count,
                dimension: wgpu::TextureDimension::D2,
                format: fmt,
                usage,
                view_formats: &[],
            });
            tex.create_view(&wgpu::TextureViewDescriptor::default())
        };
        // One full app frame → the resolved color dump.
        let shade = |terrain: &mut TerrainRenderer,
                     flight: &mut crate::flight::FlightRenderer,
                     tp: &crate::terrain::TerrainPacket,
                     fpacket: &crate::flight::FlightPacket|
         -> Vec<u8> {
            let msaa = attachment(
                wgpu::TextureUsages::RENDER_ATTACHMENT,
                crate::MSAA_SAMPLES,
                format,
            );
            let resolve = attachment(
                wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                1,
                format,
            );
            let depth = attachment(
                wgpu::TextureUsages::RENDER_ATTACHMENT,
                crate::MSAA_SAMPLES,
                wgpu::TextureFormat::Depth32Float,
            );
            let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("g3-memo"),
            });
            let cast = flight.stage_cascade_cast(&device, &queue, fpacket);
            terrain.draw_shadow(&mut enc, &device, &queue, tp, &fpacket.shadow, cast);
            {
                let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("g3-memo-far"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &msaa,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                        depth_slice: None,
                    })],
                    depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                        view: &depth,
                        depth_ops: Some(wgpu::Operations {
                            load: wgpu::LoadOp::Clear(0.0),
                            store: wgpu::StoreOp::Store,
                        }),
                        stencil_ops: None,
                    }),
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                terrain.draw_far(&mut pass, &device, &queue, tp);
                flight.draw_far(&mut pass, &device, &queue, fpacket);
            }
            {
                let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("g3-memo-near"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &msaa,
                        resolve_target: Some(&resolve),
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        },
                        depth_slice: None,
                    })],
                    depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                        view: &depth,
                        depth_ops: Some(wgpu::Operations {
                            load: wgpu::LoadOp::Clear(0.0),
                            store: wgpu::StoreOp::Store,
                        }),
                        stencil_ops: None,
                    }),
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                terrain.draw(&mut pass, &device, &queue, tp);
                flight.draw(&mut pass, &device, &queue, fpacket);
                terrain.draw_ocean(&mut pass, tp);
            }
            let row_bytes = (size * 4) as u64;
            let buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("g3-memo-readback"),
                size: row_bytes * u64::from(size),
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            enc.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture: resolve.texture(),
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyBufferInfo {
                    buffer: &buf,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(row_bytes as u32),
                        rows_per_image: Some(size),
                    },
                },
                wgpu::Extent3d {
                    width: size,
                    height: size,
                    depth_or_array_layers: 1,
                },
            );
            queue.submit([enc.finish()]);
            let (tx, rx) = std::sync::mpsc::channel();
            buf.map_async(wgpu::MapMode::Read, .., move |r| {
                let _ = tx.send(r);
            });
            let _ = device.poll(wgpu::PollType::wait_indefinitely());
            rx.recv().unwrap().expect("g3 memo readback map failed");
            let data = buf.get_mapped_range(..).unwrap().to_vec();
            buf.unmap();
            data
        };
        // 1. Steady state: same packet, two frames — memos ride.
        let a = shade(&mut terrain, &mut flight, &packet, &fp);
        let map_a = read_cascade_layer(&device, &queue, &terrain, 0);
        let b = shade(&mut terrain, &mut flight, &packet, &fp);
        assert_eq!(a, b, "the memo path changed the frame dump");
        // 2. Refresh: mutate BOTH Vecs in place — the exact recycled
        // allocation a stale memo would swallow.
        let nearest = packet
            .terrain
            .iter()
            .enumerate()
            .min_by(|(_, x), (_, y)| {
                let d = |v: [f32; 4]| {
                    f64::from(v[0]) * f64::from(v[0])
                        + f64::from(v[1]) * f64::from(v[1])
                        + f64::from(v[2]) * f64::from(v[2])
                };
                d(x.origin_rel_eye).total_cmp(&d(y.origin_rel_eye))
            })
            .map(|(i, _)| i)
            .unwrap_or(0);
        packet.terrain[nearest].origin_rel_eye[0] += 400.0;
        for v in fp.vertices.iter_mut().take(3) {
            v.color = [0.1, 0.9, 0.1, 1.0];
        }
        let c = shade(&mut terrain, &mut flight, &packet, &fp);
        let map_c = read_cascade_layer(&device, &queue, &terrain, 0);
        assert_ne!(
            a, c,
            "an in-place packet mutation failed to reach the frame dump"
        );
        assert_ne!(
            map_a, map_c,
            "an in-place instance mutation failed to reach the cascade map"
        );
        // 3. Steady state on the mutated content.
        let d = shade(&mut terrain, &mut flight, &packet, &fp);
        assert_eq!(c, d, "the memo path changed the mutated frame dump");
        assert!(
            !errored.load(std::sync::atomic::Ordering::Relaxed),
            "the memo frames produced wgpu validation errors"
        );
        let _ = eye;
    }

    /// The sky-only guard (M5 §5.2-R2 arrival polish): an ARMED row
    /// handed to `draw_shadow` with a packet that carries NO terrain
    /// instances must not change the craft's shading. The cascade
    /// passes early-return without touching the depth array, so an
    /// honored armed row would drive `fs_main`'s cascade lookup into
    /// the never-cleared map (zeros on a fresh session — every
    /// comparison fails and the direct sun term dies; the previous
    /// body's stale map after an SOI switch) — exactly the silent
    /// craft-darkening failure the producer-side disarm prevents.
    /// Pinned here at the RENDERER so even a producer regression
    /// cannot ship it: frame A shades the lit craft triangle through
    /// the armed row, frame B is the documented disarmed fallback —
    /// the readbacks must be BIT-IDENTICAL and the craft brightly
    /// lit in both.
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    #[allow(clippy::too_many_lines)] // one frame recipe; splitting fragments the pass order it pins
    fn armed_row_with_sky_only_packet_keeps_craft_fully_lit() {
        use crate::terrain::TerrainRenderer;
        let Some((device, queue, errored)) = headless_device() else {
            assert!(
                !shadow_gate_required(),
                "shadow device test: CSP_SHADOW_GATE_REQUIRED is set but no headless \
                 GPU adapter exists — install a Vulkan ICD or unset the flag"
            );
            eprintln!(
                "shadow device test: SKIP — no headless GPU adapter \
                 (same contract as csp-view's render gate)"
            );
            return;
        };
        let (packet, su) = tiny_frame();
        assert!(
            su.armed(),
            "the fixture solve must arrive armed for the A frame"
        );
        // The wait-state packet: same uniforms, but no heightfield and
        // no terrain/ocean instances — exactly what the view emits
        // while the heightfield bake runs.
        let mut sky_only = packet.clone();
        sky_only.heightfield = None;
        sky_only.terrain = Vec::new();
        sky_only.ocean = Vec::new();
        assert!(
            sky_only.terrain.is_empty(),
            "fixture sanity: the wait-state packet must carry no casters"
        );
        let format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let size = 64u32;
        let mut renderer = TerrainRenderer::new(&device, format);
        let mut flight =
            crate::flight::FlightRenderer::new(&device, format, (size, size), &renderer);
        // One lit NDC-space craft triangle (the `pos` stream is clip
        // space — the fallback sky quad uses literal NDC corners), its
        // `world` inside cascade 0's volume so the visibility lookup
        // actually runs.
        let craft = |shadow: ShadowUniforms| crate::flight::FlightPacket {
            vertices: vec![
                crate::flight::FlightVertex {
                    pos: [-0.5, -0.5, 0.5],
                    world: [-20.0, -20.0, -300.0],
                    nrm: [0.0, 0.0, 1.0],
                    color: [0.8, 0.8, 0.8, 1.0],
                    mat: [0.2, 32.0, 1.0],
                },
                crate::flight::FlightVertex {
                    pos: [0.5, -0.5, 0.5],
                    world: [20.0, -20.0, -300.0],
                    nrm: [0.0, 0.0, 1.0],
                    color: [0.8, 0.8, 0.8, 1.0],
                    mat: [0.2, 32.0, 1.0],
                },
                crate::flight::FlightVertex {
                    pos: [0.0, 0.5, 0.5],
                    world: [0.0, 20.0, -300.0],
                    nrm: [0.0, 0.0, 1.0],
                    color: [0.8, 0.8, 0.8, 1.0],
                    mat: [0.2, 32.0, 1.0],
                },
            ],
            shadow,
            ..crate::flight::FlightPacket::default()
        };
        // One frame's shaded readback: the app's order — `draw_shadow`
        // (the SU write; the passes skip on the sky-only packet) then
        // the scene pass — into an MSAA color resolved for copy.
        let attachment = |usage, sample_count, fmt| {
            let tex = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("sky-only-guard-target"),
                size: wgpu::Extent3d {
                    width: size,
                    height: size,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count,
                dimension: wgpu::TextureDimension::D2,
                format: fmt,
                usage,
                view_formats: &[],
            });
            tex.create_view(&wgpu::TextureViewDescriptor::default())
        };
        let shade = |renderer: &mut TerrainRenderer,
                     flight: &mut crate::flight::FlightRenderer,
                     row: ShadowUniforms|
         -> Vec<u8> {
            let fp = craft(row);
            let msaa = attachment(
                wgpu::TextureUsages::RENDER_ATTACHMENT,
                crate::MSAA_SAMPLES,
                format,
            );
            let resolve = attachment(
                wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                1,
                format,
            );
            let depth = attachment(
                wgpu::TextureUsages::RENDER_ATTACHMENT,
                crate::MSAA_SAMPLES,
                wgpu::TextureFormat::Depth32Float,
            );
            let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("sky-only-guard"),
            });
            let cast = flight.stage_cascade_cast(&device, &queue, &fp);
            renderer.draw_shadow(&mut enc, &device, &queue, &sky_only, &row, cast);
            {
                let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("sky-only-guard-scene"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &msaa,
                        resolve_target: Some(&resolve),
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                        depth_slice: None,
                    })],
                    depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                        view: &depth,
                        depth_ops: Some(wgpu::Operations {
                            load: wgpu::LoadOp::Clear(0.0),
                            store: wgpu::StoreOp::Store,
                        }),
                        stencil_ops: None,
                    }),
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                flight.draw(&mut pass, &device, &queue, &fp);
            }
            let row_bytes = (size * 4) as u64;
            let buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("sky-only-guard-readback"),
                size: row_bytes * u64::from(size),
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            enc.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture: resolve.texture(),
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyBufferInfo {
                    buffer: &buf,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(row_bytes as u32),
                        rows_per_image: Some(size),
                    },
                },
                wgpu::Extent3d {
                    width: size,
                    height: size,
                    depth_or_array_layers: 1,
                },
            );
            queue.submit([enc.finish()]);
            let (tx, rx) = std::sync::mpsc::channel();
            buf.map_async(wgpu::MapMode::Read, .., move |r| {
                let _ = tx.send(r);
            });
            let _ = device.poll(wgpu::PollType::wait_indefinitely());
            rx.recv()
                .unwrap()
                .expect("sky-only guard readback map failed");
            let data = buf.get_mapped_range(..).unwrap().to_vec();
            buf.unmap();
            data
        };
        let armed = shade(&mut renderer, &mut flight, su);
        let disarmed = shade(&mut renderer, &mut flight, ShadowUniforms::default());
        assert!(
            !errored.load(std::sync::atomic::Ordering::Relaxed),
            "the sky-only guard frame produced wgpu validation errors"
        );
        // The craft actually rasterized brightly (above the ambient
        // floor — the studio light's direct term is the bulk of it).
        let brightest = armed
            .chunks_exact(4)
            .map(|p| p.iter().take(3).copied().max().unwrap_or(0))
            .max()
            .unwrap_or(0);
        assert!(
            brightest > 80,
            "the craft triangle barely rasterized (max channel {brightest}) — \
             the fixture went dark and the A/B pair proves nothing"
        );
        // The contract: the armed row changed NOTHING on a sky-only
        // packet — bit-identical to the disarmed fallback.
        assert_eq!(
            armed, disarmed,
            "an armed SU row must not shade the craft while the packet is \
             sky-only (the never-cleared cascade map killed the sun term)"
        );
    }
}
