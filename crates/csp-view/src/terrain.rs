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
use crate::map::{MapCameraF64, rot3};

/// Heightfield bake dimensions (equirect texels). 1024×512 ≈ 3.7 km
/// texels at Terrin's 600 km radius — the low-frequency surface the
/// R2 quadtree displaces; finer relief is R4 scatter/VT territory.
const HEIGHTFIELD_W: u32 = 1024;
const HEIGHTFIELD_H: u32 = 512;

/// Terrain amplitude as a fraction of the body radius — ~2.4 km of
/// relief at Terrin, proportionally less on moons.
const AMPLITUDE_FRAC: f64 = 0.004;

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
    Some(Arc::new(rt::BiomeField {
        width: img.info.width,
        height: img.info.height,
        indices,
        colors,
    }))
}

/// The baked heightfield cache — one per dominant body, rebuilt when
/// the staged body id/seed/dims change (SOI transfer mid-flight).
/// Presentation data only.
#[derive(Resource, Default)]
pub struct TerrainCache {
    /// Body id + seed the current field was baked for.
    key: Option<(String, u64)>,
    /// The baked field.
    field: Option<Arc<rt::HeightField>>,
}

impl TerrainCache {
    /// The heightfield for `(body_id, seed)` — baked on first use, then
    /// Arc-shared (the packet clones are free).
    fn field(&mut self, body_id: &str, seed: u64) -> Arc<rt::HeightField> {
        let key = (body_id.to_owned(), seed);
        if self.key.as_ref() != Some(&key) {
            self.field = Some(Arc::new(rt::HeightField::bake(
                seed,
                HEIGHTFIELD_W,
                HEIGHTFIELD_H,
            )));
            self.key = Some(key);
        }
        // `key == self.key` here by construction.
        match self.field.as_ref() {
            Some(f) => Arc::clone(f),
            None => Arc::new(rt::HeightField::bake(seed, HEIGHTFIELD_W, HEIGHTFIELD_H)),
        }
    }
}

/// Does this leaf patch plausibly touch the ocean datum? Samples a
/// 5×5 direction grid across the patch on the CPU heightfield; a patch
/// whose lowest sample still exceeds the cull margin can't contain a
/// shoreline at its own resolution (margin bounds the miss on dips
/// narrower than a fifth of the patch).
///
/// `unspin` is the inertial→fixed rotation (`R(axis, −phase)`): patch
/// directions are inertial but the heightfield is body-fixed — the
/// same conjugate the shader applies before `height_at`.
fn patch_might_hold_sea(
    field: &rt::HeightField,
    id: rt::PatchId,
    amplitude_m: f64,
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
    lo * amplitude_m <= (amplitude_m * rt::OCEAN_CULL_FRAC).max(250.0)
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

/// The `RenderExtract`-side packet build: quadtree select around the
/// camera, per-patch instance baking, ocean emission, uniforms. Called
/// from `extract_flight` — `cam`/`input` are the solved presentation
/// camera; `env` is the staged physics environment.
pub(crate) fn build_packet(
    cfg: &TerrainConfig,
    cache: &mut TerrainCache,
    env: &FlightEnv,
    cam: &MapCameraF64,
    yaw: f64,
    pitch: f64,
    viewport_px: (f64, f64),
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
    let leaves = rt::select_patches(eye_rel, env.radius_m, &cfg.lod);
    let mut terrain = Vec::with_capacity(leaves.len());
    let mut ocean = Vec::new();
    for &id in &leaves {
        terrain.push(rt::patch_instance(id, 0.0, eye_rel, env.radius_m));
        if let Some(datum) = body.ocean_datum_m
            && patch_might_hold_sea(&field, id, body.amplitude_m, unspin)
        {
            ocean.push(rt::patch_instance(id, 1.0, eye_rel, datum));
        }
    }

    let rot = rot3(yaw, pitch);
    let mut view_rot = [[0.0f32; 4]; 4];
    for c in 0..3 {
        for r in 0..3 {
            view_rot[c][r] = rot[r][c] as f32;
        }
    }
    let aspect = (viewport_px.0 / viewport_px.1.max(1.0)).max(1e-6);
    let tan_y = (crate::flight::FLIGHT_FOV_Y * 0.5).tan();
    let sun = env.sun_dir;
    let sun_dir = if sun.norm_squared() > 0.0 {
        sun / sun.norm()
    } else {
        FALLBACK_SUN / FALLBACK_SUN.norm()
    };
    let atmo = (env.atm_top_m > 0.0).then_some(rt::AtmoDesc {
        radius_m: env.radius_m,
        top_m: env.atm_top_m,
        scale_height_m: env.atm_scale_height_m.max(1.0),
    });
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
                0.0,
                0.0,
            ],
            misc: [(tan_y * aspect) as f32, tan_y as f32, 0.0, 0.0],
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
        },
        heightfield: Some(field),
        biome: body.biome,
        atmo,
        terrain,
        ocean,
        sky: true,
    }
}
