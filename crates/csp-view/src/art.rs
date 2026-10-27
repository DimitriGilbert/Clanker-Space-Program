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
//! bake's welded, edge-shaded, smoothing-wedge-normalled models —
//! there is no subdivision stage (the authored geometry is dense at
//! bake).

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

/// A baked part mesh — real authored geometry from the procedural
/// part generator (`xtask bake_art`'s `parts.rs`).
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
    /// The surface-pass texture set (terrain classes, water).
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
                    inner.models.insert(name, Arc::new(m));
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
                    match name.as_str() {
                        "albedo" => t.albedo = Some(tex),
                        "normal" => t.normal = Some(tex),
                        "water_nrm" => t.water = Some(tex),
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
}

impl TerrainTexSet {
    /// Complete set → `TerrainArt` with a content key; incomplete →
    /// `None` (all-or-nothing — the shader's flag covers the pack).
    /// (`stars` was dropped from the pack: the night sky is
    /// `terrain.wgsl`'s procedural starfield now, no baked image.)
    fn finish(self) -> Option<Arc<TerrainArt>> {
        let (albedo, normal, water) = (self.albedo?, self.normal?, self.water?);
        let mut key = 0xcbf2_9ce4_8422_2325u64;
        for t in [&albedo, &normal, &water] {
            key = (key ^ (t.data.len() as u64)).wrapping_mul(0x0000_0100_0000_01b3)
                ^ (u64::from(t.w) << 32 | u64::from(t.h));
        }
        Some(Arc::new(TerrainArt {
            albedo,
            normal,
            water,
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
    /// Inflate the next `u64 len || deflate` payload, bounded at
    /// `limit` — the exact byte count the entry's header implies, so
    /// a payload cannot balloon past its declared shape (and a corrupt
    /// header cannot size the allocation).
    fn inflate(&mut self, limit: usize) -> Result<Vec<u8>, String> {
        let n =
            usize::try_from(self.u64()?).map_err(|_| "csar payload length overflow".to_string())?;
        let comp = self.take(n)?;
        miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(comp, limit)
            .map_err(|e| format!("csar inflate: {e:?}"))
    }
}

/// sRGB channel (0..1) → linear — shared by the mesh decode below and
/// `terrain::build_packet`'s `class_means` uniform fill.
pub(crate) fn srgb_to_linear(u: f32) -> f32 {
    if u <= 0.04045 {
        u / 12.92
    } else {
        ((u + 0.055) / 1.055).powf(2.4)
    }
}

fn decode(b: &[u8]) -> Result<Vec<(String, Entry)>, String> {
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
        // Owned per entry — the `&'static str` this used to `Box::leak`
        // let the entry names outlive `load()`; every `View`
        // construction leaked them again (C12-F6).
        let name = String::from_utf8_lossy(r.take(nlen)?).into_owned();
        match kind {
            1 => out.push((name, Entry::Image(read_image(&mut r)?))),
            2 => {
                let (nv, ni) = (r.u32()? as usize, r.u32()? as usize);
                // u64 math — `nv * 32` alone overflows usize on 32-bit
                // targets (wasm) for adversarial counts.
                let expected = usize::try_from(nv as u64 * 32 + ni as u64 * 4)
                    .map_err(|_| "mesh payload length overflow".to_string())?;
                let raw = r.inflate(expected)?;
                if raw.len() != expected {
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

/// Read one kind-1 image entry's tail — `u16 w, u16 h, u8 layers,
/// u8 mips, layers×4 means, u64 len || deflate`. The upload slices
/// `data` mip-major on the header's word alone (csp-render
/// `write_art_tex`), so the shape must be proven here: a payload that
/// is not exactly `layers × Σ_l (w>>l)×(h>>l)×4` bytes is a decode
/// error, not a startup panic — the same cross-check the mesh branch
/// has always run.
fn read_image(r: &mut Rd<'_>) -> Result<TerrainTex, String> {
    let (w, h) = (u32::from(r.u16()?), u32::from(r.u16()?));
    let (layers, mips) = (u32::from(r.u8()?), u32::from(r.u8()?));
    let mut means = Vec::with_capacity(layers as usize);
    for _ in 0..layers {
        means.push(self_read4(r)?);
    }
    let expected = image_payload_len(w, h, layers, mips)?;
    let data = r.inflate(expected)?;
    if data.len() != expected {
        return Err(format!(
            "image payload size mismatch (want {expected} B, got {} B)",
            data.len()
        ));
    }
    Ok(TerrainTex {
        w,
        h,
        layers,
        mips,
        means,
        data,
    })
}

/// The byte length an image entry's header implies — `layers` images
/// of `(w>>l)×(h>>l)×4` RGBA8 per mip level, levels `0..mips`, the
/// mip-major layout `write_art_tex` slices — plus the shape sanity
/// the upload assumes: nonzero dims and layers, dims inside wgpu's
/// 2-D texture limit, and a mip chain no deeper than `max(w, h)`
/// supports (a header outside that is one no bake could produce —
/// and `w >> level` for `level ≥ 32` would panic on its own).
fn image_payload_len(w: u32, h: u32, layers: u32, mips: u32) -> Result<usize, String> {
    const MAX_DIM: u32 = 8192; // wgpu's default max_texture_dimension_2d
    if w == 0 || h == 0 || layers == 0 {
        return Err(format!("degenerate image shape {w}x{h}x{layers}"));
    }
    if w > MAX_DIM || h > MAX_DIM {
        return Err(format!("image {w}x{h} exceeds the {MAX_DIM} texture cap"));
    }
    // A mip chain can have at most floor(log2(max_dim)) + 1 levels
    // (the bake's chain ends at 1×1 — anything deeper is corrupt).
    let max_mips = 32 - w.max(h).leading_zeros();
    if mips == 0 || mips > max_mips {
        return Err(format!("bad mip count {mips} for {w}x{h}"));
    }
    let mut total = 0u64;
    for l in 0..mips {
        let lw = u64::from((w >> l).max(1));
        let lh = u64::from((h >> l).max(1));
        total += u64::from(layers) * lw * lh * 4;
    }
    usize::try_from(total).map_err(|_| "image payload length overflow".to_string())
}

/// The baked model key for a part — the ONE classifier the VAB and
/// flight views share (C20-F1: the editor's preview and the flown
/// craft must resolve the same model for the same part). Resolution
/// is **id-first, kind-second**: a catalog `PartId`'s substring
/// selects the precise family when an id is known — the VAB always
/// has `EffectivePart::part_id`, and flight passes the node's
/// `CraftDef::part_ids` slot (empty on hand-built craft). Only when
/// no id exists does the module-family `kind` decide — and both
/// surfaces feed it through the shared `crate::flight::part_kind`,
/// so the two arms cannot drift apart again. `pointed` = the
/// collider profile ends in a top apex (nose-cone class) — the
/// silhouette upgrade on `Structural` alone.
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
        if id.contains(".chute.") || id.contains(".parachute.") {
            return "chute";
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
        K::Chute => "chute",
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
        assert!(t.water.mips > 4);
    }

    /// C20-F1: every shipped catalog part resolves the SAME baked
    /// model key through the VAB's `EffectivePart` path and flight's
    /// `WeldPart` + `part_ids` path. The two disjoint classifiers
    /// used to swap models across the editor→flight seam (chute →
    /// adapter, ember → engine_a, decoupler → adapter, dockport →
    /// decoupler); sharing `model_key` + `crate::flight::part_kind`
    /// makes that structurally impossible — this gates the whole
    /// catalog so a new part or id scheme cannot reopen it.
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn editor_flight_model_key_parity() {
        let root = crate::data_root();
        let src = csp_data::FsDataSource;
        let catalog = csp_data::load_parts_from(&src, &root.join("parts")).unwrap();
        let res = csp_data::load_resources_from(&src, &root.join("resources.ron")).unwrap();
        let art = load();
        assert!(!catalog.parts.is_empty(), "shipped catalog is empty");
        for entry in &catalog.parts {
            let def = &entry.def;
            let variant = def
                .variants
                .first()
                .map(|v| v.id.clone())
                .unwrap_or_else(|| csp_data::VariantId::from("stock"));
            let built = csp_phys::BuiltPart::stock(def.id.clone(), variant);
            let eff = csp_phys::resolve::resolve_built_part(
                def,
                &built,
                &res,
                &csp_phys::FamilyQualification::FRESH,
            )
            .unwrap();
            let id = eff.part_id.as_str();
            // The shared kind — the same call both surfaces run.
            let kind = crate::flight::part_kind(&eff.weld_input);
            // The editor keys off `EffectivePart::part_id` and the
            // `classify_effective` silhouette (attach nodes included)…
            let editor_key = model_key(
                Some(id),
                kind,
                crate::part_mesh::is_pointed(&crate::part_mesh::classify_effective(&eff)),
            );
            // …flight keys off the node's `part_ids` slot (the same
            // id for a catalog craft) and the `classify_weld_part`
            // silhouette.
            let flight_key = model_key(
                Some(id),
                kind,
                crate::part_mesh::is_pointed(&crate::part_mesh::classify_weld_part(
                    &eff.weld_input,
                )),
            );
            assert_eq!(editor_key, flight_key, "model key differs for {id}");
            assert!(
                art.0.models.contains_key(editor_key),
                "{id} resolves {editor_key} — no baked model under that key"
            );
        }
    }

    /// The kind arm the old flight classifier lacked: a `ParachuteSpec`
    /// on an id-less (hand-built) part still keys the chute model —
    /// the C20-F1 gap now covered by `PartKind::Chute`.
    #[test]
    fn model_key_kind_fallback_covers_chute() {
        use crate::flight::PartKind as K;
        assert_eq!(model_key(None, K::Chute, false), "chute");
        assert_eq!(model_key(None, K::Port, false), "decoupler");
        assert_eq!(model_key(None, K::Structural, true), "nosecone");
        assert_eq!(model_key(None, K::Structural, false), "adapter");
    }

    /// A minimal in-memory pack with one image entry — `w,h` level-0
    /// dims, `layers`, `mips`, and `payload` as the INFLATED bytes.
    fn image_pack(w: u16, h: u16, layers: u8, mips: u8, payload: &[u8]) -> Vec<u8> {
        let mut pack = b"CSAR".to_vec();
        pack.extend_from_slice(&2u32.to_le_bytes()); // version
        pack.extend_from_slice(&1u32.to_le_bytes()); // n entries
        pack.push(1); // kind: image
        pack.push(3); // name len
        pack.extend_from_slice(b"tex");
        pack.extend_from_slice(&w.to_le_bytes());
        pack.extend_from_slice(&h.to_le_bytes());
        pack.push(layers);
        pack.push(mips);
        pack.extend_from_slice(&vec![9u8; layers as usize * 4]); // means
        let comp = miniz_oxide::deflate::compress_to_vec_zlib(payload, 6);
        pack.extend_from_slice(&(comp.len() as u64).to_le_bytes());
        pack.extend_from_slice(&comp);
        pack
    }

    /// A corrupt pack — a valid header whose payload inflates short of
    /// the declared `w×h×layers×mips` shape — fails `decode` with an
    /// error, not a panic: the upload slices `data` on the header's
    /// word, so the cross-check must happen here (C12-F1).
    #[test]
    fn truncated_image_payload_errors_not_panics() {
        // 64×64 ×1 layer ×2 mips wants 64·64·4 + 32·32·4 = 20480 B.
        let pack = image_pack(64, 64, 1, 2, &[0u8; 16]);
        assert!(decode(&pack).is_err());
        // So does a payload that inflates PAST the declared shape.
        let pack = image_pack(64, 64, 1, 2, &[0u8; 20481]);
        assert!(decode(&pack).is_err());
    }

    /// A mip count deeper than `max(w,h)` supports is corrupt on its
    /// face — `w >> level` for level ≥ 32 would panic before any
    /// length check ran.
    #[test]
    fn absurd_mip_count_errors_not_panics() {
        // 4×4 supports exactly 3 levels (4→2→1); 33 is nonsense.
        let pack = image_pack(4, 4, 1, 33, &[0u8; 4096]);
        assert!(decode(&pack).is_err());
        let pack = image_pack(4, 4, 1, 0, &[]);
        assert!(decode(&pack).is_err());
    }

    /// A well-formed small image entry still decodes — the shape math
    /// accepts exactly the mip-major layout the bake writes.
    #[test]
    fn wellformed_image_entry_decodes() {
        // 4×4 ×1 layer, full chain 4→2→1 = 3 levels: (16+4+1)×4 B.
        let payload = vec![7u8; (16 + 4 + 1) * 4];
        let pack = image_pack(4, 4, 1, 3, &payload);
        let entries = decode(&pack).unwrap();
        let [(name, Entry::Image(tex))] = entries.as_slice() else {
            panic!("expected one image entry, got {}", entries.len())
        };
        assert_eq!(name, "tex");
        assert_eq!((tex.w, tex.h, tex.layers, tex.mips), (4, 4, 1, 3));
        assert_eq!(tex.data, payload);
    }
}
