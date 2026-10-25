//! Real art assets — the `.csar` packs baked by
//! `cargo run -p xtask --bin bake_art` from the CC0 sources under
//! `assets/src/` (provenance + licenses: `assets/ASSETS.md`).
//!
//! The packs are embedded via `include_bytes!` so native and the §6.3
//! wasm demo share one code path — this is *loading* real authored
//! assets (models + PBR textures), not runtime noise synthesis. Any
//! decode failure degrades gracefully to the procedural fallback:
//! parts keep their kitbash lathes, the surface keeps its noise detail
//! (the `uniforms.art.x` flag stays 0).
//!
//! ## `.csar` layout (little-endian, mirrors `xtask::bake_art`)
//!
//! `"CSAR"` · u32 version=2 · u32 n_entries; then per entry `u8 kind`,
//! `u8 name_len`, name bytes — kind 1 image (`u16 w, u16 h, u8 layers,
//! u8 mips, layers×4 mean RGBA, u64 len, deflate payload` — mip-major:
//! each level holds all layers contiguous), kind 2 mesh (`u32 nv,
//! u32 ni, u64 len, deflate` — vertex = f32×3 pos + f32×3 normal +
//! u8×4 sRGB color + u8×2 material (spec strength, spec exponent) +
//! u8 occlusion + u8 pad = 32 B, then u32 indices). Meshes are the
//! bake's welded, Loop-subdivided, smoothing-wedge-normalled models —
//! the low-poly facet look is removed at bake time.

use std::collections::BTreeMap;
use std::sync::Arc;

use bevy_ecs::prelude::*;
use csp_render::terrain::{TerrainArt, TerrainTex};

/// One vertex of a baked part model — model-local position (z-up —
/// the bake converted glTF y-up), unit normal, and the linear RGBA
/// albedo decoded from the source material color. `mat` carries the
/// baked per-material lighting terms: `[specular strength 0..1,
/// specular exponent, cavity occlusion 0..1]` — the shader's per-pixel
/// lighting reads them straight.
#[derive(Clone, Copy, Debug)]
pub struct ModelVert {
    /// Model-local position (m, model's own units — fitted at emit).
    pub pos: [f32; 3],
    /// Unit outward normal, model-local — the bake's smoothing-wedge
    /// normal (split across creases, smooth within a panel).
    pub nrm: [f32; 3],
    /// Linear RGBA albedo (source material × baked edge shading).
    pub col: [f32; 4],
    /// `[spec strength, spec exponent, occlusion]` — baked material
    /// terms; `[0, ·, 1]` shades matte.
    pub mat: [f32; 3],
}

/// A baked part mesh — real model geometry from the Kenney Space Kit.
#[derive(Debug)]
pub struct PartModel {
    /// Vertices.
    pub verts: Vec<ModelVert>,
    /// Triangle indices.
    pub indices: Vec<u32>,
    /// Model-local AABB min corner.
    pub lo: csp_core::DVec3,
    /// Model-local AABB max corner.
    pub hi: csp_core::DVec3,
}

/// Inner catalog — `Arc`-wrapped so cloning the resource is a refcount.
#[derive(Debug, Default)]
pub struct ArtInner {
    /// Named part meshes (`"pod"`, `"engine_a"`, `"tank"`, …).
    pub models: BTreeMap<String, Arc<PartModel>>,
    /// The surface-pass texture set (terrain classes, water, stars).
    pub terrain: Option<Arc<TerrainArt>>,
}

/// The loaded art catalog — a presentation resource inserted by
/// `View::from_source`; `with_sim` worlds get the empty default
/// (procedural fallbacks everywhere).
#[derive(Resource, Clone, Default)]
pub struct ArtAssets(pub Arc<ArtInner>);

impl ArtAssets {
    /// Look up a model by bake key.
    pub fn model(&self, key: &str) -> Option<Arc<PartModel>> {
        self.0.models.get(key).cloned()
    }
}

const PARTS: &[u8] = include_bytes!("../../../data/art/parts.csar");
const TERRAIN: &[u8] = include_bytes!("../../../data/art/terrain.csar");

/// Decode both packs into the catalog. Silent on success; on a bad
/// pack the affected assets are dropped (log + fallback), never fatal
/// — presentation assets must not gate startup (§2.13).
pub fn load() -> ArtAssets {
    let mut inner = ArtInner::default();
    match decode(PARTS) {
        Ok(entries) => {
            for (name, e) in entries {
                if let Entry::Mesh(m) = e {
                    inner.models.insert(name.to_owned(), Arc::new(m));
                }
            }
        }
        Err(e) => log::warn!("art: parts.csar decode failed ({e}) — kitbash fallback"),
    }
    match decode(TERRAIN) {
        Ok(entries) => {
            let mut t = TerrainTexSet::default();
            for (name, e) in entries {
                if let Entry::Image(tex) = e {
                    match name {
                        "albedo" => t.albedo = Some(tex),
                        "normal" => t.normal = Some(tex),
                        "water_nrm" => t.water = Some(tex),
                        "stars" => t.stars = Some(tex),
                        _ => {}
                    }
                }
            }
            inner.terrain = t.finish();
        }
        Err(e) => log::warn!("art: terrain.csar decode failed ({e}) — procedural fallback"),
    }
    ArtAssets(Arc::new(inner))
}

#[derive(Default)]
struct TerrainTexSet {
    albedo: Option<TerrainTex>,
    normal: Option<TerrainTex>,
    water: Option<TerrainTex>,
    stars: Option<TerrainTex>,
}

impl TerrainTexSet {
    /// Complete set → `TerrainArt` with a content key; incomplete →
    /// `None` (all-or-nothing — the shader's flag covers the pack).
    fn finish(self) -> Option<Arc<TerrainArt>> {
        let (albedo, normal, water, stars) = (self.albedo?, self.normal?, self.water?, self.stars?);
        let mut key = 0xcbf2_9ce4_8422_2325u64;
        for t in [&albedo, &normal, &water, &stars] {
            key = (key ^ (t.data.len() as u64)).wrapping_mul(0x0000_0100_0000_01b3)
                ^ (u64::from(t.w) << 32 | u64::from(t.h));
        }
        Some(Arc::new(TerrainArt {
            albedo,
            normal,
            water,
            stars,
            key,
        }))
    }
}

enum Entry {
    Image(TerrainTex),
    Mesh(PartModel),
}

/// Byte cursor over the pack — bounds-checked reads, `Err` on
/// truncation (never panics: a corrupt asset degrades, it doesn't kill).
struct Rd<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> Rd<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        if self.p + n > self.b.len() {
            return Err("truncated csar".to_string());
        }
        let s = &self.b[self.p..self.p + n];
        self.p += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, String> {
        Ok(u16::from_le_bytes(
            self.take(2)?.try_into().map_err(|_| "u16")?,
        ))
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().map_err(|_| "u32")?,
        ))
    }
    fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().map_err(|_| "u64")?,
        ))
    }
    fn inflate(&mut self) -> Result<Vec<u8>, String> {
        let n = self.u64()? as usize;
        let comp = self.take(n)?;
        miniz_oxide::inflate::decompress_to_vec_zlib(comp)
            .map_err(|e| format!("csar inflate: {e:?}"))
    }
}

fn srgb_to_linear(u: f32) -> f32 {
    if u <= 0.04045 {
        u / 12.92
    } else {
        ((u + 0.055) / 1.055).powf(2.4)
    }
}

fn decode(b: &[u8]) -> Result<Vec<(&str, Entry)>, String> {
    let mut r = Rd { b, p: 0 };
    if r.take(4)? != b"CSAR" {
        return Err("bad magic".to_string());
    }
    let ver = r.u32()?;
    if ver != 2 {
        return Err(format!("csar version {ver}"));
    }
    let n = r.u32()?;
    let mut out = Vec::new();
    for _ in 0..n {
        let kind = r.u8()?;
        let nlen = r.u8()? as usize;
        let name = String::from_utf8_lossy(r.take(nlen)?).into_owned();
        let name: &'static str = Box::leak(name.into_boxed_str());
        match kind {
            1 => {
                let (w, h) = (u32::from(r.u16()?), u32::from(r.u16()?));
                let (layers, mips) = (u32::from(r.u8()?), u32::from(r.u8()?));
                let mut means = Vec::with_capacity(layers as usize);
                for _ in 0..layers {
                    means.push(self_read4(&mut r)?);
                }
                let data = r.inflate()?;
                out.push((
                    name,
                    Entry::Image(TerrainTex {
                        w,
                        h,
                        layers,
                        mips,
                        means,
                        data,
                    }),
                ));
            }
            2 => {
                let (nv, ni) = (r.u32()? as usize, r.u32()? as usize);
                let raw = r.inflate()?;
                if raw.len() != nv * 32 + ni * 4 {
                    return Err("mesh payload size mismatch".to_string());
                }
                let mut lo = [f32::MAX; 3];
                let mut hi = [f32::MIN; 3];
                let mut verts = Vec::with_capacity(nv);
                for i in 0..nv {
                    let o = i * 32;
                    let f = |k: usize| {
                        f32::from_le_bytes(
                            raw[o + k * 4..o + k * 4 + 4].try_into().unwrap_or_default(),
                        )
                    };
                    let pos = [f(0), f(1), f(2)];
                    let nrm = [f(3), f(4), f(5)];
                    for a in 0..3 {
                        lo[a] = lo[a].min(pos[a]);
                        hi[a] = hi[a].max(pos[a]);
                    }
                    let c = &raw[o + 24..o + 28];
                    verts.push(ModelVert {
                        pos,
                        nrm,
                        col: [
                            srgb_to_linear(f32::from(c[0]) / 255.0),
                            srgb_to_linear(f32::from(c[1]) / 255.0),
                            srgb_to_linear(f32::from(c[2]) / 255.0),
                            f32::from(c[3]) / 255.0,
                        ],
                        mat: [
                            f32::from(raw[o + 28]) / 255.0,
                            f32::from(raw[o + 29]),
                            f32::from(raw[o + 30]) / 255.0,
                        ],
                    });
                }
                let mut indices = Vec::with_capacity(ni);
                for i in 0..ni {
                    let o = nv * 32 + i * 4;
                    indices.push(u32::from_le_bytes(
                        raw[o..o + 4].try_into().unwrap_or_default(),
                    ));
                }
                let d = |v: [f32; 3]| {
                    csp_core::DVec3::new(f64::from(v[0]), f64::from(v[1]), f64::from(v[2]))
                };
                out.push((
                    name,
                    Entry::Mesh(PartModel {
                        verts,
                        indices,
                        lo: d(lo),
                        hi: d(hi),
                    }),
                ));
            }
            k => return Err(format!("unknown csar kind {k}")),
        }
    }
    Ok(out)
}

fn self_read4(r: &mut Rd<'_>) -> Result<[u8; 4], String> {
    let s = r.take(4)?;
    Ok([s[0], s[1], s[2], s[3]])
}

/// The baked model key for a part — the def id wins when known
/// (VAB path), otherwise the render kind + silhouette hint decides
/// (flight path). `pointed` = the collider profile ends in a top apex
/// (nose-cone class) — structural parts only.
pub fn model_key(
    part_id: Option<&str>,
    kind: crate::flight::PartKind,
    pointed: bool,
) -> &'static str {
    if let Some(id) = part_id {
        if id.contains(".engine.") {
            return if id.contains("ember") {
                "engine_b"
            } else {
                "engine_a"
            };
        }
        if id.contains(".pod.") {
            return "pod";
        }
        if id.contains(".tank.") {
            return "tank";
        }
        if id.contains(".nosecone.") || id.contains(".aero.") || id.contains(".fairing.") {
            return "nosecone";
        }
        if id.contains(".decoupler.") || id.contains(".separator.") {
            return "decoupler";
        }
        if id.contains(".adapter.") || id.contains(".structure.") {
            return "adapter";
        }
        if id.contains(".battery.") {
            return "battery";
        }
        if id.contains(".science.") || id.contains(".experiment.") || id.contains(".canister.") {
            return "science";
        }
        if id.contains(".fin") {
            return "fins";
        }
        if id.contains(".booster.") || id.contains(".srb.") {
            return "booster";
        }
    }
    use crate::flight::PartKind as K;
    match kind {
        K::Engine => "engine_a",
        K::Crew => "pod",
        K::Tank => "tank",
        K::Battery => "battery",
        K::Science => "science",
        K::Port => "decoupler",
        K::Structural => {
            if pointed {
                "nosecone"
            } else {
                "adapter"
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// The embedded packs decode and the expected entries exist — the
    /// gate that keeps a rebake/schema drift loud instead of silently
    /// falling back to procedural visuals.
    #[test]
    fn packs_decode() {
        let art = load();
        assert!(art.0.terrain.is_some(), "terrain pack missing");
        for k in ["pod", "engine_a", "tank", "nosecone", "decoupler"] {
            assert!(art.0.models.contains_key(k), "missing model {k}");
        }
        let t = art.0.terrain.as_ref().unwrap();
        assert_eq!(t.albedo.layers, 4);
        assert!(t.stars.mips > 4);
    }
}
