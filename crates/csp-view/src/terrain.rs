//! R2 terrain staging for the flight view (PLAN §5.2 "CDLOD/quadtree
//! terrain, per-patch f64 origins, precomputed-LUT scattering, ocean,
//! planet-scale LOD").
//!
//! This module owns the *presentation* side of the surface: per-body
//! [`TerrainConfig`] (authored `TerrainDef` seed + ocean datum + decoded
//! biome palette), the lazily baked [`HeightField`] cache, and the
//! packet builder `extract_flight` calls. The quadtree, noise, LUT and
//! instance baking all live in `csp_render::terrain` — this module only
//! wires the staged `FlightEnv` + chase camera into them (§2.5:
//! RenderExtract reads mirrors, never the sim).
//!
//! Determinism: heights are a pure function of `noise_seed`
//! (§3.0's `TerrainDef`); the bake is one-time per body and Arc-shared.
//! Nothing here is sim state — it is the R2 "pre-baked tiles" path.

use std::collections::BTreeMap;
use std::sync::Arc;

use bevy_ecs::prelude::*;
use csp_core::{DQuat, DVec3};
use csp_render::terrain as rt;

use crate::flight::FlightEnv;
use crate::map::MapCameraF64;

// Heightfield bake dims + amplitude share `csp_core::terrain`'s
// constants — the physics contact surface (`csp-phys` `TerrainSurface`)
// bakes the SAME field at the SAME scale, so the ground the solver
// contacts is bit-identical to the surface this module draws.
// 1024×512 ≈ 3.7 km texels at Terrin's 600 km radius — the low-
// frequency surface the R2 quadtree displaces; finer relief is R4
// scatter/VT territory.
use csp_core::terrain::{
    AMPLITUDE_FRAC, BAKE_HEIGHT as HEIGHTFIELD_H, BAKE_WIDTH as HEIGHTFIELD_W,
};

/// Fallback sun direction when the world has no resolvable star —
/// unnormalized (callers divide by `norm`, like `flight.rs`'s copy).
const FALLBACK_SUN: DVec3 = DVec3 {
    x: 0.4,
    y: 0.3,
    z: 0.86,
};

/// Per-body terrain parameters — built from `BodyDef` at `View::new`,
/// or synthesised from the body id for worlds without authored data.
#[derive(Clone)]
pub struct TerrainBody {
    /// `TerrainDef::noise_seed` — the world generator's identity.
    pub seed: u64,
    /// Height amplitude (m) — `AMPLITUDE_FRAC · radius`.
    pub amplitude_m: f64,
    /// Ocean datum radius (m), `None` for dry bodies.
    pub ocean_datum_m: Option<f64>,
    /// Decoded biome palette (slot-resolved), `None` → height ramp.
    pub biome: Option<Arc<rt::BiomeField>>,
}

/// The view's terrain configuration — a `BTreeMap` (deterministic
/// iteration) keyed by body id, installed by `View::new` from the
/// loaded system; `with_sim` worlds get the empty default and
/// [`TerrainBody::synthesized`] fills in deterministically.
#[derive(Resource, Default)]
pub struct TerrainConfig {
    /// Per-body authored terrain params.
    pub bodies: BTreeMap<String, TerrainBody>,
    /// LOD selection tunables.
    pub lod: rt::LodConfig,
    /// The real-art texture set (decoded `data/art/terrain.csar`) —
    /// `None` keeps the procedural fallback shading.
    pub art: Option<Arc<rt::TerrainArt>>,
}

/// Deterministic seed for a body with no authored `TerrainDef` — a
/// string hash of the id, so unlisted bodies still get a stable world.
fn seed_of(id: &str) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for b in id.as_bytes() {
        h = (h ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

impl TerrainBody {
    /// The fallback entry for a body with no authored config — the
    /// same shape as authored, seeded by name (deterministic).
    fn synthesized(id: &str, radius_m: f64) -> Self {
        Self {
            seed: seed_of(id),
            amplitude_m: radius_m * AMPLITUDE_FRAC,
            ocean_datum_m: None,
            biome: None,
        }
    }
}

/// Build the per-body table from the loaded system. A `heightmap:`
/// ref is the R4/native-tier hook — at R2 it logs once and falls back
/// to the authored noise seed.
pub fn config_from_bodies(bodies: &[csp_data::LoadedBody]) -> TerrainConfig {
    let mut map = BTreeMap::new();
    for b in bodies {
        let def = &b.def;
        if def.terrain.heightmap.is_some() {
            log::warn!(
                "terrain: {} authors a heightmap — R2 uses noise_seed instead",
                def.id.as_str()
            );
        }
        map.insert(
            def.id.as_str().to_owned(),
            TerrainBody {
                seed: def.terrain.noise_seed,
                amplitude_m: def.radius_m * AMPLITUDE_FRAC,
                ocean_datum_m: def.ocean.as_ref().map(|o| o.datum_radius_m),
                biome: b.biomes.as_ref().and_then(biome_field),
            },
        );
    }
    TerrainConfig {
        bodies: map,
        lod: rt::LodConfig::default(),
        art: None,
    }
}

/// Map a biome id to its surface tint (linear RGB) — keyword match on
/// the authored ids, with a deterministic hash-grey fallback so an
/// unknown biome still reads as *placed*.
fn biome_color(id: &str) -> [f32; 4] {
    let c = |r: f32, g: f32, b: f32| [r, g, b, 1.0];
    if id.contains("ocean") || id.contains("sea") {
        c(0.05, 0.14, 0.30)
    } else if id.contains("ice") || id.contains("cap") {
        c(0.82, 0.87, 0.93)
    } else if id.contains("tundra") {
        c(0.50, 0.52, 0.42)
    } else if id.contains("desert") || id.contains("dune") {
        c(0.72, 0.62, 0.38)
    } else if id.contains("mountain") || id.contains("highland") {
        c(0.44, 0.40, 0.36)
    } else if id.contains("forest") {
        c(0.10, 0.28, 0.12)
    } else if id.contains("grass") || id.contains("plain") {
        c(0.26, 0.44, 0.16)
    } else if id.contains("crater") || id.contains("regolith") {
        c(0.48, 0.45, 0.42)
    } else {
        let h = seed_of(id);
        let v = 0.28 + 0.22 * ((h >> 8) & 0xff) as f32 / 255.0;
        c(v, v * 0.96, v * 0.92)
    }
}

/// Decode a loaded `biomes.png` into the shader-side [`BiomeField`]:
/// PNG palette indices resolved to palette *slots* (legend order,
/// ≤16), unmapped pixels slot 255 (shader keeps the height ramp).
fn biome_field(loaded: &csp_data::LoadedBiomeMap) -> Option<Arc<rt::BiomeField>> {
    let img = match csp_data::decode_indexed_png(&loaded.png) {
        Ok(i) => i,
        Err(e) => {
            log::warn!("terrain: biome map failed to decode: {e}");
            return None;
        }
    };
    let mut slot_of = [255u8; 256];
    let mut colors = [[0.45f32, 0.42, 0.38, 1.0]; 16];
    for (slot, entry) in loaded.legend.entries.iter().enumerate().take(16) {
        slot_of[entry.index as usize] = slot as u8;
        colors[slot] = biome_color(entry.id.as_str());
    }
    let indices: Vec<u8> = img.indices.iter().map(|&px| slot_of[px as usize]).collect();
    Some(Arc::new(rt::BiomeField::new(
        img.info.width,
        img.info.height,
        indices,
        colors,
    )))
}

/// The baked heightfield cache — one per dominant body, rebuilt when
/// the staged body id/seed/dims change (SOI transfer mid-flight).
/// The bake (524k texels × 13 octaves) used to run synchronously on
/// whichever thread first sampled it — the frame a body became
/// dominant stalled outright (C12-F5). On native it now runs on a
/// worker thread: [`Self::field`] returns a flat datum stand-in
/// until the real field lands, so every caller (camera floor,
/// shadow anchor, ocean cull, packet displacement) still samples
/// the SAME field the shader draws — the honest "surface" while the
/// bake is in flight. wasm has no threads and keeps the synchronous
/// path (a one-time hitch the demo already pays at startup).
/// Presentation data only.
#[derive(Resource, Default)]
pub struct TerrainCache {
    /// Body id + seed the current field was baked for.
    key: Option<(String, u64)>,
    /// The baked field.
    field: Option<Arc<rt::HeightField>>,
    /// Completed fields per `(body_id, seed)` — a body visited
    /// before swaps back in instantly (bounded by body count).
    done: BTreeMap<(String, u64), Arc<rt::HeightField>>,
    /// The in-flight background bake for `key` (native only).
    #[cfg(not(target_arch = "wasm32"))]
    pending: Option<std::sync::Mutex<std::sync::mpsc::Receiver<Arc<rt::HeightField>>>>,
    /// The flat datum stand-in drawn while `pending` runs —
    /// zero-height, so the drawn surface equals `radius_m` and the
    /// camera floor / shadow / ocean tests bound it exactly.
    flat: Option<Arc<rt::HeightField>>,
}

impl TerrainCache {
    /// The heightfield for `(body_id, seed)` — the real field once
    /// baked, the flat stand-in while a background bake is in flight
    /// (both share the caller contract "this is the surface the frame
    /// draws").
    fn field(&mut self, body_id: &str, seed: u64) -> Arc<rt::HeightField> {
        let key = (body_id.to_owned(), seed);
        if self.key.as_ref() != Some(&key) {
            // A body visited before swaps back in for free.
            self.field = self.done.get(&key).cloned();
            self.flat = None;
            #[cfg(not(target_arch = "wasm32"))]
            {
                self.pending = None;
                if self.field.is_none() {
                    let (tx, rx) = std::sync::mpsc::channel();
                    match std::thread::Builder::new()
                        .name("terrain-heightfield-bake".into())
                        .spawn(move || {
                            let _ = tx.send(Arc::new(rt::HeightField::bake(
                                seed,
                                HEIGHTFIELD_W,
                                HEIGHTFIELD_H,
                            )));
                        }) {
                        Ok(_handle) => self.pending = Some(std::sync::Mutex::new(rx)),
                        Err(e) => {
                            log::warn!(
                                "terrain: bake thread failed to spawn ({e}) — baking inline"
                            );
                            self.field = Some(Arc::new(rt::HeightField::bake(
                                seed,
                                HEIGHTFIELD_W,
                                HEIGHTFIELD_H,
                            )));
                        }
                    }
                }
            }
            #[cfg(target_arch = "wasm32")]
            {
                if self.field.is_none() {
                    self.field = Some(Arc::new(rt::HeightField::bake(
                        seed,
                        HEIGHTFIELD_W,
                        HEIGHTFIELD_H,
                    )));
                }
            }
            self.key = Some(key);
        }
        #[cfg(not(target_arch = "wasm32"))]
        if self.field.is_none()
            && let Some(rx) = &self.pending
        {
            let done = match rx
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .try_recv()
            {
                Ok(f) => {
                    self.field = Some(f);
                    true
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => false,
                // The worker died mid-bake — take the one-time hit
                // rather than draw a datum sphere forever.
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    log::warn!("terrain: background bake lost — baking inline");
                    self.field = Some(Arc::new(rt::HeightField::bake(
                        seed,
                        HEIGHTFIELD_W,
                        HEIGHTFIELD_H,
                    )));
                    true
                }
            };
            if done {
                self.pending = None;
            }
        }
        // `key == self.key` here by construction; a completed field
        // also joins `done` so a return visit is free.
        if let Some(f) = self.field.as_ref()
            && let Some(k) = self.key.as_ref()
        {
            self.done.entry(k.clone()).or_insert_with(|| Arc::clone(f));
        }
        match self.field.as_ref() {
            Some(f) => Arc::clone(f),
            None => self.flat_field(),
        }
    }

    /// The zero-height stand-in — same dims, a content key from the
    /// tagged stand-in namespace ([`rt::HeightField::flat_key`] sets
    /// bit 0; real bakes keep it clear), so it provably cannot collide
    /// with a real bake and the renderer re-uploads on the swap.
    /// Built once per key miss. wasm compiles it too (the `pending`
    /// machinery is cfg'd out there; the call stays reachable in the
    /// match arm either way).
    fn flat_field(&mut self) -> Arc<rt::HeightField> {
        if self.flat.is_none() {
            let (w, h) = (HEIGHTFIELD_W, HEIGHTFIELD_H);
            let seed = self.key.as_ref().map_or(0, |(_, s)| *s);
            self.flat = Some(Arc::new(rt::HeightField {
                width: w,
                height: h,
                data: vec![0.0; w as usize * h as usize],
                key: rt::HeightField::flat_key(seed, w, h),
            }));
        }
        // `flat` is Some by construction.
        match self.flat.as_ref() {
            Some(f) => Arc::clone(f),
            None => Arc::new(rt::HeightField::bake(0, HEIGHTFIELD_W, HEIGHTFIELD_H)),
        }
    }
}

/// Does this leaf patch plausibly touch the ocean datum? Samples a
/// 5×5 direction grid across the patch on the CPU heightfield; a patch
/// whose lowest sample still exceeds the cull margin can't contain a
/// shoreline at its own resolution (margin bounds the miss on dips
/// narrower than a fifth of the patch).
///
/// The terrain surface sits at `radius_m + h·amplitude_m`; the ocean
/// surface at `datum_m` — which is *not* necessarily the datum radius
/// (an authored `ocean.datum_radius_m` can sit above or below it). The
/// shoreline test therefore compares `lo·amplitude_m` against
/// `(datum_m − radius_m) + margin`, not against the margin alone.
///
/// `unspin` is the inertial→fixed rotation (`R(axis, −phase)`): patch
/// directions are inertial but the heightfield is body-fixed — the
/// same conjugate the shader applies before `height_at`.
fn patch_might_hold_sea(
    field: &rt::HeightField,
    id: rt::PatchId,
    amplitude_m: f64,
    datum_m: f64,
    radius_m: f64,
    unspin: DQuat,
) -> bool {
    let (corner, du, dv) = rt::patch_cube(id);
    let mut lo = f64::INFINITY;
    for j in 0..5 {
        for i in 0..5 {
            let c = corner + du * (i as f64 / 4.0) + dv * (j as f64 / 4.0);
            lo = lo.min(field.sample_dir(unspin.rotate(c / c.norm())));
        }
    }
    let margin = (amplitude_m * rt::OCEAN_CULL_FRAC).max(250.0);
    lo * amplitude_m <= (datum_m - radius_m) + margin
}

/// The chase camera's ground floor along a unit `dir` (bubble-frame,
/// inertial): the local terrain surface radius — `radius_m` plus the
/// baked heightfield's bilinear sample scaled by the body's
/// amplitude — taken as the MAX over a ±1.5-texel ring around the
/// direction. A bilinear texel cell's peak sits at a corner, so the
/// ring bounds the *rendered* surface under the eye even when the eye
/// direction lands mid-cell on a spike; [`crate::flight`] clamps the
/// solved eye above this + the eye margin so an orbit drag cannot
/// bury the camera inside a hill (the datum-only floor let terrain
/// poke through — the camera-went-underground defect).
///
/// Body resolution and seeding match [`build_packet`] exactly (the
/// authored table, else the `synthesized` fallback), and the
/// inertial→fixed `unspin` conjugate is the same `R(axis, −phase)`
/// the shader applies — the clamp measures the same surface the pass
/// draws.
pub(crate) fn surface_floor_radius(
    cfg: &TerrainConfig,
    cache: &mut TerrainCache,
    env: &FlightEnv,
    dir: DVec3,
) -> f64 {
    // Ring taps at ~1.5 equirect texels of angular offset (2π/W ≈
    // 6.1 mrad at 1024 wide) — near the poles a fixed offset covers
    // more longitude texels, which only widens the bound.
    const TAP: f64 = 1.5 * 2.0 * core::f64::consts::PI / HEIGHTFIELD_W as f64;
    ground_radius(cfg, cache, env, dir, TAP)
}

/// The rendered terrain surface radius along an inertial unit `dir`:
/// `radius_m` plus the baked heightfield's bilinear sample scaled by
/// the body's amplitude, taken as the MAX over `dir` plus a ±`tap_rad`
/// ring of four taps around it (radians on the unit sphere — pass a
/// structure's footprint/radius to bound the ground under it). The
/// shared core of [`surface_floor_radius`]: same body resolution,
/// same `R(axis, −phase)` inertial→fixed conjugate the shader applies
/// before `height_at`, so callers measure the surface the pass draws.
/// `crate::structures` anchors the launch-site buildings on it.
pub(crate) fn ground_radius(
    cfg: &TerrainConfig,
    cache: &mut TerrainCache,
    env: &FlightEnv,
    dir: DVec3,
    tap_rad: f64,
) -> f64 {
    let body_id = env.body.as_ref().map_or("", |b| b.as_str());
    let body = cfg
        .bodies
        .get(body_id)
        .cloned()
        .unwrap_or_else(|| TerrainBody::synthesized(body_id, env.radius_m));
    let field = cache.field(body_id, body.seed);
    let phase = env.spin_phase.rem_euclid(2.0 * core::f64::consts::PI);
    let fixed = if phase != 0.0 {
        DQuat::from_axis_angle(env.spin_axis, -phase).rotate(dir)
    } else {
        dir
    };
    let (t1, t2) = {
        let a = if fixed.z.abs() < 0.9 {
            DVec3::Z
        } else {
            DVec3::X
        };
        let t = {
            let t = fixed.cross(a);
            if t.norm_squared() > 1.0e-12 {
                t / t.norm()
            } else {
                DVec3::X
            }
        };
        (t, fixed.cross(t))
    };
    let mut h = field.sample_dir(fixed);
    for off in [t1 * tap_rad, -t1 * tap_rad, t2 * tap_rad, -t2 * tap_rad] {
        let d = fixed + off;
        if d.norm_squared() > 1.0e-12 {
            h = h.max(field.sample_dir(d / d.norm()));
        }
    }
    env.radius_m + h * body.amplitude_m
}

/// Cast a row-major f64 4×4 into the WGSL column-major `[[f32;4];4]`
/// (`out[col][row] = m[row][col]`).
fn m4_wgsl(m: &[[f64; 4]; 4]) -> [[f32; 4]; 4] {
    let mut out = [[0.0f32; 4]; 4];
    for r in 0..4 {
        for c in 0..4 {
            out[c][r] = m[r][c] as f32;
        }
    }
    out
}

/// `fs_sky`'s view→body basis, recovered from `view_proj = P·V` (the
/// solved chase camera — not rebuilt from the orbit angles). The
/// reverse-Z projection's inverse is exact in f64: with
/// `P = [[f/a,0,0,0],[0,f,0,0],[0,0,0,near],[0,0,-1,0]]` the view rows
/// come back as `(row0·tan·aspect, row1·tan, −row3)`. Those rows are
/// the camera axes — and the WGSL `mat4x4` is column-major, so they
/// land as `view_rot`'s COLUMNS: `view_rot * v` then expands the
/// view-space ray over `right`/`up`/`z_out`, giving `fs_sky`'s
/// per-pixel rays exactly the directions the vertex stream projects.
/// (Storing them per-row uploads V — the shader then computes
/// `V·v = Rᵀ·v` and the sky draws rotated away from the view: the
/// "second sky" sliding against the terrain.)
fn sky_view_rot(vp: &[[f64; 4]; 4], tan_y: f64, aspect: f64) -> [[f32; 4]; 4] {
    let mut view_rot = [[0.0f32; 4]; 4];
    for c in 0..3 {
        view_rot[0][c] = (vp[0][c] * tan_y * aspect) as f32; // column 0 = right
        view_rot[1][c] = (vp[1][c] * tan_y) as f32; // column 1 = up
        view_rot[2][c] = (-vp[3][c]) as f32; // column 2 = backward (z_out)
    }
    view_rot
}

/// The vessel blob-shadow anchor — `(uniform_row, craft_alt_m)`.
/// The chase camera's focus IS the vessel CoM, so
/// `focus = eye − z_out·distance_m` recovers it from the solved basis
/// (column 2 of `view_rot` is the camera's world-frame z_out —
/// `solve_chase_camera` points it focus → eye). The shadow lands where
/// the down-sun ray from the craft meets the local surface:
/// `craft − sun·(alt/μ☉)` in the local-horizontal approximation,
/// clamped so a low sun doesn't fling the anchor kilometres away.
/// No sim data crosses into the extract — this is pure camera math.
/// A `w = 0` row disables the blob (no focus, night side, orbit
/// altitude).
#[allow(clippy::too_many_arguments)]
fn vessel_shadow(
    view_rot: &[[f32; 4]; 4],
    cam: &MapCameraF64,
    env: &FlightEnv,
    field: &rt::HeightField,
    unspin: &DQuat,
    amplitude_m: f64,
    sun_dir: DVec3,
) -> ([f32; 4], f32) {
    let z_out = DVec3::new(
        f64::from(view_rot[2][0]),
        f64::from(view_rot[2][1]),
        f64::from(view_rot[2][2]),
    );
    let craft_rel = cam.eye - env.body_center - z_out * cam.distance_m;
    let craft_r = craft_rel.norm();
    if !(craft_r.is_finite() && craft_r > env.radius_m * 0.5) {
        return ([0.0; 4], 0.0);
    }
    let up = craft_rel / craft_r;
    let surf_r = env.radius_m + field.sample_dir(unspin.rotate(up)) * amplitude_m;
    let alt = (craft_r - surf_r).max(0.0);
    let sun_up = sun_dir.dot(up);
    if sun_up <= 0.04 || alt >= 4_000.0 {
        return ([0.0; 4], alt as f32);
    }
    let slide = ((alt + 2.0) / sun_up.max(0.10)).min(1_500.0);
    let hit = craft_rel - sun_dir * slide;
    let shadow_dir = if hit.norm_squared() > 1.0 {
        hit / hit.norm()
    } else {
        up
    };
    // The blob spreads and softens with altitude — a landed craft gets
    // a tight contact shadow, a low flyer a broad diffuse one.
    let radius = (7.0 + alt * 0.22).min(150.0) as f32;
    (
        [
            shadow_dir.x as f32,
            shadow_dir.y as f32,
            shadow_dir.z as f32,
            radius,
        ],
        alt as f32,
    )
}

/// The `RenderExtract`-side packet build: quadtree select around the
/// camera, per-patch instance baking, ocean emission, uniforms. Called
/// from `extract_flight` — `cam`/`input` are the solved presentation
/// camera; `env` is the staged physics environment.
#[allow(clippy::too_many_lines)] // quadtree select + uniform bake is one pass
pub(crate) fn build_packet(
    cfg: &TerrainConfig,
    cache: &mut TerrainCache,
    env: &FlightEnv,
    cam: &MapCameraF64,
    viewport_px: (f64, f64),
    time_s: f64,
) -> rt::TerrainPacket {
    let body_id = env.body.as_ref().map_or("", |b| b.as_str());
    let body = cfg
        .bodies
        .get(body_id)
        .cloned()
        .unwrap_or_else(|| TerrainBody::synthesized(body_id, env.radius_m));
    let field = cache.field(body_id, body.seed);

    // The §2.6.4 spin phase — the same `R(axis, ω·t)` `bubble.rs`
    // co-rotates latched poses by. The heightfield/biome data is
    // body-fixed while patch directions and positions are inertial, so
    // lookups run through the conjugate rotation `unspin`; the shader
    // does the same inside `vs_terrain`. `phase` wraps to [0, 2π)
    // before the f32 cast so long-running clocks don't lose resolution.
    let spin_phase = env.spin_phase.rem_euclid(2.0 * core::f64::consts::PI);
    let unspin = if spin_phase != 0.0 {
        DQuat::from_axis_angle(env.spin_axis, -spin_phase)
    } else {
        DQuat::IDENTITY
    };

    let eye_rel = cam.eye - env.body_center;
    // `amplitude_m` rides the horizon cull — displaced relief up to
    // `radius + amplitude` stays visible past the bare-datum limb.
    let leaves = rt::select_patches(eye_rel, env.radius_m, body.amplitude_m, &cfg.lod);
    let mut terrain = Vec::with_capacity(leaves.len());
    let mut ocean = Vec::new();
    for &id in &leaves {
        terrain.push(rt::patch_instance(id, 0.0, eye_rel, env.radius_m));
        if let Some(datum) = body.ocean_datum_m
            && patch_might_hold_sea(&field, id, body.amplitude_m, datum, env.radius_m, unspin)
        {
            ocean.push(rt::patch_instance(id, 1.0, eye_rel, datum));
        }
    }

    // `view_rot` is the SOLVED chase camera's own basis — recovered
    // from `view_proj` rather than rebuilt from the orbit angles (see
    // `sky_view_rot`; the pre-fix `rot3(yaw, pitch)` basis was the
    // map's ecliptic orbit-camera frame — the sky drew rotated away
    // from the actual view, a second sky sliding against the terrain
    // while orbiting).
    let aspect = (viewport_px.0 / viewport_px.1.max(1.0)).max(1e-6);
    let tan_y = (crate::flight::FLIGHT_FOV_Y * 0.5).tan();
    let view_rot = sky_view_rot(&cam.view_proj, tan_y, aspect);
    let sun = env.sun_dir;
    let sun_dir = if sun.norm_squared() > 0.0 {
        sun / sun.norm()
    } else {
        FALLBACK_SUN / FALLBACK_SUN.norm()
    };

    let (shadow, craft_alt) = vessel_shadow(
        &view_rot,
        cam,
        env,
        &field,
        &unspin,
        body.amplitude_m,
        sun_dir,
    );

    let atmo = (env.atm_top_m > 0.0).then_some(rt::AtmoDesc {
        radius_m: env.radius_m,
        top_m: env.atm_top_m,
        scale_height_m: env.atm_scale_height_m.max(1.0),
    });
    // fs_sky's ray/shell quadratic `b² − c`: `c = |pa|² − r²` is a
    // per-frame constant, baked here in f64 so the ~1e11-magnitude
    // subtraction never runs in f32 (`misc.z` top shell, `misc.w`
    // datum shell — the shader's `b·b − c` then carries only the `b²`
    // rounding error, ~4 decades smaller than the cancelled terms).
    let eye_r2 = eye_rel.norm_squared();
    let r_top = env.radius_m + env.atm_top_m;
    let c_top = (eye_r2 - r_top * r_top) as f32;
    let c_ground = (eye_r2 - env.radius_m * env.radius_m) as f32;
    rt::TerrainPacket {
        uniforms: rt::TerrainUniforms {
            view_proj: m4_wgsl(&cam.view_proj),
            view_rot,
            eye_rel_body: [eye_rel.x as f32, eye_rel.y as f32, eye_rel.z as f32, 0.0],
            sun_dir: [sun_dir.x as f32, sun_dir.y as f32, sun_dir.z as f32, 0.0],
            planet: [
                env.radius_m as f32,
                env.atm_top_m as f32,
                body.ocean_datum_m.unwrap_or(0.0) as f32,
                body.amplitude_m as f32,
            ],
            atmo: [
                env.atm_scale_height_m as f32,
                if body.biome.is_some() { 1.0 } else { 0.0 },
                craft_alt,
                0.0,
            ],
            misc: [(tan_y * aspect) as f32, tan_y as f32, c_top, c_ground],
            spin: [
                env.spin_axis.x as f32,
                env.spin_axis.y as f32,
                env.spin_axis.z as f32,
                spin_phase as f32,
            ],
            rayleigh: [
                rt::RAYLEIGH_BETA[0] as f32,
                rt::RAYLEIGH_BETA[1] as f32,
                rt::RAYLEIGH_BETA[2] as f32,
                0.0,
            ],
            biome_colors: body.biome.as_ref().map_or([[0.0; 4]; 16], |b| b.colors),
            shadow,
            // x: real-texture flag; y: the sim-clock seconds the wave
            // scroll reads (deterministic — tick-derived, never wall).
            art: [
                if cfg.art.is_some() { 1.0 } else { 0.0 },
                time_s as f32,
                0.0,
                0.0,
            ],
            // The pack's per-class means, sRGB bytes → linear — the
            // shader divides class detail by these (the header row is
            // the single source of truth for the layer mean).
            class_means: cfg.art.as_ref().map_or([[0.0; 4]; 4], |a| {
                let mut m = [[0.0f32; 4]; 4];
                for (dst, src) in m.iter_mut().zip(a.albedo.means.iter()) {
                    *dst = [
                        crate::art::srgb_to_linear(f32::from(src[0]) / 255.0),
                        crate::art::srgb_to_linear(f32::from(src[1]) / 255.0),
                        crate::art::srgb_to_linear(f32::from(src[2]) / 255.0),
                        f32::from(src[3]) / 255.0,
                    ];
                }
                m
            }),
        },
        heightfield: Some(field),
        biome: body.biome,
        atmo,
        terrain,
        ocean,
        sky: true,
        art: cfg.art.clone(),
    }
}

#[cfg(test)]
// Tests signal failure by panicking; §2.13's no-panic rule covers
// shipping fallible paths, not test code.
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::map::{mat4_mul, perspective_revz, project_into};

    /// A non-axis-aligned camera basis — a transpose slips through
    /// any test whose axes line up with the coordinate axes.
    fn tilted_view() -> (DVec3, DVec3, DVec3, [[f64; 4]; 4]) {
        let z_out = {
            let d = DVec3::new(0.4, -0.5, 0.75);
            d / d.norm()
        };
        let right = {
            let r = DVec3::Y.cross(z_out);
            r / r.norm()
        };
        let upv = z_out.cross(right);
        let view = [
            [right.x, right.y, right.z, 0.0],
            [upv.x, upv.y, upv.z, 0.0],
            [z_out.x, z_out.y, z_out.z, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ];
        (right, upv, z_out, view)
    }

    /// `sky_view_rot`'s columns must BE the recovered camera axes —
    /// the WGSL `mat4x4` is column-major and `view_rot * v` expands
    /// the ray over them. Storing the axes per-row uploads `V`
    /// instead of `Vᵀ` and `fs_sky` rays rotate against the view
    /// (the "second sky" defect).
    #[test]
    fn view_rot_columns_are_the_camera_axes() {
        let (right, upv, z_out, view) = tilted_view();
        let aspect = 1.6;
        let tan_y = (crate::flight::FLIGHT_FOV_Y * 0.5).tan();
        let vp = mat4_mul(
            &perspective_revz(crate::flight::FLIGHT_FOV_Y, aspect, 0.1),
            &view,
        );
        let m = sky_view_rot(&vp, tan_y, aspect);
        for (c, axis) in [right, upv, z_out].into_iter().enumerate() {
            let got = DVec3::new(f64::from(m[c][0]), f64::from(m[c][1]), f64::from(m[c][2]));
            assert!(
                (got - axis).norm() < 1e-6,
                "column {c} = {got:?}, expected camera axis {axis:?}"
            );
            assert_eq!(m[c][3], 0.0, "column {c} row 3 must pad to 0");
        }
    }

    /// The full shader path: `rd = normalize(view_rot · (ndc·misc,
    /// −1))` must be the world direction that same pixel projects to —
    /// the end-to-end check that fails under a transpose.
    #[test]
    fn sky_ray_roundtrips_through_view_proj() {
        let (_r, _u, _z, view) = tilted_view();
        let aspect = 1.6;
        let tan_y = (crate::flight::FLIGHT_FOV_Y * 0.5).tan();
        let vp = mat4_mul(
            &perspective_revz(crate::flight::FLIGHT_FOV_Y, aspect, 0.1),
            &view,
        );
        let m = sky_view_rot(&vp, tan_y, aspect);
        for &(nx, ny) in &[(0.0, 0.0), (0.8, -0.4), (-0.6, 0.9)] {
            // `fs_sky`: v = (ndc.x·misc.x, ndc.y·misc.y, −1, 0).
            let v = [nx * tan_y * aspect, ny * tan_y, -1.0, 0.0];
            let rd = DVec3::new(
                f64::from(m[0][0]) * v[0] + f64::from(m[1][0]) * v[1] + f64::from(m[2][0]) * v[2],
                f64::from(m[0][1]) * v[0] + f64::from(m[1][1]) * v[1] + f64::from(m[2][1]) * v[2],
                f64::from(m[0][2]) * v[0] + f64::from(m[1][2]) * v[1] + f64::from(m[2][2]) * v[2],
            );
            let rd = rd / rd.norm();
            // A world point along `rd` from the eye (origin — the
            // view carries no translation) must land back on (nx, ny).
            let ndc = project_into(&vp, DVec3::ZERO, rd)
                .unwrap_or_else(|| panic!("ray for ndc ({nx}, {ny}) is behind the eye"));
            assert!(
                (f64::from(ndc[0]) - nx).abs() < 1e-5 && (f64::from(ndc[1]) - ny).abs() < 1e-5,
                "ndc ({nx}, {ny}) ray projected to ({}, {})",
                ndc[0],
                ndc[1]
            );
        }
    }
}
