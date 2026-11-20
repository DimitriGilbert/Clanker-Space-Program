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
// contacts is bit-identical to the surface this module draws. The bake
// runs through `HeightField::bake_profiled` at the body's authored
// `ReliefProfile` dims (M5 §5.2-R2: 1024×512 ≈ 3.7 km texels Legacy,
// 2048×1024 Mountain, at Terrin's 600 km radius); the width constant
// stays imported for the camera-floor tap angle.
use csp_core::terrain::{AMPLITUDE_FRAC, BAKE_WIDTH as HEIGHTFIELD_W, ReliefProfile};
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
    /// Height amplitude (m) — the authored
    /// `TerrainDef::amplitude_frac · radius` (M5 §5.2-R2); the
    /// synthesized fallback keeps `AMPLITUDE_FRAC · radius`.
    pub amplitude_m: f64,
    /// Relief spectrum (M5 §5.2-R2) — which height function and bake
    /// dims [`TerrainCache`] bakes for this body (`TerrainDef::relief`).
    pub profile: ReliefProfile,
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
    /// The real-art texture set (decoded from the cfg-selected terrain
    /// pack — `data/art/terrain.csar` native, `terrain_web.csar` wasm)
    /// — `None` keeps the procedural fallback shading.
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
            // No authored `TerrainDef` to read — the legacy spectrum is
            // the deterministic fallback for unlisted bodies.
            profile: ReliefProfile::Legacy,
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
                // M5 §5.2-R2: amplitude and spectrum are authored
                // (`TerrainDef::amplitude_frac`/`.relief`), not the
                // fixed `AMPLITUDE_FRAC`/Legacy share — every shipped
                // body still authors the legacy values, so nothing
                // changes until Phase 2's data lands.
                amplitude_m: def.radius_m * def.terrain.amplitude_frac,
                profile: def.terrain.relief,
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

/// Cache identity: `(body id, seed, profile seed-fold)` — the fold is
/// [`ReliefProfile::key_seed_fold`], the same constant
/// [`rt::HeightField::bake_profiled_key`] mixes into the field's
/// content key, so a Legacy and a Mountain field for the same
/// `(body, seed)` can never share a cache slot (M5 §5.2-R2).
type CacheKey = (String, u64, u64);

/// The baked heightfield cache — one per dominant body, rebuilt when
/// the staged body id/seed/profile change (SOI transfer mid-flight).
/// The bake (524k texels × 13 octaves legacy, 4× that under Mountain)
/// used to run synchronously on whichever thread first sampled it —
/// the frame a body became dominant stalled outright (C12-F5). On
/// native it now runs on a worker thread and [`Self::field`] returns
/// `None` until the real field lands: NO surface is drawn in the
/// meantime (the packet suppresses terrain+ocean; the sky, craft and
/// launch complex still render) — the pre-5.2-R2 "flat datum
/// stand-in" showed a visibly flat planet for those first seconds and
/// is removed. CPU-side takers of the field (camera floor, structure
/// anchors, shadow) fall back to the datum radius, exactly the number
/// the zeros stand-in used to sample. wasm has no threads and keeps
/// the synchronous path (a one-time hitch the demo already pays at
/// startup — `None` is unreachable there by construction).
/// Presentation data only.
#[derive(Resource, Default)]
pub struct TerrainCache {
    /// Body id + seed + profile fold the current field was baked for.
    key: Option<CacheKey>,
    /// The baked field — `None` while the native background bake for
    /// `key` is in flight.
    field: Option<Arc<rt::HeightField>>,
    /// Completed fields per cache key — a body visited before swaps
    /// back in instantly (bounded by body count × profile).
    done: BTreeMap<CacheKey, Arc<rt::HeightField>>,
    /// C2: the ocean-cull probe table for the current heightfield
    /// (rebuilt with it — see [`SeaProbeMemo`]).
    sea: SeaProbeMemo,
    /// The in-flight background bake for `key` (native only).
    #[cfg(not(target_arch = "wasm32"))]
    pending: Option<std::sync::Mutex<std::sync::mpsc::Receiver<Arc<rt::HeightField>>>>,
}

impl TerrainCache {
    /// The heightfield for `(body_id, seed, profile)` — the real field
    /// once baked, `None` while a native background bake is in flight
    /// (the caller contract: `None` means *draw/sample no displaced
    /// surface this frame* — the packet suppresses terrain+ocean and
    /// the CPU takers fall back to the datum radius, never to a zeros
    /// stand-in). The bake dims follow the profile
    /// ([`rt::HeightField::bake_profiled`], M5 §5.2-R2).
    fn field(
        &mut self,
        body_id: &str,
        seed: u64,
        profile: ReliefProfile,
    ) -> Option<Arc<rt::HeightField>> {
        let key: CacheKey = (body_id.to_owned(), seed, profile.key_seed_fold());
        if self.key.as_ref() != Some(&key) {
            // A body visited before swaps back in for free.
            self.field = self.done.get(&key).cloned();
            #[cfg(not(target_arch = "wasm32"))]
            {
                self.pending = None;
                if self.field.is_none() {
                    let (tx, rx) = std::sync::mpsc::channel();
                    match std::thread::Builder::new()
                        .name("terrain-heightfield-bake".into())
                        .spawn(move || {
                            let _ =
                                tx.send(Arc::new(rt::HeightField::bake_profiled(seed, profile)));
                        }) {
                        Ok(_handle) => self.pending = Some(std::sync::Mutex::new(rx)),
                        Err(e) => {
                            log::warn!(
                                "terrain: bake thread failed to spawn ({e}) — baking inline"
                            );
                            self.field =
                                Some(Arc::new(rt::HeightField::bake_profiled(seed, profile)));
                        }
                    }
                }
            }
            #[cfg(target_arch = "wasm32")]
            {
                // No threads on wasm: the bake runs inline HERE, so by
                // the time `key` is stored the field is already Some —
                // the `None` wait state below is unreachable on this
                // target (no deadlock risk: no channel, no lock).
                if self.field.is_none() {
                    self.field = Some(Arc::new(rt::HeightField::bake_profiled(seed, profile)));
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
                // rather than draw nothing forever.
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    log::warn!("terrain: background bake lost — baking inline");
                    self.field = Some(Arc::new(rt::HeightField::bake_profiled(seed, profile)));
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
        self.field.clone()
    }

    /// The ocean-cull answer for `id`, memoized (C2). On a miss this
    /// runs the verbatim [`patch_might_hold_sea`]; on a hit it returns
    /// the memoized answer, served ONLY inside the phase window the
    /// miss proved the answer stable over — so every frame's packet is
    /// bit-identical to an always-recompute extract. Callers pass the
    /// same `unspin` they would have handed the probe plus the
    /// `(axis, wrapped phase)` it derives from.
    #[allow(clippy::too_many_arguments)] // the probe's inputs + the phase pair
    fn sea_probe(
        &mut self,
        field: &rt::HeightField,
        id: rt::PatchId,
        amplitude_m: f64,
        datum_m: f64,
        radius_m: f64,
        unspin: DQuat,
        phase: f64,
        axis: DVec3,
    ) -> bool {
        let bits = (
            [axis.x.to_bits(), axis.y.to_bits(), axis.z.to_bits()],
            amplitude_m.to_bits(),
            datum_m.to_bits(),
            radius_m.to_bits(),
        );
        if self.sea.header_key.as_ref() != self.key.as_ref() || self.sea.header_bits != Some(bits) {
            self.sea.entries.clear();
            self.sea.header_key = self.key.clone();
            self.sea.header_bits = Some(bits);
            self.sea.hits = 0;
            self.sea.misses = 0;
            self.sea.rebuilds += 1;
        }
        let tau = 2.0 * core::f64::consts::PI;
        if let Some(p) = self.sea.entries.get(&id)
            && (phase - p.lo).rem_euclid(tau) <= (p.hi - p.lo).rem_euclid(tau)
        {
            self.sea.hits += 1;
            return p.holds;
        }
        let p = sea_probe_eval(field, id, amplitude_m, datum_m, radius_m, unspin, phase);
        if self.sea.entries.len() >= 4096 {
            self.sea.entries.clear();
        }
        self.sea.entries.insert(id, p);
        self.sea.misses += 1;
        p.holds
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
///
/// C2: a pure function of `(field, id, amplitude, datum, radius,
/// unspin)` — every input but the phase is pinned by the heightfield
/// cache key (the body's authored table), and the phase's slow motion
/// is memoized with a PROVEN stability window by
/// [`TerrainCache::sea_probe`] (below); this function stays the
/// verbatim recompute path.
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

// ---------------------------------------------------------------------------
// C2: the sea-cull probe memo.
// ---------------------------------------------------------------------------

/// Window cap (rad): every proven stability window is clamped to this
/// span around the evaluated phase. Generous against per-frame phase
/// motion (Terrin's 6 h day advances ~5 µrad per 64 Hz tick) while
/// keeping the latitude-band bound in `sea_probe_eval` honest.
const SEA_PROBE_WINDOW_CAP: f64 = core::f64::consts::PI / 32.0;

/// `HeightField::texel` mirrored (the csp-core original is private) —
/// the same wrap-u/clamp-v edge rules, so the cell corners
/// `sea_probe_window` reads are the exact corners `sample_dir`
/// interpolated.
fn texel(field: &rt::HeightField, i: i64, j: i64) -> f64 {
    let i = i.rem_euclid(field.width as i64);
    let j = j.clamp(0, field.height as i64 - 1);
    f64::from(field.data[j as usize * field.width as usize + i as usize])
}

/// One memoized probe: the answer plus the phase window it is proven
/// over (C2).
#[derive(Clone, Copy)]
struct SeaProbe {
    /// The memoized `patch_might_hold_sea` answer.
    holds: bool,
    /// Wrapped-phase window lower bound.
    lo: f64,
    /// Wrapped-phase window upper bound.
    hi: f64,
}

/// One sample's stability inputs (the `sea_probe_eval` proof's
/// per-sample terms).
#[derive(Clone, Copy)]
struct SampleBound {
    /// The sample's normalized height (bit-identical to the probe's
    /// own evaluation).
    h: f64,
    /// `|Δh| ≤ rate·δ` over a phase move δ (normalized per radian).
    rate: f64,
    /// Phase radius that keeps the sample inside its bilinear cell.
    cell: f64,
}

/// The sample's stability radius: the phase distance its `margin`
/// (metres, derated) and cell slack prove the height stays on its own
/// side of the threshold `t_m` over.
fn sample_radius(b: &SampleBound, amplitude_m: f64, t_m: f64) -> f64 {
    // Relative derate: the bound's own float error (mapping, cos,
    // products) stays far under 1e-9 of the margin.
    let margin = (b.h * amplitude_m - t_m).abs() * (1.0 - 1.0e-9);
    if !b.rate.is_finite() || margin == 0.0 {
        return 0.0;
    }
    if b.rate > 0.0 {
        (margin / (amplitude_m * b.rate)).min(b.cell)
    } else {
        // Flat cell: constant within it; only the cell walls bound
        // the reuse.
        b.cell
    }
}

/// The PROVEN stability window of one probe evaluation: the wrapped
/// phase interval `[lo, hi]` (span ≤ `2·SEA_PROBE_WINDOW_CAP`) within
/// which the probe's answer cannot change, so the memo may serve it
/// in place of a recompute with bit-identical effect on the packet.
///
/// **The proof.** The answer flips only when the argmin sample's
/// height crosses the cull threshold `t_m = (datum − radius) + margin`
/// (metres; the sample contributes `h·amplitude`): for a TRUE answer
/// the min staying ≤ `t_m` keeps it true (any other sample only
/// matters by dropping lower, which keeps the min lower), and for a
/// FALSE answer every sample must stay > `t_m` since any one dipping
/// flips it. Each of the 25 samples sits at `unspin.rotate(c_i)` — as
/// the phase moves by δ the direction moves along the unit sphere by
/// arc ≤ δ, so its latitude moves ≤ δ and its longitude ≤ δ/cos λ at
/// every latitude the path visits; the pole-ward edge of the band,
/// `cos(|λ| + CAP)`, bounds that factor for any δ ≤ CAP (a band
/// touching a pole yields a zero window). Through the equirect
/// mapping (`u`/`v` mirrored from `sample_dir`) that bounds the index
/// motion, and the in-cell bilinear partials are bounded by the
/// cell's edge differences (`|∂h/∂u| ≤ max(|b−a|, |d−c|)`,
/// `|∂h/∂v| ≤ max(|c−a|, |d−b|)` — each partial is linear in the
/// other coordinate, so its extremes sit at the cell edges). So per
/// sample `|Δh_i| ≤ rate_i·δ`, valid while the sample stays inside
/// the cell the bounds were read from (`cell` enforces exactly
/// that). For TRUE answers δ ≤ the ARGMIN sample's own radius keeps
/// the min on its side; for FALSE answers δ ≤ the minimum radius over
/// all samples keeps every sample on its side. QED.
#[allow(clippy::too_many_arguments)] // the probe's input set + the phase
fn sea_probe_eval(
    field: &rt::HeightField,
    id: rt::PatchId,
    amplitude_m: f64,
    datum_m: f64,
    radius_m: f64,
    unspin: DQuat,
    phase: f64,
) -> SeaProbe {
    // The verbatim answer — the very code the pre-C2 extract ran per
    // leaf per frame (and the differential test's brute-force side).
    let holds = patch_might_hold_sea(field, id, amplitude_m, datum_m, radius_m, unspin);
    let t_m = (datum_m - radius_m) + (amplitude_m * rt::OCEAN_CULL_FRAC).max(250.0);
    let (corner, du, dv) = rt::patch_cube(id);
    let w = f64::from(field.width);
    let h = f64::from(field.height);
    // The per-sample bound terms (the loop re-walks the probe's own
    // 25 directions — a miss-only cost, amortized over the window).
    let mut bounds = [SampleBound {
        h: 0.0,
        rate: f64::INFINITY,
        cell: 0.0,
    }; 25];
    let mut lo = f64::INFINITY;
    let mut argmin = 0usize;
    for j in 0..5 {
        for i in 0..5 {
            let c = corner + du * (i as f64 / 4.0) + dv * (j as f64 / 4.0);
            let dir = unspin.rotate(c / c.norm());
            let sample = field.sample_dir(dir);
            if sample < lo {
                lo = sample;
                argmin = j * 5 + i;
            }
            // The sample's equirect cell, mirroring `sample_dir`'s
            // mapping so the corner reads below are the corners it
            // interpolated. (std `asin`/`atan2` here vs csp-core's
            // libm: any ulp disagreement can only land ON a cell
            // boundary, where the cell slack is ~0 and the window
            // collapses — sound; the margin derate absorbs the same
            // worry on the bound itself.)
            let lat = dir.z.clamp(-1.0, 1.0).asin();
            let lon = dir.y.atan2(dir.x);
            let u = (lon + core::f64::consts::PI) / (2.0 * core::f64::consts::PI) * w - 0.5;
            let v = (lat + core::f64::consts::FRAC_PI_2) / core::f64::consts::PI * h - 0.5;
            let (u0, v0) = (u.floor() as i64, v.floor() as i64);
            let (fu, fv) = (u - u0 as f64, v - v0 as f64);
            let (a, b) = (texel(field, u0, v0), texel(field, u0 + 1, v0));
            let (cc, d) = (texel(field, u0, v0 + 1), texel(field, u0 + 1, v0 + 1));
            let lu = (b - a).abs().max((d - cc).abs());
            let lv = (cc - a).abs().max((d - b).abs());
            let cos_b = (lat.abs() + SEA_PROBE_WINDOW_CAP).cos();
            let (rate, cell_u) = if cos_b > 0.0 {
                (
                    lu * w / (2.0 * core::f64::consts::PI * cos_b) + lv * h / core::f64::consts::PI,
                    fu.min(1.0 - fu) * 2.0 * core::f64::consts::PI * cos_b / w,
                )
            } else {
                // The δ-band touches a pole: longitude rate unbounded.
                (f64::INFINITY, 0.0)
            };
            let cell_v = fv.min(1.0 - fv) * core::f64::consts::PI / h;
            bounds[j * 5 + i] = SampleBound {
                h: sample,
                rate,
                cell: cell_u.min(cell_v),
            };
        }
    }
    debug_assert_eq!(
        holds,
        lo * amplitude_m <= t_m,
        "the eval loop must mirror the verbatim probe"
    );
    let r = if holds {
        // TRUE: the argmin staying ≤ t_m keeps the answer — and it
        // carries the LARGEST below-threshold buffer of the samples.
        sample_radius(&bounds[argmin], amplitude_m, t_m)
    } else {
        // FALSE: any sample dipping flips it — every sample stays.
        bounds
            .iter()
            .map(|b| sample_radius(b, amplitude_m, t_m))
            .fold(SEA_PROBE_WINDOW_CAP, f64::min)
    }
    .min(SEA_PROBE_WINDOW_CAP);
    let tau = 2.0 * core::f64::consts::PI;
    SeaProbe {
        holds,
        lo: (phase - r).rem_euclid(tau),
        hi: (phase + r).rem_euclid(tau),
    }
}

/// The sea-cull probe table (C2): per `(heightfield cache key,
/// PatchId)` with proven phase windows, so a spinning body's
/// slow-moving probes are answered from the table instead of 25
/// heightfield samples per leaf per frame. Rebuilt when the
/// heightfield, spin axis or the body's amplitude/datum/radius rows
/// change; bounded at 4096 entries (a surface-locked camera's
/// co-rotating leaf set sweeps the whole sphere over a body day — the
/// cap keeps a long flight bounded; a clear re-computes, never
/// mis-answers).
#[derive(Default)]
struct SeaProbeMemo {
    /// Heightfield cache key the table was built under.
    header_key: Option<CacheKey>,
    /// Header bits — spin axis `xyz`, amplitude, datum, radius.
    header_bits: Option<([u64; 3], u64, u64, u64)>,
    /// The memoized probes.
    entries: BTreeMap<rt::PatchId, SeaProbe>,
    /// Lookups served from the table since the last rebuild (test
    /// observability).
    hits: u64,
    /// Recomputations since the last rebuild.
    misses: u64,
    /// Rebuilds (heightfield/parameter changes).
    rebuilds: u32,
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
/// While the background bake is in flight this returns the bare datum
/// `radius_m` — the honest floor for a frame that draws no terrain
/// (see [`TerrainCache::field`]).
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
    // While the bake is in flight there is no surface to measure — the
    // datum radius IS the ground, exactly the number the removed zeros
    // stand-in used to sample (its every texel was 0). No displaced
    // terrain draws in those frames, so nothing can poke through a
    // datum-level anchor; the next frame with a real field measures
    // real relief again.
    let Some(field) = cache.field(body_id, body.seed, body.profile) else {
        return env.radius_m;
    };
    // The inertial→fixed half of `crate::structures::spin_rotation` —
    // the same `R(axis, −phase)` conjugate the shader applies.
    let fixed = crate::structures::spin_rotation(env)
        .conjugate()
        .rotate(dir);
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
///
/// **Arrival contract (M5 §5.2-R2 arrival polish):** while the real
/// heightfield for the dominant body is still baking on the background
/// thread, the packet carries `heightfield: None` and NO terrain or
/// ocean instances — the renderer draws the sky pass, the craft and
/// the launch complex over a haze/space backdrop, never a flat datum
/// sphere standing in for mountains. The shadow blob is disarmed
/// (`shadow.w = 0`; it has no displaced surface to land on) and the
/// HUD mirrors the wait ("generating terrain"). The first packet with
/// a field swaps the full displaced surface in (the field's content
/// key forces the height-texture re-upload).
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
    let field = cache.field(body_id, body.seed, body.profile);

    // The §2.6.4 spin phase — the same `R(axis, ω·t)` `bubble.rs`
    // co-rotates latched poses by. The heightfield/biome data is
    // body-fixed while patch directions and positions are inertial, so
    // lookups run through the conjugate rotation `unspin`; the shader
    // does the same inside `vs_terrain`. `spin_rotation` wraps the
    // phase to [0, 2π) so long-running clocks don't lose resolution —
    // `conjugate` of `R(axis, φ)` IS `R(axis, −φ)` for a unit quat.
    let unspin = crate::structures::spin_rotation(env).conjugate();
    let spin_phase = crate::structures::wrapped_spin_phase(env);

    let eye_rel = cam.eye - env.body_center;
    // `amplitude_m` rides the horizon cull — displaced relief up to
    // `radius + amplitude` stays visible past the bare-datum limb.
    // While the bake is pending there is nothing to select FOR: the
    // instance lists stay empty (the arrival contract above — no
    // displaced draw, hence no flat stand-in either).
    let mut near_terrain = Vec::new();
    let mut far_terrain = Vec::new();
    let mut near_ocean = Vec::new();
    let mut far_ocean = Vec::new();
    if let Some(field) = &field {
        let leaves = rt::select_patches(eye_rel, env.radius_m, body.amplitude_m, &cfg.lod);
        // §2.3's depth partition: a patch routes FAR only when its
        // nearest point ALONG THE VIEW AXIS still clears the split —
        // `dot(patch_origin − eye, fwd) − patch_extent ≥ FAR_SPLIT`.
        // The forward (view-depth) coordinate matters, not the radial
        // distance: the far pass's clip test `split ≤ clip.w` runs on
        // `w = d·cos(off-axis)`, which at the frame's side reaches is
        // up to ~25 % shorter than the radial distance — a
        // radial-distance rule clipped side-of-frame quads out of
        // both passes (the second sky-through-planet hole; forward is
        // 1-Lipschitz, so `dot − extent` lower-bounds every vertex's
        // clip.w). Straddlers — radial OR angular — route NEAR and
        // draw whole, exactly the pre-split behaviour for them; the
        // far pass's clip test stays only as the staleness safety
        // net: coverage never depends on it. The bound's cost: a
        // near-routed straddler's beyond-split fringe rides the near
        // pass's unconditional-over policy (the same documented
        // approximation as the craft case), confined to the grazing
        // band at the split ring.
        // `fwd` recovered from the projection itself: (P·V)'s row 3
        // is `[0,0,−1,0]·V = −z_out` — the camera's forward.
        let fwd = {
            let r3 = cam.view_proj[3];
            let n = (r3[0] * r3[0] + r3[1] * r3[1] + r3[2] * r3[2]).sqrt();
            if n > 1e-9 {
                DVec3::new(r3[0] / n, r3[1] / n, r3[2] / n)
            } else {
                DVec3::ZERO
            }
        };
        let split = csp_render::flight::FAR_SPLIT_M;
        for &id in &leaves {
            let extent = rt::patch_extent_m(id, env.radius_m, body.amplitude_m);
            let forward_of = |inst: &rt::TerrainInstance| {
                let o = inst.origin_rel_eye;
                f64::from(o[0]) * fwd.x + f64::from(o[1]) * fwd.y + f64::from(o[2]) * fwd.z
            };
            let inst = rt::patch_instance(id, 0.0, eye_rel, env.radius_m);
            if forward_of(&inst) - extent >= split {
                far_terrain.push(inst);
            } else {
                near_terrain.push(inst);
            }
            if let Some(datum) = body.ocean_datum_m
                && cache.sea_probe(
                    field,
                    id,
                    body.amplitude_m,
                    datum,
                    env.radius_m,
                    unspin,
                    spin_phase,
                    env.spin_axis,
                )
            {
                // Ocean quads sit flat at the datum; the extent bound
                // still applies through the terrain row's amplitude
                // term (conservative, never under).
                let extent = rt::patch_extent_m(id, datum, body.amplitude_m);
                let inst = rt::patch_instance(id, 1.0, eye_rel, datum);
                if forward_of(&inst) - extent >= split {
                    far_ocean.push(inst);
                } else {
                    near_ocean.push(inst);
                }
            }
        }
    }
    // The packet's near-then-far layout the renderer slices: the
    // `*_far_start` boundary counts index the first FAR instance of
    // each list.
    let terrain_far_start = near_terrain.len() as u32;
    let ocean_far_start = near_ocean.len() as u32;
    let mut terrain = near_terrain;
    terrain.append(&mut far_terrain);
    let mut ocean = near_ocean;
    ocean.append(&mut far_ocean);

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

    // Pending bake ⇒ no displaced surface ⇒ the blob has nowhere to
    // land: disarm it (the `w = 0` row is `blob_shadow`'s own off
    // switch) and zero `craft_alt` — that uniform only feeds the
    // blob's altitude fade, which the disarm already killed.
    let (shadow, craft_alt) = match field.as_ref() {
        Some(field) => vessel_shadow(
            &view_rot,
            cam,
            env,
            field,
            &unspin,
            body.amplitude_m,
            sun_dir,
        ),
        None => ([0.0; 4], 0.0),
    };

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
    let uniforms = rt::TerrainUniforms {
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
    };
    // §2.3's far-field projection — the "cheaper math" the PRD's
    // second pass allows: `(P·V)`'s clip.z row is exactly `n·V[3]`
    // (the reverse-Z projection zeroes every other z-row term, and
    // the camera-relative view's bottom row is `[0,0,0,1]`), i.e.
    // `[0,0,0,n]` with `n = FLIGHT_NEAR_M`. The far frustum is the
    // SAME matrix with that one row set to `[0,0,0,FAR_SPLIT_M]` —
    // xy columns and the w row are bit-identical, so far geometry
    // keeps its screen position and only depth remaps
    // (`z = split/d`; NDC z at 10⁷ m moves from 10⁻⁸ to 0.1).
    let mut far_vp = cam.view_proj;
    far_vp[2] = [0.0, 0.0, 0.0, csp_render::flight::FAR_SPLIT_M];
    let mut uniforms_far = uniforms;
    uniforms_far.view_proj = m4_wgsl(&far_vp);
    rt::TerrainPacket {
        uniforms,
        uniforms_far,
        heightfield: field,
        biome: body.biome,
        atmo,
        terrain,
        terrain_far_start,
        ocean,
        ocean_far_start,
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
    use crate::flight::FlightEnv;
    use crate::map::{MapCameraF64, mat4_mul, perspective_revz, project_into};
    // The legacy bake height — only the tests assert the dims pair.
    use csp_core::terrain::BAKE_HEIGHT as HEIGHTFIELD_H;

    /// A minimal `LoadedBody` — only the fields `config_from_bodies`
    /// reads carry authored values; the rest is inert filler.
    fn body_def(id: &str, radius_m: f64, terrain: csp_data::TerrainDef) -> csp_data::LoadedBody {
        csp_data::LoadedBody {
            def: csp_data::BodyDef {
                schema_version: csp_data::BODY_SCHEMA_VERSION,
                id: csp_data::BodyId::from(id),
                display_name: csp_data::LocKey::from("x"),
                parent: None,
                radius_m,
                mu_m3_per_s2: 1.0,
                soi_m: None,
                rotation_period_s: Some(1.0),
                tilt_rad: 0.0,
                orbit: None,
                atmosphere: None,
                ocean: None,
                terrain,
                biome_map: None,
                surface_resources: Vec::new(),
                insolation_w_m2: None,
            },
            biomes: None,
        }
    }

    /// M5 §5.2-R2: the authored `TerrainDef::amplitude_frac`/`.relief`
    /// map into the presentation `TerrainBody` — amplitude is
    /// `radius · amplitude_frac` (no longer the fixed
    /// `AMPLITUDE_FRAC` share) and the profile rides along to the
    /// cache/bake.
    #[test]
    fn config_from_bodies_maps_authored_terrain_fields() {
        let authored = csp_data::TerrainDef {
            noise_seed: 601,
            heightmap: None,
            amplitude_frac: 0.006,
            relief: ReliefProfile::Mountain,
        };
        let legacy = csp_data::TerrainDef {
            noise_seed: 7,
            heightmap: None,
            amplitude_frac: AMPLITUDE_FRAC,
            relief: ReliefProfile::Legacy,
        };
        let cfg = config_from_bodies(&[
            body_def("terrin", 600_000.0, authored),
            body_def("mun", 200_000.0, legacy),
        ]);
        let terrin = cfg.bodies.get("terrin").cloned().unwrap();
        assert_eq!(terrin.seed, 601);
        assert!(
            (terrin.amplitude_m - 3_600.0).abs() < 1e-9,
            "authored 0.006 × 600 km ⇒ ~3600 m, got {}",
            terrin.amplitude_m
        );
        assert_eq!(terrin.profile, ReliefProfile::Mountain);
        let mun = cfg.bodies.get("mun").cloned().unwrap();
        assert_eq!(mun.amplitude_m, 200_000.0 * AMPLITUDE_FRAC);
        assert_eq!(mun.profile, ReliefProfile::Legacy);
    }

    /// M5 §5.2-R2: the cache key folds the profile, so a Legacy and a
    /// Mountain field for the same `(body, seed)` are distinct bakes —
    /// different dims, different content keys (the
    /// `bake_profiled_key` contract) — and a return visit swaps the
    /// completed Legacy field back in from `done`.
    #[test]
    fn cache_separates_profiles_at_same_seed() {
        // `field` serves `None` while the background bake is in flight
        // (no stand-in since the arrival polish) — poll until it lands.
        let real = |cache: &mut TerrainCache, profile: ReliefProfile| -> Arc<rt::HeightField> {
            for _ in 0..4_000 {
                if let Some(f) = cache.field("terrin", 601, profile) {
                    return f;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            panic!("background bake never landed for {profile:?}");
        };
        let mut cache = TerrainCache::default();
        let legacy = real(&mut cache, ReliefProfile::Legacy);
        let mountain = real(&mut cache, ReliefProfile::Mountain);
        assert_eq!(
            (legacy.width, legacy.height),
            (HEIGHTFIELD_W, HEIGHTFIELD_H)
        );
        assert_eq!(
            (mountain.width, mountain.height),
            ReliefProfile::Mountain.bake_dims()
        );
        assert_ne!(legacy.key, mountain.key);
        assert_eq!(
            legacy.key,
            rt::HeightField::bake_profiled_key(601, ReliefProfile::Legacy)
        );
        assert_eq!(
            mountain.key,
            rt::HeightField::bake_profiled_key(601, ReliefProfile::Mountain)
        );
        // The spectra are different functions — the texels differ.
        assert_ne!(legacy.data, mountain.data);
        // A return visit reuses the completed field (same bake key).
        let back = real(&mut cache, ReliefProfile::Legacy);
        assert_eq!(back.key, legacy.key);
    }

    /// The shared scene for the arrival-contract tests: Terrin's
    /// authored Mountain body (seed 601), an environment centred at
    /// the origin, and a solved camera 15 km above the north pole —
    /// any eye outside the body selects quadtree leaves, and the pole
    /// keeps the direction axis-aligned.
    fn arrival_scene() -> (TerrainConfig, FlightEnv, MapCameraF64) {
        let cfg = config_from_bodies(&[body_def(
            "terrin",
            600_000.0,
            csp_data::TerrainDef {
                noise_seed: 601,
                heightmap: None,
                amplitude_frac: 0.006,
                relief: ReliefProfile::Mountain,
            },
        )]);
        let env = FlightEnv {
            body_center: DVec3::ZERO,
            radius_m: 600_000.0,
            atm_top_m: 0.0,
            atm_scale_height_m: 0.0,
            mu_m3_per_s2: 1.0,
            body: Some(csp_data::BodyId::from("terrin")),
            sun_dir: DVec3::new(1.0, 0.0, 0.0),
            spin_axis: DVec3::Z,
            spin_phase: 0.0,
            body_spin: DVec3::ZERO,
        };
        let cam = MapCameraF64 {
            view_proj: [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
            eye: DVec3::new(0.0, 0.0, 615_000.0),
            distance_m: 15_000.0,
            valid: true,
        };
        (cfg, env, cam)
    }

    /// M5 §5.2-R2 arrival contract, first half: the frames immediately
    /// after a body becomes dominant (fresh cache, bake in flight)
    /// must NEVER carry a flat-datum heightfield. The wait presents as
    /// a SKY-ONLY packet — `heightfield: None`, zero terrain/ocean
    /// instances, disarmed shadow blob — and the only field a packet
    /// ever carries is the real bake (its authored key, bit 0 clear,
    /// non-zero relief). The zeros stand-in this replaces drew a flat
    /// planet for exactly these first seconds.
    #[test]
    fn first_frames_never_draw_the_flat_datum() {
        let (cfg, env, cam) = arrival_scene();
        let mut cache = TerrainCache::default();
        let real_key = rt::HeightField::bake_profiled_key(601, ReliefProfile::Mountain);
        // The loop must actually OBSERVE the wait state — otherwise
        // bake-timing shrinkage would quietly hollow the test into
        // "only ever saw the real field".
        let mut saw_pending = false;
        for _ in 0..64 {
            let p = build_packet(&cfg, &mut cache, &env, &cam, (320.0, 240.0), 0.0);
            match &p.heightfield {
                // The wait state: sky (and the craft, drawn by the
                // flight stream) only. A terrain or ocean instance
                // here would draw displaced geometry against nothing.
                None => {
                    saw_pending = true;
                    assert!(p.sky, "the sky backdrop stays while the bake runs");
                    assert!(
                        p.terrain.is_empty() && p.ocean.is_empty(),
                        "a pending packet must draw no terrain/ocean"
                    );
                    assert_eq!(
                        p.uniforms.shadow, [0.0; 4],
                        "the blob has no surface to land on while pending"
                    );
                }
                // If the bake already landed mid-loop, the field must
                // be the REAL one — never zeros under a stand-in key.
                Some(h) => {
                    assert_eq!(h.key, real_key, "only the authored field may be carried");
                    assert_eq!(h.key & 1, 0, "real bake keys keep bit 0 clear");
                    assert!(
                        h.data.iter().any(|&x| x != 0.0),
                        "a carried field must be real relief, not a zeros stand-in"
                    );
                }
            }
        }
        assert!(
            saw_pending,
            "the first frames never observed the bake wait — the test went \
             vacuous (the bake landed inside one packet build?)"
        );
    }

    /// M5 §5.2-R2 arrival contract, second half: once the background
    /// bake lands, displaced draws RESUME — the packet carries the real
    /// field's key with a non-empty terrain instance list (an eye
    /// 15 km up always selects leaves), and the cache keeps serving
    /// that field afterwards (no flap back to pending for a finished
    /// key).
    #[test]
    fn terrain_draws_resume_when_the_bake_lands() {
        let (cfg, env, cam) = arrival_scene();
        let mut cache = TerrainCache::default();
        let real_key = rt::HeightField::bake_profiled_key(601, ReliefProfile::Mountain);
        let mut landed = false;
        for _ in 0..4_000 {
            let p = build_packet(&cfg, &mut cache, &env, &cam, (320.0, 240.0), 0.0);
            if let Some(h) = &p.heightfield {
                assert_eq!(h.key, real_key, "the landed field is the authored bake");
                assert_eq!(h.key & 1, 0);
                assert!(
                    !p.terrain.is_empty(),
                    "an eye above the surface must select terrain leaves again"
                );
                let next = build_packet(&cfg, &mut cache, &env, &cam, (320.0, 240.0), 0.0);
                assert_eq!(
                    next.heightfield.as_ref().map(|h| h.key),
                    Some(real_key),
                    "a finished key must keep serving its field"
                );
                landed = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(landed, "the background bake never landed — stalled?");
    }

    /// The packet's `planet.w` is the body's AUTHORED amplitude — the
    /// same `TerrainBody::amplitude_m` every amplitude-driven consumer
    /// (horizon cull, blob shadow, ocean margin, camera floor) reads.
    #[test]
    fn packet_uniforms_carry_authored_amplitude() {
        let (cfg, env, _cam) = arrival_scene();
        let packet = build_packet(
            &cfg,
            &mut TerrainCache::default(),
            &env,
            &MapCameraF64::default(),
            (320.0, 240.0),
            0.0,
        );
        assert_eq!(packet.uniforms.planet[0], 600_000.0);
        assert!(
            (f64::from(packet.uniforms.planet[3]) - 3_600.0).abs() < 1e-3,
            "planet.w must be the authored 600 km × 0.006 = 3600 m, got {}",
            packet.uniforms.planet[3]
        );
    }

    /// §2.3's far-field algebra, pinned at the source: a real chase
    /// projection's clip.z row is EXACTLY `[0,0,0,FLIGHT_NEAR_M]`
    /// (positions are camera-relative, so `(P·V)`'s z row is `n·V[3]`
    /// with `V[3] = [0,0,0,1]`), and `build_packet`'s far uniforms
    /// differ from the near row in that row ONLY — set to
    /// `[0,0,0,FAR_SPLIT_M]` — leaving every other matrix cell and
    /// every other uniform field bit-equal. That one-row swap is what
    /// keeps far geometry on its screen pixels while remapping depth
    /// to `z = split/d`.
    #[test]
    fn far_uniforms_are_exactly_the_clip_z_row_swap() {
        let (cfg, env, _cam) = arrival_scene();
        // A real chase-shaped camera (tilted basis + reverse-Z near
        // 0.1), not arrival_scene's identity `view_proj`.
        let (_r, _u, _z, view) = tilted_view();
        let proj = perspective_revz(
            crate::flight::FLIGHT_FOV_Y,
            320.0 / 240.0,
            crate::flight::FLIGHT_NEAR_M,
        );
        let vp = mat4_mul(&proj, &view);
        assert_eq!(
            vp[2],
            [0.0, 0.0, 0.0, crate::flight::FLIGHT_NEAR_M],
            "the algebra's premise: (P·V) row 2 is [0,0,0,near]"
        );
        let cam = MapCameraF64 {
            view_proj: vp,
            eye: DVec3::new(0.0, 0.0, 615_000.0),
            distance_m: 15_000.0,
            valid: true,
        };
        let packet = build_packet(
            &cfg,
            &mut TerrainCache::default(),
            &env,
            &cam,
            (320.0, 240.0),
            0.0,
        );
        // Column-major: uniform.view_proj[col][row] — the swapped row
        // is index 2 along the second axis.
        for c in 0..4 {
            for r in 0..4 {
                if r == 2 {
                    continue;
                }
                assert_eq!(
                    packet.uniforms_far.view_proj[c][r], packet.uniforms.view_proj[c][r],
                    "far uniforms must not touch cell [{}][{}]",
                    c, r
                );
            }
        }
        let split = csp_render::flight::FAR_SPLIT_M as f32;
        for c in 0..4 {
            let want = if c == 3 { split } else { 0.0 };
            assert_eq!(
                packet.uniforms_far.view_proj[c][2], want,
                "far clip.z row must be [0,0,0,split] at column {c}"
            );
        }
        // Every uniform outside view_proj is shared verbatim — the far
        // row is a FULL clone with one row swapped. (A far row built
        // from `Default::default()` zeroed `planet.x` and collapsed
        // the far pass's displacement to nothing — the load-bearing
        // half of this contract.)
        let u = &packet.uniforms;
        let f = &packet.uniforms_far;
        assert_eq!(f.view_rot, u.view_rot);
        assert_eq!(f.eye_rel_body, u.eye_rel_body);
        assert_eq!(f.sun_dir, u.sun_dir);
        assert_eq!(
            f.planet, u.planet,
            "planet.x is the body radius — zero collapses the far displacement"
        );
        assert_eq!(f.atmo, u.atmo);
        assert_eq!(f.misc, u.misc);
        assert_eq!(f.spin, u.spin);
        assert_eq!(f.rayleigh, u.rayleigh);
        assert_eq!(f.biome_colors, u.biome_colors);
        assert_eq!(f.shadow, u.shadow);
        assert_eq!(f.art, u.art);
        assert_eq!(f.class_means, u.class_means);
    }

    /// §2.3's instance routing: `build_packet` lays instances out
    /// near-then-far with the `*_far_start` boundary, and a patch
    /// routes far only when its nearest point ALONG THE VIEW AXIS
    /// (`dot(centre − eye, fwd) − patch_extent`) clears the split —
    /// the earlier centre-only and radial-nearest rules both left
    /// straddlers clipped by the far pass and drawn by no pass (sky
    /// through the planet). A 15 km pole vantage routes everything
    /// near; a 2 Mm vantage routes everything far; an 800 km vantage
    /// straddles the split ring and MUST keep every straddler whole
    /// in the near pass — the blocker's regression pin.
    #[test]
    #[allow(clippy::too_many_lines)] // one rule pinned across three vantages; splitting dilutes the invariants
    fn far_field_routing_partitions_instances() {
        let (cfg, env, _cam) = arrival_scene();
        let split = csp_render::flight::FAR_SPLIT_M;
        // A real chase-shaped projection (the routing reads the camera
        // forward out of `view_proj` row 3, which the identity matrix
        // does not carry).
        let (_r, _u, _z, view) = tilted_view();
        let vp = mat4_mul(
            &perspective_revz(
                crate::flight::FLIGHT_FOV_Y,
                320.0 / 240.0,
                crate::flight::FLIGHT_NEAR_M,
            ),
            &view,
        );
        let fwd = {
            let r3 = vp[3];
            let v = DVec3::new(r3[0], r3[1], r3[2]);
            let n = v.norm();
            if n > 1e-9 {
                DVec3::new(v.x / n, v.y / n, v.z / n)
            } else {
                DVec3::ZERO
            }
        };
        // The routing rule, reconstructed from each instance's own
        // rows: extent = |du+dv|/2·R + amplitude (the exact body of
        // `rt::patch_extent_m`; `du`/`dv` are f32 casts of exact
        // dyadic cube spans, so the f64 reconstruction agrees).
        let amp = 600_000.0 * 0.006; // Terrin's authored amplitude
        let dist2 = |i: &rt::TerrainInstance| {
            let o = i.origin_rel_eye;
            f64::from(o[0]) * f64::from(o[0])
                + f64::from(o[1]) * f64::from(o[1])
                + f64::from(o[2]) * f64::from(o[2])
        };
        let extent_of = |i: &rt::TerrainInstance| {
            let d = |v: [f32; 4]| DVec3::new(f64::from(v[0]), f64::from(v[1]), f64::from(v[2]));
            (d(i.du) + d(i.dv)).norm() * 0.5 * env.radius_m + amp
        };
        let is_far = |i: &rt::TerrainInstance| {
            let o = i.origin_rel_eye;
            f64::from(o[0]) * fwd.x + f64::from(o[1]) * fwd.y + f64::from(o[2]) * fwd.z
                - extent_of(i)
                >= split
        };
        let check = |p: &rt::TerrainPacket| {
            for (i, inst) in p.terrain.iter().enumerate() {
                let near = (i as u32) < p.terrain_far_start;
                assert_eq!(
                    near,
                    !is_far(inst),
                    "terrain instance {i} routed {} at distance² {:.3e}, extent {:.0} m",
                    if near { "near" } else { "far" },
                    dist2(inst),
                    extent_of(inst)
                );
            }
            for (i, inst) in p.ocean.iter().enumerate() {
                let near = (i as u32) < p.ocean_far_start;
                assert_eq!(
                    near,
                    !is_far(inst),
                    "ocean instance {i} routed {} at distance² {:.3e}, extent {:.0} m",
                    if near { "near" } else { "far" },
                    dist2(inst),
                    extent_of(inst)
                );
            }
        };
        // The bake is served by the background thread — poll until the
        // packet carries instances (the arrival-contract pattern).
        let mut cache = TerrainCache::default();
        let near_cam = MapCameraF64 {
            view_proj: vp,
            eye: DVec3::new(0.0, 0.0, 615_000.0),
            distance_m: 15_000.0,
            valid: true,
        };
        let mut near_case = None;
        for _ in 0..4_000 {
            let p = build_packet(&cfg, &mut cache, &env, &near_cam, (320.0, 240.0), 0.0);
            if !p.terrain.is_empty() {
                near_case = Some(p);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let near_case = match near_case {
            Some(p) => p,
            None => panic!("the bake never landed for the routing test"),
        };
        check(&near_case);
        assert_eq!(
            near_case.terrain_far_start as usize,
            near_case.terrain.len(),
            "a 15 km vantage routes every patch near (horizon ≪ 1 Mm)"
        );
        // The blocker's vantage: 800 km above the pole — the split
        // ring crosses the disc (terrain spans ~800 km..1.24 Mm eye
        // distance), so straddling patches EXIST. Every one of them
        // must route near (whole quad, near depth semantics); only
        // patches whose forward-nearest point clears the split may
        // route far. Both bands non-empty proves the case is live.
        let straddle_cam = MapCameraF64 {
            view_proj: vp,
            eye: DVec3::new(0.0, 0.0, 1_400_000.0),
            distance_m: 800_000.0,
            valid: true,
        };
        let straddle_case =
            build_packet(&cfg, &mut cache, &env, &straddle_cam, (320.0, 240.0), 0.0);
        check(&straddle_case);
        assert!(
            !straddle_case.terrain.is_empty(),
            "an 800 km vantage selects leaves"
        );
        let near_n = straddle_case.terrain_far_start;
        let far_n = (straddle_case.terrain.len() - near_n as usize) as u32;
        assert!(near_n > 0, "the 800 km disc's near band is non-empty");
        assert!(far_n > 0, "the 800 km disc's far band is non-empty");
        // Liveness: straddlers — radial centre beyond the split, but
        // nearest-forward point inside — EXIST at this vantage and sit
        // in the NEAR list (the centre-only rule shipped them to the
        // far pass, whose clip trimmed their fringes into the
        // sky-through-planet hole).
        let n_straddlers_near = straddle_case.terrain[..near_n as usize]
            .iter()
            .filter(|i| dist2(i) >= split * split)
            .count();
        assert!(
            n_straddlers_near > 0,
            "the 800 km vantage must exercise straddlers (centre ≥ split routed near)"
        );
        // And the far list's forward-nearest bound genuinely clears
        // the split (the far pass's clip test measures exactly this).
        for i in &straddle_case.terrain[straddle_case.terrain_far_start as usize..] {
            let o = i.origin_rel_eye;
            let nearest_fwd =
                f64::from(o[0]) * fwd.x + f64::from(o[1]) * fwd.y + f64::from(o[2]) * fwd.z
                    - extent_of(i);
            assert!(
                nearest_fwd >= split,
                "far-routed patch forward-nearest point {:.0} m < split",
                nearest_fwd
            );
        }
        // The far vantage: 2 Mm above the pole — the nearest terrain
        // is ~2 Mm, wholly past the split even with the extent bound.
        let far_cam = MapCameraF64 {
            view_proj: vp,
            eye: DVec3::new(0.0, 0.0, 2_600_000.0),
            distance_m: 2_000_000.0,
            valid: true,
        };
        let far_case = build_packet(&cfg, &mut cache, &env, &far_cam, (320.0, 240.0), 0.0);
        check(&far_case);
        assert!(
            !far_case.terrain.is_empty(),
            "a 2 Mm vantage still selects leaves (the body's disc)"
        );
        assert_eq!(
            far_case.terrain_far_start, 0,
            "every patch at a 2 Mm vantage routes far"
        );
    }

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

    // ==================== C2: the sea-cull probe memo ================

    /// The C2 test scene: Terrin's seed with an ocean at the datum
    /// radius and a spinning Z-axis day, at the Legacy bake dims (the
    /// faster field).
    fn sea_scene() -> (TerrainConfig, FlightEnv) {
        let mut bodies = BTreeMap::new();
        bodies.insert(
            "terrin".to_owned(),
            TerrainBody {
                seed: 601,
                amplitude_m: 3_600.0,
                profile: ReliefProfile::Legacy,
                ocean_datum_m: Some(600_000.0),
                biome: None,
            },
        );
        let env = FlightEnv {
            body_center: DVec3::ZERO,
            radius_m: 600_000.0,
            atm_top_m: 0.0,
            atm_scale_height_m: 0.0,
            mu_m3_per_s2: 1.0,
            body: Some(csp_data::BodyId::from("terrin")),
            sun_dir: DVec3::X,
            spin_axis: DVec3::Z,
            spin_phase: 0.0,
            body_spin: DVec3::ZERO,
        };
        (
            TerrainConfig {
                bodies,
                ..Default::default()
            },
            env,
        )
    }

    /// Pump the background bake for `body` until the field lands (the
    /// arrival-contract pattern).
    fn baked_field(cache: &mut TerrainCache, body: &str, seed: u64) -> Arc<rt::HeightField> {
        for _ in 0..4_000 {
            if let Some(f) = cache.field(body, seed, ReliefProfile::Legacy) {
                return f;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("sea-probe test: the {body} bake never landed");
    }

    /// C2's differential proof: across a phase schedule that exercises
    /// both memo HITS (sub-window steps, the live-flight pattern) and
    /// MISSES (window-crossing jumps), the memoized probe must equal
    /// the verbatim brute-force evaluation at every step — and the
    /// stored windows must not overreach (brute checks at their
    /// interior, including just inside both ends).
    #[test]
    #[allow(clippy::too_many_lines)] // one differential sweep + the window-edge audit — splitting dilutes the invariant
    fn sea_probe_memo_matches_bruteforce_and_windows_hold() {
        let (_cfg, mut env) = sea_scene();
        let mut cache = TerrainCache::default();
        let field = baked_field(&mut cache, "terrin", 601);
        // Leaves from three vantages — pole plus two tilted equatorial
        // eyes — so the probe set spans shore and open ocean.
        let mut leaves: Vec<rt::PatchId> = Vec::new();
        for eye in [
            DVec3::new(0.0, 0.0, 615_000.0),
            DVec3::new(604_000.0, 0.0, 60_000.0),
            DVec3::new(300_000.0, 300_000.0, 300_000.0),
        ] {
            leaves.extend(rt::select_patches(
                eye,
                600_000.0,
                3_600.0,
                &rt::LodConfig::default(),
            ));
        }
        leaves.sort_unstable();
        leaves.dedup();
        assert!(
            leaves.len() > 50,
            "the vantages must select a real leaf set (got {})",
            leaves.len()
        );
        let tau = 2.0 * core::f64::consts::PI;
        // Fine steps (the ~5 µrad/frame live pattern), then jumps that
        // cross windows, then fine steps again — inside the new windows.
        let mut phases = Vec::new();
        let mut p = 0.3_f64;
        for _ in 0..60 {
            phases.push(p);
            p += 1.0e-5;
        }
        for _ in 0..8 {
            p += 0.02;
            phases.push(p);
        }
        for _ in 0..30 {
            phases.push(p);
            p += 1.0e-5;
        }
        let mut saw_hold = false;
        let mut saw_drop = false;
        for &phi in &phases {
            env.spin_phase = phi;
            let unspin = crate::structures::spin_rotation(&env).conjugate();
            let wrapped = crate::structures::wrapped_spin_phase(&env);
            for &id in &leaves {
                let memo = cache.sea_probe(
                    &field,
                    id,
                    3_600.0,
                    600_000.0,
                    600_000.0,
                    unspin,
                    wrapped,
                    env.spin_axis,
                );
                let brute = patch_might_hold_sea(&field, id, 3_600.0, 600_000.0, 600_000.0, unspin);
                assert_eq!(
                    memo, brute,
                    "memo diverged from brute force at phase {phi:.8} for {id:?}"
                );
                saw_hold |= memo;
                saw_drop |= !memo;
            }
        }
        assert!(
            saw_hold && saw_drop,
            "the leaf set must straddle a shoreline (both answers live)"
        );
        assert!(
            cache.sea.hits > 0 && cache.sea.misses > 0,
            "the schedule must exercise hits (got {}) and misses (got {})",
            cache.sea.hits,
            cache.sea.misses
        );
        assert!(
            cache.sea.hits > cache.sea.misses,
            "the memo must actually carry the load (hits {}, misses {})",
            cache.sea.hits,
            cache.sea.misses
        );
        // Window soundness: brute checks inside the stored windows —
        // near both ends and the middle. An overreaching window fails
        // HERE, not in a screenshot.
        let mut checked = 0;
        for (id, e) in cache.sea.entries.iter().take(48) {
            let span = (e.hi - e.lo).rem_euclid(tau);
            if span <= 0.0 {
                continue;
            }
            for frac in [0.02_f64, 0.5, 0.98] {
                let phi = (e.lo + span * frac).rem_euclid(tau);
                env.spin_phase = phi;
                let unspin = crate::structures::spin_rotation(&env).conjugate();
                let brute =
                    patch_might_hold_sea(&field, *id, 3_600.0, 600_000.0, 600_000.0, unspin);
                assert_eq!(
                    brute, e.holds,
                    "window [{:.6}, {:.6}] (span {span:.3e}) overreaches for {id:?}",
                    e.lo, e.hi
                );
                checked += 1;
            }
        }
        assert!(checked > 0, "at least one window must be non-degenerate");
    }

    /// C2 invalidation: a heightfield change (new body key) or a
    /// header change (spin axis, datum) rebuilds the table — the
    /// probes re-run instead of serving stale shoreline answers.
    #[test]
    fn sea_probe_table_rebuilds_when_inputs_change() {
        let (_cfg, mut env) = sea_scene();
        let mut cache = TerrainCache::default();
        let field = baked_field(&mut cache, "terrin", 601);
        let id = rt::PatchId {
            face: 2,
            level: 6,
            x: 17,
            y: 40,
        };
        let unspin = crate::structures::spin_rotation(&env).conjugate();
        let wrapped = crate::structures::wrapped_spin_phase(&env);
        cache.sea_probe(
            &field,
            id,
            3_600.0,
            600_000.0,
            600_000.0,
            unspin,
            wrapped,
            env.spin_axis,
        );
        assert_eq!(cache.sea.rebuilds, 1, "the first probe builds the table");
        assert_eq!(cache.sea.misses, 1);
        // Same inputs: a hit, no rebuild.
        cache.sea_probe(
            &field,
            id,
            3_600.0,
            600_000.0,
            600_000.0,
            unspin,
            wrapped,
            env.spin_axis,
        );
        assert_eq!(cache.sea.rebuilds, 1);
        assert_eq!(cache.sea.hits, 1);
        // A different spin axis rebuilds (the header bits).
        env.spin_axis = DVec3::X;
        let unspin = crate::structures::spin_rotation(&env).conjugate();
        cache.sea_probe(
            &field,
            id,
            3_600.0,
            600_000.0,
            600_000.0,
            unspin,
            wrapped,
            env.spin_axis,
        );
        assert_eq!(cache.sea.rebuilds, 2, "an axis change must rebuild");
        assert_eq!(
            cache.sea.misses, 1,
            "the miss counter resets with the rebuild"
        );
        // A different ocean datum rebuilds.
        cache.sea_probe(
            &field,
            id,
            3_600.0,
            605_000.0,
            600_000.0,
            unspin,
            wrapped,
            env.spin_axis,
        );
        assert_eq!(cache.sea.rebuilds, 3, "a datum change must rebuild");
        // A different heightfield (second body) rebuilds.
        let mun = baked_field(&mut cache, "mun", 7);
        let unspin = crate::structures::spin_rotation(&env).conjugate();
        cache.sea_probe(
            &mun,
            id,
            3_600.0,
            600_000.0,
            600_000.0,
            unspin,
            wrapped,
            env.spin_axis,
        );
        assert_eq!(cache.sea.rebuilds, 4, "a heightfield change must rebuild");
    }
}
