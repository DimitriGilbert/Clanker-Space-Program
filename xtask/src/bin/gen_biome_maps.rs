#![forbid(unsafe_code)]

//! `cargo run -p xtask --bin gen_biome_maps [--check]` — regenerate the
//! §3.0.1 biome maps under `data/systems/home/*/biomes.png`.
//!
//! The PNGs are **hand-authored fiction** (§3.0: "every number is
//! hand-authored"): the region rules below ARE the authored content —
//! committed PNGs are byte-identical output of this program, and the
//! legend indices must match each body's `biomes.ron`. `--check`
//! regenerates in memory and compares, so CI can prove the committed
//! files are what this program produces.
//!
//! The encoder is deliberately dependency-free: 8-bit palette PNGs whose
//! IDAT is a zlib stream of *stored* (uncompressed) DEFLATE blocks — a
//! valid, deterministic, spec-legal encoding.

use std::path::{Path, PathBuf};
use std::process::exit;

/// §3.0.1 equirect dimensions — well under the 2048×1024 cap.
const W: u32 = 64;
const H: u32 = 32;

/// One authored map: output path (repo-relative under the system dir)
/// plus the pixel rule.
struct MapSpec {
    /// Body directory name under `data/systems/home/`.
    body: &'static str,
    /// RGB palette — MUST be indexed to match `biomes.ron` indices.
    palette: &'static [[u8; 3]],
    /// Pixel rule: (longitude°, latitude°) at pixel centre -> index.
    pixel: fn(f64, f64) -> u8,
}

/// Whether the ellipse `((lon-cx)/rx)² + ((lat-cy)/ry)² ≤ 1` covers the
/// point — with longitude wrapped so `cx = 350` still neighbours `lon = 5`.
fn in_ellipse(lon: f64, lat: f64, cx: f64, cy: f64, rx: f64, ry: f64) -> bool {
    let mut dx = (lon - cx).abs();
    if dx > 180.0 {
        dx = 360.0 - dx;
    }
    let dy = lat - cy;
    (dx / rx) * (dx / rx) + (dy / ry) * (dy / ry) <= 1.0
}

/// Terrin (legend: 0 ocean, 1 grassland, 2 forest, 3 desert, 4 mountains,
/// 5 tundra, 6 ice_caps). One big mid-latitude ocean basin, a southern
// sea and a northern sea, a mountain spine, latitude bands ashore.
fn terrin(lon: f64, lat: f64) -> u8 {
    if in_ellipse(lon, lat, 215.0, -5.0, 85.0, 50.0)
        || in_ellipse(lon, lat, 45.0, -45.0, 55.0, 20.0)
        || in_ellipse(lon, lat, 100.0, 32.0, 40.0, 14.0)
    {
        return 0; // ocean
    }
    let a = lat.abs();
    if a > 72.0 {
        return 6; // ice caps
    }
    if a > 55.0 {
        return 5; // tundra
    }
    if in_ellipse(lon, lat, 340.0, 12.0, 25.0, 11.0)
        || in_ellipse(lon, lat, 330.0, -30.0, 15.0, 8.0)
    {
        return 4; // mountains
    }
    if (10.0..30.0).contains(&lat) {
        return 3; // subtropical desert belt
    }
    if (-45.0..-8.0).contains(&lat) {
        return 2; // forest
    }
    1 // grassland
}

/// Rask (legend: 0 maria, 1 highlands, 2 polar_deposits, 3 crater_fields).
/// Dark basaltic plains near the sub-Terrin point (lon 0), bright
/// highlands elsewhere, polar cold-trap deposits, a farside crater field.
fn rask(lon: f64, lat: f64) -> u8 {
    if lat.abs() > 70.0 {
        return 2; // polar deposits
    }
    if in_ellipse(lon, lat, 0.0, 5.0, 45.0, 30.0) || in_ellipse(lon, lat, 320.0, -25.0, 30.0, 18.0)
    {
        return 0; // maria
    }
    if in_ellipse(lon, lat, 175.0, 15.0, 55.0, 32.0) {
        return 3; // crater fields
    }
    1 // highlands
}

/// Duva (legend: 0 polar_ice, 1 dunes, 2 cratered_highlands,
/// 3 impact_basin). Thin air: polar caps, an equatorial dune sea, one
/// great basin, cratered highlands in between.
fn duva(lon: f64, lat: f64) -> u8 {
    if lat.abs() > 60.0 {
        return 0; // polar ice
    }
    if in_ellipse(lon, lat, 300.0, -20.0, 55.0, 28.0) {
        return 3; // impact basin
    }
    if lat.abs() < 25.0 {
        return 1; // dunes
    }
    2 // cratered highlands
}

const MAPS: &[MapSpec] = &[
    MapSpec {
        body: "terrin",
        palette: &[
            [30, 60, 140],   // 0 ocean
            [90, 160, 60],   // 1 grassland
            [30, 110, 40],   // 2 forest
            [210, 190, 120], // 3 desert
            [140, 120, 110], // 4 mountains
            [170, 170, 150], // 5 tundra
            [235, 240, 245], // 6 ice caps
        ],
        pixel: terrin,
    },
    MapSpec {
        body: "rask",
        palette: &[
            [90, 90, 95],    // 0 maria
            [150, 145, 135], // 1 highlands
            [200, 205, 215], // 2 polar deposits
            [120, 115, 110], // 3 crater fields
        ],
        pixel: rask,
    },
    MapSpec {
        body: "duva",
        palette: &[
            [200, 220, 235], // 0 polar ice
            [205, 170, 120], // 1 dunes
            [160, 140, 125], // 2 cratered highlands
            [120, 110, 120], // 3 impact basin
        ],
        pixel: duva,
    },
];

/// Rasterize a spec into W×H palette indices (row-major, top row = +90°).
fn rasterize(spec: &MapSpec) -> Vec<u8> {
    let mut px = Vec::with_capacity((W * H) as usize);
    for y in 0..H {
        // Pixel-centre coordinates: equirect, lat +90 (top) to -90.
        let lat = 90.0 - (f64::from(y) + 0.5) * (180.0 / f64::from(H));
        for x in 0..W {
            let lon = (f64::from(x) + 0.5) * (360.0 / f64::from(W));
            px.push((spec.pixel)(lon, lat));
        }
    }
    px
}

// ---- minimal deterministic PNG encoder --------------------------------
// CRC32 (PNG polynomial) and Adler-32, computed bitwise/table-free so the
// output depends on nothing outside this file.

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn adler32(bytes: &[u8]) -> u32 {
    let mut a = 1u32;
    let mut b = 0u32;
    for &byte in bytes {
        a = (a + u32::from(byte)) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

/// One PNG chunk: length + type + data + CRC32(type || data).
fn chunk(kind: &[u8; 4], data: &[u8], out: &mut Vec<u8>) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut c = Vec::with_capacity(4 + data.len());
    c.extend_from_slice(kind);
    c.extend_from_slice(data);
    out.extend_from_slice(&crc32(&c).to_be_bytes());
}

/// zlib wrap of `raw` as one stored-DEFLATE stream (header 0x78 0x01,
/// blocks of ≤65535 bytes with BFINAL on the last, Adler-32 trailer).
fn zlib_stored(raw: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    let mut chunks = raw.chunks(65535).peekable();
    if raw.is_empty() {
        out.extend_from_slice(&[0x01, 0, 0, 0xFF, 0xFF]);
    }
    while let Some(block) = chunks.next() {
        let last = chunks.peek().is_none();
        out.push(u8::from(last));
        let len = block.len() as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(block);
    }
    out.extend_from_slice(&adler32(raw).to_be_bytes());
    out
}

/// Encode `pixels` (row-major, length `w*h`) as an 8-bit palette PNG.
fn encode_indexed_png(w: u32, h: u32, palette: &[[u8; 3]], pixels: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 3, 0, 0, 0]); // depth 8, colour type 3 (palette)
    chunk(b"IHDR", &ihdr, &mut out);
    let mut plte = Vec::with_capacity(palette.len() * 3);
    for c in palette {
        plte.extend_from_slice(c);
    }
    chunk(b"PLTE", &plte, &mut out);
    // Scanlines, each prefixed with filter byte 0 (None).
    let mut raw = Vec::with_capacity((w as usize + 1) * h as usize);
    for row in pixels.chunks(w as usize) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    chunk(b"IDAT", &zlib_stored(&raw), &mut out);
    chunk(b"IEND", &[], &mut out);
    out
}

fn home_dir() -> PathBuf {
    // xtask sits at the repo root's `xtask/`.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default()
        .join("data/systems/home")
}

fn run(check: bool) -> Result<(), String> {
    let base = home_dir();
    let mut stale = Vec::new();
    for spec in MAPS {
        let path = base.join(spec.body).join("biomes.png");
        let bytes = encode_indexed_png(W, H, spec.palette, &rasterize(spec));
        if check {
            let on_disk = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            if on_disk != bytes {
                stale.push(path.display().to_string());
            }
        } else {
            std::fs::write(&path, &bytes).map_err(|e| format!("{}: {e}", path.display()))?;
            println!("wrote {}", path.display());
        }
    }
    if check && !stale.is_empty() {
        return Err(format!(
            "stale biome maps (run without --check to regenerate): {}",
            stale.join(", ")
        ));
    }
    Ok(())
}

fn main() {
    let check = std::env::args().skip(1).any(|a| a == "--check");
    if let Err(e) = run(check) {
        eprintln!("gen_biome_maps: {e}");
        exit(1);
    }
    if check {
        println!("gen_biome_maps: all committed PNGs match generator output");
    }
}
