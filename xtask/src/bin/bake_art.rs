//! `cargo run -p xtask --bin bake_art` — bake the real art assets in
//! `assets/src/` (see `assets/ASSETS.md` for provenance/licenses) into the
//! `.csar` packs the shipped build loads from `data/art/`.
//!
//! ## `.csar` (Clanker Space Art Resource) — little-endian
//!
//! ```text
//! "CSAR"  u32 version=1  u32 n_entries
//! per entry:  u8 kind, u8 name_len, name bytes
//!   kind 1 = image: u16 w, u16 h, u8 layers, u8 mips,
//!                   layers×4 u8 mean-RGBA (per-layer average),
//!                   u64 payload_len, deflate(payload)
//!          payload = mip-major: level 0..mips, each level's
//!          layers contiguous — (w>>l)×(h>>l)×4 RGBA8.
//!   kind 2 = mesh:  u32 n_verts, u32 n_idx, u64 payload_len,
//!                   deflate(payload)
//!          vertex  = f32×3 pos, f32×3 nrm, u8×4 srgb color (28 B)
//!          payload = verts ++ u32 indices (LE)
//! ```
//!
//! The decoder is `csp-view`'s `art` module; the format is deliberately
//! frozen-independent — bump `version` on any layout change.

#![allow(clippy::unwrap_used, clippy::expect_used)] // bake tooling: loud failure is the honest path
#![allow(clippy::too_many_lines)]

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

// ---------------------------------------------------------------------------
// .csar writer
// ---------------------------------------------------------------------------

const KIND_IMAGE: u8 = 1;
const KIND_MESH: u8 = 2;

struct ImageEntry {
    w: u16,
    h: u16,
    layers: u8,
    mips: u8,
    means: Vec<[u8; 4]>,
    /// mip-major payload: level → layer → rgba8
    payload: Vec<u8>,
}

struct MeshEntry {
    verts: Vec<MeshVert>,
    indices: Vec<u32>,
}

#[derive(Clone, Copy)]
struct MeshVert {
    pos: [f32; 3],
    nrm: [f32; 3],
    col: [u8; 4],
}

enum Entry {
    Image(ImageEntry),
    Mesh(MeshEntry),
}

fn write_csar(path: &Path, entries: &[(&str, Entry)]) {
    let mut out = Vec::new();
    out.extend_from_slice(b"CSAR");
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for (name, e) in entries {
        match e {
            Entry::Image(_) => out.push(KIND_IMAGE),
            Entry::Mesh(_) => out.push(KIND_MESH),
        }
        out.push(name.len() as u8);
        out.extend_from_slice(name.as_bytes());
        match e {
            Entry::Image(img) => {
                out.extend_from_slice(&img.w.to_le_bytes());
                out.extend_from_slice(&img.h.to_le_bytes());
                out.push(img.layers);
                out.push(img.mips);
                for m in &img.means {
                    out.extend_from_slice(m);
                }
                let comp = miniz_oxide::deflate::compress_to_vec_zlib(&img.payload, 8);
                out.extend_from_slice(&(comp.len() as u64).to_le_bytes());
                out.extend_from_slice(&comp);
            }
            Entry::Mesh(m) => {
                out.extend_from_slice(&(m.verts.len() as u32).to_le_bytes());
                out.extend_from_slice(&(m.indices.len() as u32).to_le_bytes());
                let mut raw = Vec::with_capacity(m.verts.len() * 28 + m.indices.len() * 4);
                for v in &m.verts {
                    for f in v.pos.iter().chain(v.nrm.iter()) {
                        raw.extend_from_slice(&f.to_le_bytes());
                    }
                    raw.extend_from_slice(&v.col);
                }
                for i in &m.indices {
                    raw.extend_from_slice(&i.to_le_bytes());
                }
                let comp = miniz_oxide::deflate::compress_to_vec_zlib(&raw, 8);
                out.extend_from_slice(&(comp.len() as u64).to_le_bytes());
                out.extend_from_slice(&comp);
            }
        }
    }
    fs::write(path, &out).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
    println!(
        "wrote {} ({} entries, {} B)",
        path.display(),
        entries.len(),
        out.len()
    );
}

// ---------------------------------------------------------------------------
// glTF (.glb) minimal reader — POSITION/NORMAL/indices + material colors.
// Enough for the Kenney packs (non-interleaved, f32 accessors).
// ---------------------------------------------------------------------------

fn comp_read<'a>(bin: &'a [u8], views: &[Value], acc: &Value) -> (&'a [u8], usize, usize) {
    let bv = &views[acc["bufferView"].as_u64().unwrap() as usize];
    let off = bv["byteOffset"].as_u64().unwrap_or(0) as usize
        + acc["byteOffset"].as_u64().unwrap_or(0) as usize;
    let n = acc["count"].as_u64().unwrap() as usize;
    let ty = acc["type"].as_str().unwrap();
    let comps = match ty {
        "SCALAR" => 1,
        "VEC2" => 2,
        "VEC3" => 3,
        "VEC4" => 4,
        _ => panic!("accessor type {ty}"),
    };
    (&bin[off..], n, comps)
}

fn read_f32v3(bin: &[u8], views: &[Value], acc: &Value) -> Vec<[f32; 3]> {
    assert_eq!(acc["componentType"].as_u64().unwrap(), 5126);
    let (buf, n, comps) = comp_read(bin, views, acc);
    assert_eq!(comps, 3);
    (0..n)
        .map(|i| {
            let o = i * 12;
            [
                f32::from_le_bytes(buf[o..o + 4].try_into().unwrap()),
                f32::from_le_bytes(buf[o + 4..o + 8].try_into().unwrap()),
                f32::from_le_bytes(buf[o + 8..o + 12].try_into().unwrap()),
            ]
        })
        .collect()
}

fn read_indices(bin: &[u8], views: &[Value], acc: &Value) -> Vec<u32> {
    let (buf, n, _) = comp_read(bin, views, acc);
    match acc["componentType"].as_u64().unwrap() {
        5123 => (0..n)
            .map(|i| u16::from_le_bytes(buf[i * 2..i * 2 + 2].try_into().unwrap()) as u32)
            .collect(),
        5125 => (0..n)
            .map(|i| u32::from_le_bytes(buf[i * 4..i * 4 + 4].try_into().unwrap()))
            .collect(),
        c => panic!("index componentType {c}"),
    }
}

fn srgb_u8(v: f64) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Load a GLB: merge all primitives, bake material `baseColorFactor`
/// into per-vertex colors (sRGB), and convert glTF y-up to the game's
/// z-up part frame `(x,y,z) -> (x,-z,y)` — model "up" lands on the
/// stack axis.
fn load_glb(path: &Path) -> MeshEntry {
    let d = fs::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    assert_eq!(&d[0..4], b"glTF", "{}: not a GLB", path.display());
    let jlen = u32::from_le_bytes(d[12..16].try_into().unwrap()) as usize;
    let json: Value = serde_json::from_slice(&d[20..20 + jlen]).unwrap();
    let bin_off = 20 + jlen;
    assert_eq!(&d[bin_off + 4..bin_off + 8], b"BIN\0", "no BIN chunk");
    let blen = u32::from_le_bytes(d[bin_off..bin_off + 4].try_into().unwrap()) as usize;
    let bin = &d[bin_off + 8..bin_off + 8 + blen];

    let views: Vec<Value> = json["bufferViews"].as_array().unwrap().clone();
    let accs: Vec<Value> = json["accessors"].as_array().unwrap().clone();
    let mats: Vec<Value> = json["materials"].as_array().cloned().unwrap_or_default();

    let mut verts: Vec<MeshVert> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    for mesh in json["meshes"].as_array().unwrap() {
        for prim in mesh["primitives"].as_array().unwrap() {
            let mode = prim["mode"].as_u64().unwrap_or(4);
            assert_eq!(mode, 4, "non-triangle primitive");
            let attrs = &prim["attributes"];
            let pos = read_f32v3(
                bin,
                &views,
                &accs[attrs["POSITION"].as_u64().unwrap() as usize],
            );
            let nrm = read_f32v3(
                bin,
                &views,
                &accs[attrs["NORMAL"].as_u64().unwrap() as usize],
            );
            let col = prim["material"]
                .as_u64()
                .and_then(|m| {
                    mats[m as usize]["pbrMetallicRoughness"]["baseColorFactor"]
                        .as_array()
                        .map(|a| {
                            [
                                srgb_u8(a[0].as_f64().unwrap_or(1.0)),
                                srgb_u8(a[1].as_f64().unwrap_or(1.0)),
                                srgb_u8(a[2].as_f64().unwrap_or(1.0)),
                                srgb_u8(a[3].as_f64().unwrap_or(1.0)),
                            ]
                        })
                })
                .unwrap_or([255, 255, 255, 255]);
            let base = verts.len() as u32;
            for (p, n) in pos.iter().zip(&nrm) {
                verts.push(MeshVert {
                    // y-up → z-up: (x,y,z) -> (x,-z,y)
                    pos: [p[0], -p[2], p[1]],
                    nrm: [n[0], -n[2], n[1]],
                    col,
                });
            }
            match prim.get("indices").and_then(|i| i.as_u64()) {
                Some(a) => indices.extend(
                    read_indices(bin, &views, &accs[a as usize])
                        .iter()
                        .map(|i| i + base),
                ),
                None => indices.extend(base..base + pos.len() as u32),
            }
        }
    }
    MeshEntry { verts, indices }
}

// ---------------------------------------------------------------------------
// Images — decode, resize, mip chain (box downsample), per-layer means.
// ---------------------------------------------------------------------------

fn load_rgba8(path: &Path, w: u32, h: u32) -> Vec<u8> {
    let img = image::open(path)
        .unwrap_or_else(|e| panic!("decode {}: {e}", path.display()))
        .resize_exact(w, h, image::imageops::FilterType::Lanczos3)
        .to_rgba8();
    img.into_raw()
}

/// Radiance .hdr → tone-mapped sRGB RGBA8. `image`'s hdr decoder yields
/// Rgb32F; Reinhard + a modest exposure keep bright stars above the
/// skyglow floor without clipping the Milky Way to a grey wash.
fn load_hdr_rgba8(path: &Path, w: u32, h: u32, exposure: f32) -> Vec<u8> {
    let img = image::load_from_memory_with_format(
        &fs::read(path).unwrap_or_else(|e| panic!("{e}")),
        image::ImageFormat::Hdr,
    )
    .unwrap_or_else(|e| panic!("decode {}: {e}", path.display()))
    .to_rgb32f();
    let (dw, dh) = img.dimensions();
    let mut raw: Vec<u8> = Vec::with_capacity((dw * dh * 4) as usize);
    for px in img.as_raw().chunks_exact(3) {
        for &c in px {
            let v: f32 = 1.0 - (-exposure * c).exp(); // soft roll-off
            let s: f32 = if v <= 0.003_130_8 {
                12.92 * v
            } else {
                1.055 * v.powf(1.0 / 2.4) - 0.055
            };
            raw.push((s.clamp(0.0, 1.0) * 255.0).round() as u8);
        }
        raw.push(255);
    }
    let img = image::RgbaImage::from_raw(dw, dh, raw).expect("hdr dims");
    image::imageops::resize(&img, w, h, image::imageops::FilterType::Lanczos3).into_raw()
}

fn downsample(level: &[u8], w: usize, h: usize) -> (Vec<u8>, usize, usize) {
    let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
    let mut out = vec![0u8; nw * nh * 4];
    for y in 0..nh {
        for x in 0..nw {
            let mut acc = [0u32; 4];
            let mut cnt = 0u32;
            for dy in 0..2 {
                for dx in 0..2 {
                    let (sx, sy) = (2 * x + dx, 2 * y + dy);
                    if sx < w && sy < h {
                        let o = (sy * w + sx) * 4;
                        for c in 0..4 {
                            acc[c] += level[o + c] as u32;
                        }
                        cnt += 1;
                    }
                }
            }
            let o = (y * nw + x) * 4;
            for c in 0..4 {
                out[o + c] = (acc[c] / cnt) as u8;
            }
        }
    }
    (out, nw, nh)
}

/// Build the mip-major payload + per-layer means for an image stack
/// (`layers` same-sized RGBA8 tiles).
fn image_entry(w: u16, h: u16, level0_layers: Vec<Vec<u8>>) -> ImageEntry {
    let layers = level0_layers.len() as u8;
    let means: Vec<[u8; 4]> = level0_layers
        .iter()
        .map(|l| {
            let n = l.len() as u64 / 4;
            let mut acc = [0u64; 4];
            for px in l.chunks_exact(4) {
                for c in 0..4 {
                    acc[c] += px[c] as u64;
                }
            }
            [0, 1, 2, 3].map(|c| (acc[c] / n.max(1)) as u8)
        })
        .collect();
    let mut mips = 0u8;
    let mut payload = Vec::new();
    let mut cur = level0_layers;
    let (mut cw, mut ch) = (w as usize, h as usize);
    loop {
        mips += 1;
        for layer in &cur {
            payload.extend_from_slice(layer);
        }
        if cw == 1 && ch == 1 {
            break;
        }
        let mut next = Vec::with_capacity(cur.len());
        for layer in &cur {
            next.push(downsample(layer, cw, ch).0);
        }
        cw = (cw / 2).max(1);
        ch = (ch / 2).max(1);
        cur = next;
    }
    ImageEntry {
        w,
        h,
        layers,
        mips,
        means,
        payload,
    }
}

// ---------------------------------------------------------------------------
// The bake
// ---------------------------------------------------------------------------

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let src = root.join("assets/src");
    let out = root.join("data/art");
    fs::create_dir_all(&out).unwrap();

    let glb = |n: &str| src.join(format!("kenney_space-kit/Models/GLTF format/{n}.glb"));

    // --- rocket part models (Kenney Space Kit, CC0) -------------------------
    let parts: &[(&str, &str)] = &[
        ("engine_a", "rocket_baseA"),
        ("engine_b", "rocket_baseB"),
        ("pod", "rocket_topA"),
        ("nosecone", "rocket_topB"),
        ("tank", "rocket_fuelB"),
        ("booster", "rocket_sidesA"),
        ("decoupler", "pipe_ring"),
        ("adapter", "structure"),
        ("battery", "machine_wireless"),
        ("science", "machine_barrel"),
        ("fins", "rocket_finsA"),
    ];
    let mut entries: Vec<(&str, Entry)> = Vec::new();
    for (key, file) in parts {
        let m = load_glb(&glb(file));
        println!(
            "part {key:<10} <- {file}.glb: {} verts {} tris",
            m.verts.len(),
            m.indices.len() / 3
        );
        entries.push((key, Entry::Mesh(m)));
    }
    write_csar(&out.join("parts.csar"), &entries);

    // --- terrain/sky textures (ambientCG + Poly Haven, CC0) ------------------
    let acg = |id: &str, map: &str| src.join(format!("{id}/{id}_1K-JPG_{map}.jpg"));
    const T: u32 = 512;
    let alb_layers: Vec<Vec<u8>> = [
        acg("Grass003", "Color"),
        acg("Rock035", "Color"),
        acg("Ground080", "Color"),
        acg("Snow010A", "Color"),
    ]
    .iter()
    .map(|p| load_rgba8(p, T, T))
    .collect();
    let nrm_layers: Vec<Vec<u8>> = [
        acg("Grass003", "NormalGL"),
        acg("Rock035", "NormalGL"),
        acg("Ground080", "NormalGL"),
        acg("Snow010A", "NormalGL"),
    ]
    .iter()
    .map(|p| load_rgba8(p, T, T))
    .collect();
    let water = load_rgba8(&acg("Foam002", "NormalGL"), T, T);
    let stars = load_hdr_rgba8(&src.join("lakeside_night_2k.hdr"), 1024, 512, 3.0);

    let entries: Vec<(&str, Entry)> = vec![
        // layer order: 0 grass, 1 rock, 2 sand, 3 snow — terrain.wgsl keys it
        (
            "albedo",
            Entry::Image(image_entry(T as u16, T as u16, alb_layers)),
        ),
        (
            "normal",
            Entry::Image(image_entry(T as u16, T as u16, nrm_layers)),
        ),
        (
            "water_nrm",
            Entry::Image(image_entry(T as u16, T as u16, vec![water])),
        ),
        ("stars", Entry::Image(image_entry(1024, 512, vec![stars]))),
    ];
    write_csar(&out.join("terrain.csar"), &entries);
}
