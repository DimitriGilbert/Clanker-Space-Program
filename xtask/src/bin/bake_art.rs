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
//!                   layers×4 u8 mean-RGBA (per-layer average),
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
//! ## Part-model quality pass (v2)
//!
//! The Kenney Space Kit ships flat-shaded, faceted models (~50–340
//! triangles per part): every face corner carries a duplicated vertex
//! with a face normal, so the baked meshes read as hard-edged low-poly
//! solids. The bake upgrades them instead of shipping that:
//!
//! 1. **Weld** — all primitives merge on quantized position, so the
//!    real topology (closed solids, real dihedral angles) is visible;
//!    material borders stay as per-face colors, not as vertex splits.
//! 2. **Edge shading** — every sharp edge is classified convex vs
//!    concave; convex edges catch a light tint, concave seams darken —
//!    the cheap "panel gap + edge wear" read that keeps flat materials
//!    from looking untextured.
//! 3. **Crease-aware Loop subdivision** — 1–2 iterations, smooth edges
//!    (< ~53° dihedral) take the Loop limit surface while hard edges
//!    (≥ ~53°, boundaries, non-manifold seams) take the sharp-crease
//!    rules: 45° bevels and 8-gon barrels round into true curved
//!    surfaces while machined 90° corners stay crisp. The low-poly
//!    silhouette is gone at bake time, so the runtime cost stays a
//!    plain indexed mesh.
//! 4. **Split normals** — vertex normals rebuilt per smoothing wedge
//!    (angle-weighted, split across creases): smooth regions shade
//!    continuously, hard edges keep their flat facets.
//! 5. **Material params** — each source material maps to a specular
//!    strength/exponent pair (machined dark metal, painted hull,
//!    glass, accent paint) and a cavity AO term is baked per vertex —
//!    the shader reads them per pixel.
//!
//! The decoder is `csp-view`'s `art` module; the format is deliberately
//! frozen-independent — bump `version` on any layout change.

#![allow(clippy::unwrap_used, clippy::expect_used)] // bake tooling: loud failure is the honest path
#![allow(clippy::too_many_lines)]

use std::collections::BTreeMap;
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

fn write_csar(path: &Path, entries: &[(&str, Entry)]) {
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

// ---------------------------------------------------------------------------
// Part-model quality pass — weld → orient → edge-shade → Loop-subdivide →
// split normals. All mesh math in f64 at bake; the pack stores f32.
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
/// under this cosine (≈53°) are a crease — Loop sharp-rules it and the
/// normal rebuild splits it. Kenney's 45° bevels stay under the bar
/// (they smooth), 60°+ machined corners sit over it (they stay crisp).
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

/// The third vertex of face `fi` — the corner opposite edge `(a, b)`
/// (order-free; only the sum enters the Loop odd-vertex rule).
fn third(faces: &[Face], fi: usize, a: u32, b: u32) -> u32 {
    faces[fi]
        .v
        .iter()
        .copied()
        .find(|&v| v != a && v != b)
        .unwrap_or(a)
}

/// Convex/concave edge-shading: each hard 2-face edge darkens or
/// brightens its endpoint vertices — concave seams read as panel gaps,
/// convex edges catch a faint edge light. Applied to the coarse mesh
/// BEFORE subdivision so the falloff spreads smoothly through the
/// subdivided descendants.
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

/// One crease-aware Loop subdivision step — faces ×4. Smooth edges get
/// the Loop limit rules (bevels and polygonal barrels round into real
/// curves); crease/boundary/non-manifold edges get the sharp rules so
/// hard corners hold. `tint` tracks positions: edge midpoints average
/// their endpoints' edge-shade factors.
fn subdivide(pos: &mut Vec<[f64; 3]>, faces: &mut Vec<Face>, tint: &mut Vec<f32>) {
    let fns = face_normals(pos, faces);
    let edges = edge_faces(faces);
    let n_v = pos.len();
    // Vertex adjacency split by edge class — the even-vertex rule needs
    // both the full one-ring (smooth rule) and just the crease
    // neighbours (crease rule).
    let mut nbrs: Vec<Vec<u32>> = vec![Vec::new(); n_v];
    let mut crease_nbrs: Vec<Vec<u32>> = vec![Vec::new(); n_v];
    for ((a, b), fl) in &edges {
        nbrs[*a as usize].push(*b);
        nbrs[*b as usize].push(*a);
        if edge_is_crease(fl, &fns) {
            crease_nbrs[*a as usize].push(*b);
            crease_nbrs[*b as usize].push(*a);
        }
    }
    // Odd vertices (one per edge).
    let mut odd: BTreeMap<(u32, u32), u32> = BTreeMap::new();
    for ((a, b), fl) in &edges {
        let (pa, pb) = (pos[*a as usize], pos[*b as usize]);
        let p = if edge_is_crease(fl, &fns) {
            scale(add(pa, pb), 0.5)
        } else {
            let c = pos[third(faces, fl[0], *a, *b) as usize];
            let d = pos[third(faces, fl[1], *a, *b) as usize];
            add(scale(add(pa, pb), 0.375), scale(add(c, d), 0.125))
        };
        odd.insert((*a, *b), pos.len() as u32);
        pos.push(p);
        tint.push((tint[*a as usize] + tint[*b as usize]) * 0.5);
    }
    // Even vertices: standard Loop — smooth interior verts take the
    // β-rule, verts on exactly two crease edges take the crease-curve
    // rule, corners/ambiguous verts are pinned.
    let even: Vec<[f64; 3]> = (0..n_v)
        .map(|v| {
            let cn = &crease_nbrs[v];
            if cn.len() == 2 {
                scale(
                    add(
                        add(pos[cn[0] as usize], pos[cn[1] as usize]),
                        scale(pos[v], 6.0),
                    ),
                    0.125,
                )
            } else if cn.is_empty() {
                let nb = &nbrs[v];
                let n = nb.len();
                if n < 3 {
                    return pos[v];
                }
                let beta = if n == 3 {
                    3.0 / 16.0
                } else {
                    3.0 / (8.0 * n as f64)
                };
                let sum = nb
                    .iter()
                    .fold([0.0; 3], |acc, &u| add(acc, pos[u as usize]));
                add(scale(pos[v], 1.0 - n as f64 * beta), scale(sum, beta))
            } else {
                pos[v]
            }
        })
        .collect();
    pos[..n_v].copy_from_slice(&even);
    // Rebuild: each face → 4 children, winding preserved.
    let mut out = Vec::with_capacity(faces.len() * 4);
    for f in faces.iter() {
        let [a, b, c] = f.v;
        let eab = odd[&(a.min(b), a.max(b))];
        let ebc = odd[&(b.min(c), b.max(c))];
        let eca = odd[&(c.min(a), c.max(a))];
        for v in [[a, eab, eca], [eab, b, ebc], [eab, ebc, eca], [eca, ebc, c]] {
            out.push(Face { v, col: f.col });
        }
    }
    *faces = out;
}

/// `(spec_strength·255, spec_exponent)` for a source material color —
/// the per-material shininess the shader mixes per pixel. Dark
/// blue-steel reads as machined metal (tight, strong highlight), the
/// whites as glass/ceramic (tightest), the orange accent and hull
/// greys as paint and brushed metal.
fn mat_params(col: [u8; 4]) -> [u8; 2] {
    let (r, g, b) = (f64::from(col[0]), f64::from(col[1]), f64::from(col[2]));
    let lum = (0.3 * r + 0.5 * g + 0.2 * b) / 255.0;
    if r > 200.0 && g < 190.0 && b < 120.0 {
        [115, 40] // accent paint
    } else if lum < 0.38 {
        [150, 72] // machined dark metal (nozzle, shadowed hardware)
    } else if lum > 0.92 {
        [165, 90] // near-white: pod glazing / ceramic
    } else if lum > 0.72 {
        [100, 44] // painted hull
    } else {
        [125, 56] // brushed mid metal
    }
}

/// Rebuild the emitted mesh from the subdivided working mesh: smooth
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

/// Load a GLB: weld all primitives on quantized position (f32 sources,
/// 1e-6 grid — material borders merge into the shared topology),
/// orient every face outward through its authored normals, then run
/// the subdivision/normal/material quality pass. glTF y-up converts
/// to the game's z-up part frame `(x,y,z) -> (x,-z,y)`.
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
    let accs: Vec<Value> = json["accessors"].as_array().cloned().unwrap_or_default();
    let mats: Vec<Value> = json["materials"].as_array().cloned().unwrap_or_default();

    let mut vmap: BTreeMap<[i64; 3], u32> = BTreeMap::new();
    let mut pos: Vec<[f64; 3]> = Vec::new();
    let mut faces: Vec<Face> = Vec::new();
    let mut weld = |p: [f64; 3]| -> u32 {
        let key = [
            (p[0] * 1e6).round() as i64,
            (p[1] * 1e6).round() as i64,
            (p[2] * 1e6).round() as i64,
        ];
        *vmap.entry(key).or_insert_with(|| {
            pos.push(p);
            (pos.len() - 1) as u32
        })
    };
    for mesh in json["meshes"].as_array().unwrap() {
        for prim in mesh["primitives"].as_array().unwrap() {
            let mode = prim["mode"].as_u64().unwrap_or(4);
            assert_eq!(mode, 4, "non-triangle primitive");
            let attrs = &prim["attributes"];
            let prim_pos = read_f32v3(
                bin,
                &views,
                &accs[attrs["POSITION"].as_u64().unwrap() as usize],
            );
            let prim_nrm = read_f32v3(
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
            let idx: Vec<u32> = match prim.get("indices").and_then(|i| i.as_u64()) {
                Some(a) => read_indices(bin, &views, &accs[a as usize]),
                None => (0..prim_pos.len() as u32).collect(),
            };
            for t in idx.chunks_exact(3) {
                let [ia, ib, ic] = [t[0], t[1], t[2]].map(|i| i as usize);
                if ia == ib || ib == ic || ic == ia {
                    continue;
                }
                let (pa, pb, pc) = (prim_pos[ia], prim_pos[ib], prim_pos[ic]);
                // y-up → z-up conversion lands the model "up" on the
                // stack axis; positions weld on the CONVERTED frame.
                let cv = |p: [f32; 3]| [f64::from(p[0]), f64::from(-p[2]), f64::from(p[1])];
                // Orient outward: the authored corner normals are the
                // kit's ground truth — a face whose geometric normal
                // fights them gets its winding flipped so every
                // downstream normal points out.
                let an = {
                    let n = unit(cross(sub(cv(pb), cv(pa)), sub(cv(pc), cv(pa))));
                    let authored = unit(add(
                        add(
                            [
                                f64::from(prim_nrm[ia][0]),
                                f64::from(-prim_nrm[ia][2]),
                                f64::from(prim_nrm[ia][1]),
                            ],
                            [
                                f64::from(prim_nrm[ib][0]),
                                f64::from(-prim_nrm[ib][2]),
                                f64::from(prim_nrm[ib][1]),
                            ],
                        ),
                        [
                            f64::from(prim_nrm[ic][0]),
                            f64::from(-prim_nrm[ic][2]),
                            f64::from(prim_nrm[ic][1]),
                        ],
                    ));
                    if dot(n, authored) < 0.0 {
                        [cv(pa), cv(pc), cv(pb)]
                    } else {
                        [cv(pa), cv(pb), cv(pc)]
                    }
                };
                let (va, vb, vc) = (weld(an[0]), weld(an[1]), weld(an[2]));
                if va == vb || vb == vc || vc == va {
                    continue;
                }
                faces.push(Face {
                    v: [va, vb, vc],
                    col,
                });
            }
        }
    }
    assert!(!faces.is_empty(), "{}: no triangles", path.display());
    let mut tint = vec![1.0f32; pos.len()];
    edge_shade(&pos, &faces, &mut tint);
    // Up to two Loop iterations, bounded — subdividing stops when the
    // next step would pass ~2.4k tris so the big models don't explode.
    for _ in 0..2 {
        if faces.len() * 4 > 2400 {
            break;
        }
        subdivide(&mut pos, &mut faces, &mut tint);
    }
    finish(&pos, &faces, &tint)
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

/// The sky-dome source must be a *pure* star field. The first pass here
/// baked Poly Haven's `lakeside_night` HDRI — a landscape panorama whose
/// lower hemisphere is trees/water/shore and whose upper half blows out
/// to a white twilight glow under tone-mapping; mapped onto the sky
/// sphere it read as "the inside of a planet". The replacement is NASA
/// SVS's Deep Star Map (svs.gsfc.nasa.gov/3895): a plate carrée
/// (equirect) all-sky render of ~100 M catalogue stars + the Milky Way
/// — dark sky in EVERY direction, no landscape anywhere.
fn load_stars(path: &Path, w: u32, h: u32) -> Vec<u8> {
    load_rgba8(path, w, h)
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

    // --- terrain/sky textures (ambientCG CC0 + NASA SVS public-domain) ------
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
    // 2048×1024 keeps catalogue stars ~0.2°/texel — points, not smears.
    let stars = load_stars(&src.join("starmap_4k.jpg"), 2048, 1024);

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
        ("stars", Entry::Image(image_entry(2048, 1024, vec![stars]))),
    ];
    write_csar(&out.join("terrain.csar"), &entries);
}
