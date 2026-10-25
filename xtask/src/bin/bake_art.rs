//! `cargo run -p xtask --bin bake_art` — bake the real art assets in
//! `assets/src/` (see `assets/ASSETS.md` for provenance/licenses) into the
//! `.csar` packs the shipped build loads from `data/art/`.
//!
//! ## `.csar` (Clanker Space Art Resource) — little-endian
//!
//! ```text
//! "CSAR"  u32 version=2  u32 n_entries
//! per entry:  u8 kind, u8 name_len, name bytes
//!   kind 1 = image: u16 w, u16 h, u8 layers, u8 mips,
//!                   layers×4 u8 mean-RGBA (per-layer average —
//!                   the shader divides class detail by it),
//!                   u64 payload_len, deflate(payload)
//!          payload = mip-major: level 0..mips, each level's
//!          layers contiguous — (w>>l)×(h>>l)×4 RGBA8.
//!   kind 2 = mesh:  u32 n_verts, u32 n_idx, u64 payload_len,
//!                   deflate(payload)
//!          vertex  = f32×3 pos, f32×3 nrm, u8×4 srgb color,
//!                    u8×2 material (spec strength, spec exponent),
//!                    u8 ao, u8 pad (32 B)
//!          payload = verts ++ u32 indices (LE)
//! ```
//!
//! ## Part models — procedurally authored (v3)
//!
//! The Kenney Space Kit models were retired: they are stylized
//! cartoon primitives, and no bake-side pass can un-cartoon a
//! fundamentally low-poly authored silhouette. The parts are now
//! **authored procedurally** by `parts.rs` in this directory — real
//! aerospace geometry built as lathe/sweep/prism meshes with many
//! control points: a Rao-flared double-wall bell nozzle with
//! corrugated cooling channels and a copper liner, an Apollo-class
//! capsule with recessed glass windows and a docking probe, a tank
//! with ellipsoidal domes and raised weld rings, a tangent-ogive
//! nosecone, an SRB, machined rings, a superellipse battery pack —
//! then the same downstream quality pass the kit loader used:
//!
//! 1. **Weld** — all primitives merge on quantized position, so the
//!    authored topology (closed solids, real dihedral angles) is
//!    intact; material borders stay as per-face colors.
//! 2. **Edge shading** — every sharp edge is classified convex vs
//!    concave; convex edges catch a light tint, concave seams darken —
//!    the cheap "panel gap + edge wear" read.
//! 3. **Split normals** — vertex normals rebuilt per smoothing wedge
//!    (angle-weighted, split across creases): smooth regions shade
//!    continuously, machined edges keep their crisp facets.
//! 4. **Material params** — each authored face color maps to a
//!    specular strength/exponent pair (machined dark metal, painted
//!    hull, glossy glazing, charred ablative, accent paint) and a
//!    cavity AO term is baked per vertex — the shader reads them
//!    per pixel.
//!
//! The decoder is `csp-view`'s `art` module; the format is deliberately
//! frozen-independent — bump `version` on any layout change.

#![allow(clippy::unwrap_used, clippy::expect_used)] // bake tooling: loud failure is the honest path
#![allow(clippy::too_many_lines)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

mod parts;

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
    /// `[spec_strength·255, spec_exponent]` — the per-material shininess
    /// pair the shader reads (`mat.x`, `mat.y`).
    mat: [u8; 2],
    /// Baked cavity occlusion, 255 = fully open (`mat.z`).
    ao: u8,
    pad: u8,
}

enum Entry {
    Image(ImageEntry),
    Mesh(MeshEntry),
}

/// Serialize a pack fully in memory — writing happens only in
/// [`atomic_write`], after BOTH packs have assembled without panic.
fn assemble_csar(entries: &[(&str, Entry)]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"CSAR");
    out.extend_from_slice(&2u32.to_le_bytes());
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
                let mut raw = Vec::with_capacity(m.verts.len() * 32 + m.indices.len() * 4);
                for v in &m.verts {
                    for f in v.pos.iter().chain(v.nrm.iter()) {
                        raw.extend_from_slice(&f.to_le_bytes());
                    }
                    raw.extend_from_slice(&v.col);
                    raw.extend_from_slice(&v.mat);
                    raw.push(v.ao);
                    raw.push(v.pad);
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
    out
}

/// Write `bytes` to `path` via a sibling tmp file + rename — a
/// mid-write kill can never leave a truncated pack (the embedded
/// `include_bytes!` packs must never be observed half-written).
fn atomic_write(path: &Path, bytes: &[u8]) {
    let tmp = path.with_extension("csar.tmp");
    fs::write(&tmp, bytes).unwrap_or_else(|e| panic!("write {}: {e}", tmp.display()));
    fs::rename(&tmp, path)
        .unwrap_or_else(|e| panic!("rename {} → {}: {e}", tmp.display(), path.display()));
    println!("wrote {} ({} B)", path.display(), bytes.len());
}

// ---------------------------------------------------------------------------
// Part-model quality pass — weld → edge-shade → split normals. The
// authored meshes (parts.rs) arrive already dense and correctly
// oriented, so no subdivision step is needed. All mesh math in f64 at
// bake; the pack stores f32.
// ---------------------------------------------------------------------------

/// One triangle of the welded working mesh: vertex indices into the
/// shared position table + the face's flat sRGB material color. Colors
/// ride faces, not vertices — a material border stays a hard line.
#[derive(Clone, Copy)]
struct Face {
    v: [u32; 3],
    col: [u8; 4],
}

/// Dihedral threshold for "hard": adjacent faces whose normals meet
/// under this cosine (≈53°) are a crease — the wedge-normal rebuild
/// splits there. Sub-53° bevels stay under the bar (they smooth),
/// 60°+ machined corners sit over it (they stay crisp).
const CREASE_DOT: f64 = 0.6;

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn add(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn scale(a: [f64; 3], k: f64) -> [f64; 3] {
    [a[0] * k, a[1] * k, a[2] * k]
}
fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}
fn len(a: [f64; 3]) -> f64 {
    dot(a, a).sqrt()
}
fn unit(a: [f64; 3]) -> [f64; 3] {
    let l = len(a);
    if l > 1e-12 {
        scale(a, 1.0 / l)
    } else {
        [0.0, 0.0, 1.0]
    }
}

/// Face geometric normals (unit; zero-area faces get the zero vector).
/// Faces are pre-oriented outward at load time via the authored
/// normals, so these are genuine outward normals, not winding guesses.
fn face_normals(pos: &[[f64; 3]], faces: &[Face]) -> Vec<[f64; 3]> {
    faces
        .iter()
        .map(|f| {
            let e = cross(
                sub(pos[f.v[1] as usize], pos[f.v[0] as usize]),
                sub(pos[f.v[2] as usize], pos[f.v[0] as usize]),
            );
            let l = len(e);
            if l > 1e-30 {
                scale(e, 1.0 / l)
            } else {
                [0.0, 0.0, 0.0]
            }
        })
        .collect()
}

/// Every undirected edge → its incident faces.
fn edge_faces(faces: &[Face]) -> BTreeMap<(u32, u32), Vec<usize>> {
    let mut edges: BTreeMap<(u32, u32), Vec<usize>> = BTreeMap::new();
    for (fi, f) in faces.iter().enumerate() {
        for k in 0..3 {
            let (a, b) = (f.v[k], f.v[(k + 1) % 3]);
            edges.entry((a.min(b), a.max(b))).or_default().push(fi);
        }
    }
    edges
}

/// `true` where the edge must stay hard: a boundary, a non-manifold
/// seam, or a dihedral over [`CREASE_DOT`].
fn edge_is_crease(fl: &[usize], fns: &[[f64; 3]]) -> bool {
    fl.len() != 2 || dot(fns[fl[0]], fns[fl[1]]) < CREASE_DOT
}

/// Convex/concave edge-shading: each hard 2-face edge darkens or
/// brightens its endpoint vertices — concave seams read as panel gaps,
/// convex edges catch a faint edge light. Applied directly to the
/// authored mesh (there is no subdivision stage).
fn edge_shade(pos: &[[f64; 3]], faces: &[Face], tint: &mut [f32]) {
    let fns = face_normals(pos, faces);
    for ((a, b), fl) in edge_faces(faces) {
        if fl.len() != 2 {
            continue;
        }
        let n1 = fns[fl[0]];
        if dot(n1, fns[fl[1]]) >= CREASE_DOT || dot(n1, fns[fl[1]]) <= -0.5 {
            continue; // smooth edge, or a folded/back-to-back pair
        }
        let m = scale(add(pos[a as usize], pos[b as usize]), 0.5);
        // The neighbour face's centroid sits below this face's plane on
        // a convex fold, above it on a concave one.
        let c2 = scale(
            faces[fl[1]]
                .v
                .iter()
                .fold([0.0; 3], |acc, &v| add(acc, pos[v as usize])),
            1.0 / 3.0,
        );
        let k = if dot(sub(c2, m), n1) < 0.0 {
            1.06 // convex — edge catch-light
        } else {
            0.86 // concave — panel-gap darkening
        };
        tint[a as usize] *= k;
        tint[b as usize] *= k;
    }
    for t in tint.iter_mut() {
        *t = t.clamp(0.7, 1.15);
    }
}

/// `(spec_strength·255, spec_exponent)` for an authored face color —
/// the per-material shininess the shader mixes per pixel. Dark
/// blue-dominant colors read as glossy glazing (tight, strong
/// highlight); warm darks as charred ablative (matte); the orange
/// accent as paint, the whites as ceramic, the hull greys as paint
/// and brushed metal.
fn mat_params(col: [u8; 4]) -> [u8; 2] {
    let (r, g, b) = (f64::from(col[0]), f64::from(col[1]), f64::from(col[2]));
    let lum = (0.3 * r + 0.5 * g + 0.2 * b) / 255.0;
    if b > r + 16.0 && lum < 0.45 {
        [200, 120] // dark glazing — pod windows, lenses
    } else if r > g * 1.15 && g >= b && lum < 0.30 {
        [40, 10] // charred ablative — heat shields, sooted interiors
    } else if r > 200.0 && g < 190.0 && b < 120.0 {
        [115, 40] // accent paint
    } else if lum < 0.38 {
        [150, 72] // machined dark metal (nozzle, shadowed hardware)
    } else if lum > 0.92 {
        [165, 90] // near-white: ceramic
    } else if lum > 0.72 {
        [100, 44] // painted hull
    } else {
        [125, 56] // brushed mid metal
    }
}

/// Rebuild the emitted mesh from the welded working mesh: smooth
/// vertex normals per smoothing wedge (angle-weighted face normals
/// over the corner's connected-by-smooth-edges face set), split across
/// creases and material colors, plus per-vertex cavity AO from the
/// one-ring centroid dip.
fn finish(pos: &[[f64; 3]], faces: &[Face], tint: &[f32]) -> MeshEntry {
    let fns = face_normals(pos, faces);
    let edges = edge_faces(faces);
    let mut vf: Vec<Vec<usize>> = vec![Vec::new(); pos.len()];
    for (fi, f) in faces.iter().enumerate() {
        for &v in &f.v {
            vf[v as usize].push(fi);
        }
    }
    // Per-position one-ring vertex neighbours (for the AO dip).
    let mut vnbrs: Vec<Vec<u32>> = vec![Vec::new(); pos.len()];
    for &(a, b) in edges.keys() {
        vnbrs[a as usize].push(b);
        vnbrs[b as usize].push(a);
    }
    // Wedge partition: two faces incident at a vertex belong to one
    // smoothing wedge when a SMOOTH edge at that vertex links them.
    // Faces only touching through a crease shade independently.
    let mut wedge_nrm: Vec<Vec<(u32, [f64; 3])>> = vec![Vec::new(); pos.len()];
    let mut face_wedge: Vec<[u32; 3]> = vec![[0; 3]; faces.len()];
    for v in 0..pos.len() as u32 {
        // Link faces in vf[v] via smooth edges at v.
        let incident = &vf[v as usize];
        let mut comp: Vec<i32> = vec![-1; incident.len()];
        let mut n_comp = 0i32;
        for i in 0..incident.len() {
            if comp[i] >= 0 {
                continue;
            }
            let mut stack = vec![i];
            comp[i] = n_comp;
            while let Some(ci) = stack.pop() {
                let f = faces[incident[ci]];
                // The two edges of `f` at v: (v, prev), (v, next).
                let k = f.v.iter().position(|&x| x == v).unwrap();
                for u in [f.v[(k + 1) % 3], f.v[(k + 2) % 3]] {
                    let key = (v.min(u), v.max(u));
                    let Some(fl) = edges.get(&key) else { continue };
                    if edge_is_crease(fl, &fns) {
                        continue;
                    }
                    for &of in fl {
                        if of == incident[ci] {
                            continue;
                        }
                        if let Some(oi) = incident.iter().position(|&x| x == of)
                            && comp[oi] < 0
                        {
                            comp[oi] = n_comp;
                            stack.push(oi);
                        }
                    }
                }
            }
            n_comp += 1;
        }
        // Angle-weighted normal per wedge.
        for (i, &fi) in incident.iter().enumerate() {
            let w = comp[i] as u32;
            let f = faces[fi];
            let k = f.v.iter().position(|&x| x == v).unwrap();
            let e1 = unit(sub(pos[f.v[(k + 1) % 3] as usize], pos[v as usize]));
            let e2 = unit(sub(pos[f.v[(k + 2) % 3] as usize], pos[v as usize]));
            let ang = dot(e1, e2).clamp(-1.0, 1.0).acos();
            let n = scale(fns[fi], ang.max(1e-6));
            let wedge_list = &mut wedge_nrm[v as usize];
            match wedge_list.iter_mut().find(|(id, _)| *id == w) {
                Some((_, acc)) => *acc = add(*acc, n),
                None => wedge_list.push((w, n)),
            }
        }
        for (i, &fi) in incident.iter().enumerate() {
            let k = faces[fi].v.iter().position(|&x| x == v).unwrap();
            face_wedge[fi][k] = comp[i] as u32;
        }
    }
    // Emit: one vertex per (position, wedge, color) — hard material
    // borders stay hard, smooth regions share one normal.
    let mut verts: Vec<MeshVert> = Vec::new();
    let mut vmap: BTreeMap<(u32, u32, [u8; 4]), u32> = BTreeMap::new();
    let mut indices: Vec<u32> = Vec::with_capacity(faces.len() * 3);
    for (fi, f) in faces.iter().enumerate() {
        for (k, &v) in f.v.iter().enumerate() {
            let w = face_wedge[fi][k];
            let key = (v, w, f.col);
            let idx = *vmap.entry(key).or_insert_with(|| {
                let n = wedge_nrm[v as usize]
                    .iter()
                    .find(|(id, _)| *id == w)
                    .map(|(_, n)| *n)
                    .unwrap_or([0.0, 0.0, 1.0]);
                let n = unit(n);
                // Cavity AO: the one-ring centroid of a concave vertex
                // sits along its normal (the surface cups around it) —
                // darken by the dip. Convex verts sit at/below the
                // ring → no change.
                let c = vnbrs[v as usize]
                    .iter()
                    .fold([0.0; 3], |acc, &u| add(acc, pos[u as usize]));
                let cnt = vnbrs[v as usize].len().max(1) as f64;
                let dip = dot(n, unit(sub(scale(c, 1.0 / cnt), pos[v as usize]))).max(0.0);
                let ao = (1.0 - 0.55 * dip.min(1.0)).clamp(0.55, 1.0);
                let t = f64::from(tint[v as usize]);
                let lit = |c: u8| -> u8 { (f64::from(c) * t).round().clamp(0.0, 255.0) as u8 };
                verts.push(MeshVert {
                    pos: [
                        pos[v as usize][0] as f32,
                        pos[v as usize][1] as f32,
                        pos[v as usize][2] as f32,
                    ],
                    nrm: [n[0] as f32, n[1] as f32, n[2] as f32],
                    col: [lit(f.col[0]), lit(f.col[1]), lit(f.col[2]), f.col[3]],
                    mat: mat_params(f.col),
                    ao: (ao * 255.0).round() as u8,
                    pad: 0,
                });
                (verts.len() - 1) as u32
            });
            indices.push(idx);
        }
    }
    MeshEntry { verts, indices }
}

// The parts are procedurally authored in `parts/` — no model files
// load here anymore. (The Kenney GLB path was retired: stylized
// cartoon primitives cannot be baked into real hardware.)

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

/// round(acc/cnt) without float — `floor((2a + n) / 2n)`.
fn div_round(acc: u32, cnt: u32) -> u8 {
    ((acc * 2 + cnt) / (cnt * 2)) as u8
}

/// 2× box downsample. Rounds to nearest (the old `as u8` cast floored
/// every level, biasing each mip ≤1 LSB dark per channel). `normal`
/// marks tangent-space normal maps: their RGB channels are averaged
/// in signed space and RENORMALIZED to unit length — a plain box
/// mean of unit vectors is strictly sub-unit, so un-renormalized
/// mips flattened the surface slope with distance (C19bake-F1).
fn downsample(level: &[u8], w: usize, h: usize, normal: bool) -> (Vec<u8>, usize, usize) {
    let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
    let mut out = vec![0u8; nw * nh * 4];
    for y in 0..nh {
        for x in 0..nw {
            let o = (y * nw + x) * 4;
            if normal {
                let mut v = [0.0f64; 3];
                let mut acc_a = 0u32;
                let mut cnt = 0u32;
                for dy in 0..2 {
                    for dx in 0..2 {
                        let (sx, sy) = (2 * x + dx, 2 * y + dy);
                        if sx < w && sy < h {
                            let p = (sy * w + sx) * 4;
                            for c in 0..3 {
                                v[c] += f64::from(level[p + c]) / 255.0 * 2.0 - 1.0;
                            }
                            acc_a += level[p + 3] as u32;
                            cnt += 1;
                        }
                    }
                }
                let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
                // Degenerate (opposing normals cancel) → up vector.
                let s = if l > 1e-6 { 1.0 / l } else { 0.0 };
                for c in 0..3 {
                    let n = v[c] * s;
                    let enc = if l > 1e-6 { n } else { [0.0, 0.0, 1.0][c] };
                    out[o + c] = ((enc * 0.5 + 0.5) * 255.0).round().clamp(0.0, 255.0) as u8;
                }
                out[o + 3] = div_round(acc_a, cnt);
            } else {
                let mut acc = [0u32; 4];
                let mut cnt = 0u32;
                for dy in 0..2 {
                    for dx in 0..2 {
                        let (sx, sy) = (2 * x + dx, 2 * y + dy);
                        if sx < w && sy < h {
                            let p = (sy * w + sx) * 4;
                            for c in 0..4 {
                                acc[c] += level[p + c] as u32;
                            }
                            cnt += 1;
                        }
                    }
                }
                for c in 0..4 {
                    out[o + c] = div_round(acc[c], cnt);
                }
            }
        }
    }
    (out, nw, nh)
}

/// Build the mip-major payload + per-layer means for an image stack
/// (`layers` same-sized RGBA8 tiles). `normal` marks normal maps —
/// their mip chain renormalizes (see `downsample`); the means row is
/// the albedo normalization factor the shader divides detail by
/// (terrain.wgsl `u.class_means`), NOT a normal-map value.
fn image_entry(w: u16, h: u16, level0_layers: Vec<Vec<u8>>, normal: bool) -> ImageEntry {
    let layers = level0_layers.len() as u8;
    let means: Vec<[u8; 4]> = level0_layers
        .iter()
        .map(|l| {
            let n = (l.len() as u64 / 4).max(1);
            let mut acc = [0u64; 4];
            for px in l.chunks_exact(4) {
                for c in 0..4 {
                    acc[c] += px[c] as u64;
                }
            }
            // Round, not floor — the shader divides by this.
            [0, 1, 2, 3].map(|c| ((acc[c] * 2 + n) / (n * 2)) as u8)
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
            next.push(downsample(layer, cw, ch, normal).0);
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

    // --- rocket part models (procedurally authored — see parts.rs) ---
    // Both packs ASSEMBLE fully before either writes: the old order
    // committed parts.csar before the terrain sources decoded, so a
    // corrupt jpg left a fresh parts pack beside a stale terrain pack
    // — silently mixed generations embedded by include_bytes
    // (C19bake-F3).
    let entries: Vec<(&str, Entry)> = parts::build_all()
        .into_iter()
        .map(|(k, m)| (k, Entry::Mesh(m)))
        .collect();
    let parts_bytes = assemble_csar(&entries);

    // --- terrain textures (ambientCG CC0) -----------------------------------
    // The night sky is NOT baked: `terrain.wgsl`'s fs_sky synthesizes a
    // procedural shell-lattice starfield — a baked equirect star map
    // blurred catalogue stars into speckle noise and smeared at the
    // poles, so the image was removed rather than replaced.
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

    let entries: Vec<(&str, Entry)> = vec![
        // layer order: 0 grass, 1 rock, 2 sand, 3 snow — terrain.wgsl keys it
        (
            "albedo",
            Entry::Image(image_entry(T as u16, T as u16, alb_layers, false)),
        ),
        (
            "normal",
            Entry::Image(image_entry(T as u16, T as u16, nrm_layers, true)),
        ),
        (
            "water_nrm",
            Entry::Image(image_entry(T as u16, T as u16, vec![water], true)),
        ),
    ];
    let terrain_bytes = assemble_csar(&entries);

    // Both packs assembled — publish each atomically.
    atomic_write(&out.join("parts.csar"), &parts_bytes);
    atomic_write(&out.join("terrain.csar"), &terrain_bytes);
}
