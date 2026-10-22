//! §3.0.1 biome maps: the RON legend schema plus a minimal, dependency-free
//! inspector for the palette-indexed PNG the legend refers to.
//!
//! Biomes are authored, versioned, low-resolution index maps — equirect,
//! ≤2048×1024, 8-bit palette PNG at `data/systems/<sys>/<body>/biomes.png`
//! plus a `biomes.ron` legend. They are deliberately decoupled from terrain
//! noise (science keys on `(body, biome, situation)`; §3.0.1), and the
//! §3.1 data-set hash covers them — [`crate::datahash`] treats the PNG
//! bytes as data. [`inspect_indexed_png`] validates headers; the pixel
//! payload only decodes for *presentation* consumers ([`decode_indexed_png`]
//! — the R2 terrain palette), never into the sim.

use serde::{Deserialize, Serialize};

use crate::ids::{BiomeId, LocKey};

/// §3.0.1's hard cap on biome-map dimensions (equirect pixels).
pub const BIOME_MAP_MAX_W: u32 = 2048;
/// See [`BIOME_MAP_MAX_W`].
pub const BIOME_MAP_MAX_H: u32 = 1024;

/// One legend row: pixel palette index → biome identity.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BiomeLegendEntry {
    /// Palette index used in the PNG (0-based).
    pub index: u8,
    /// Stable biome ID — part of the `(body, biome, situation)` science
    /// key (§3.2), so it changes only via deliberate data-set change.
    pub id: BiomeId,
    /// Loc key for the biome's display name.
    pub name: LocKey,
}

/// `biomes.ron` — the versioned legend for a body's `biomes.png`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BiomeLegend {
    /// First field of every data file (§3.1). Must equal
    /// [`crate::system::SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Legend rows. Indices must be unique and within the PNG's palette.
    pub entries: Vec<BiomeLegendEntry>,
}

/// What [`inspect_indexed_png`] recovered from a biome map's headers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PngInfo {
    /// Image width (px).
    pub width: u32,
    /// Image height (px).
    pub height: u32,
    /// Number of RGB triples in the `PLTE` chunk — the palette size the
    /// pixel indices address.
    pub palette_len: usize,
}

/// Parse just enough PNG structure to validate a §3.0.1 biome map:
/// signature, `IHDR` (must be 8-bit palette — colour type 3), `PLTE`
/// (palette size), `IEND`. Returns `Err(&'static str)` with a stable
/// reason token the validator wraps into [`crate::ValidationError`]s.
///
/// This is header validation, not decoding — pixel data is never
/// decompressed here, so no zlib dependency is needed.
pub fn inspect_indexed_png(bytes: &[u8]) -> Result<PngInfo, &'static str> {
    const SIG: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
    if bytes.len() < SIG.len() || &bytes[..8] != SIG {
        return Err("not a PNG (bad signature)");
    }
    let mut pos = 8usize;
    let mut ihdr: Option<(u32, u32)> = None;
    let mut palette_len: Option<usize> = None;
    while pos + 12 <= bytes.len() {
        let len =
            u32::from_be_bytes(bytes[pos..pos + 4].try_into().map_err(|_| "truncated")?) as usize;
        let kind = &bytes[pos + 4..pos + 8];
        let data_start = pos + 8;
        let data_end = data_start.checked_add(len).ok_or("chunk length overflow")?;
        // +4 for the trailing CRC.
        if data_end.checked_add(4).is_none_or(|e| e > bytes.len()) {
            return Err("truncated chunk");
        }
        match kind {
            b"IHDR" => {
                if ihdr.is_some() {
                    return Err("duplicate IHDR");
                }
                if len != 13 {
                    return Err("IHDR must be 13 bytes");
                }
                let w = u32::from_be_bytes(
                    bytes[data_start..data_start + 4]
                        .try_into()
                        .map_err(|_| "truncated")?,
                );
                let h = u32::from_be_bytes(
                    bytes[data_start + 4..data_start + 8]
                        .try_into()
                        .map_err(|_| "truncated")?,
                );
                let bit_depth = bytes[data_start + 8];
                let colour_type = bytes[data_start + 9];
                if w == 0 || h == 0 {
                    return Err("zero-dimension image");
                }
                // §3.0.1 mandates palette-indexed maps.
                if colour_type != 3 {
                    return Err("not palette-indexed (colour type != 3)");
                }
                if bit_depth != 8 {
                    return Err("palette bit depth must be 8");
                }
                ihdr = Some((w, h));
            }
            b"PLTE" => {
                if len == 0 || !len.is_multiple_of(3) {
                    return Err("malformed PLTE");
                }
                palette_len = Some(len / 3);
            }
            b"IEND" => {
                let (width, height) = ihdr.ok_or("missing IHDR")?;
                let palette_len = palette_len.ok_or("missing PLTE")?;
                return Ok(PngInfo {
                    width,
                    height,
                    palette_len,
                });
            }
            _ => {}
        }
        pos = data_end + 4;
    }
    Err("missing IEND")
}

/// A decoded §3.0.1 biome map: the header facts plus one palette index
/// per pixel, row-major top-to-bottom (PNG scanline order = equirect
/// north-to-south).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexedPng {
    /// Header facts (same fields [`inspect_indexed_png`] recovers).
    pub info: PngInfo,
    /// `width * height` palette indices, row-major.
    pub indices: Vec<u8>,
}

/// Fully decode a §3.0.1 biome map: header checks identical to
/// [`inspect_indexed_png`], then zlib-inflate the `IDAT` payload and
/// reverse the PNG scanline filters. Bit depth 8 / colour type 3 means
/// one index byte per pixel — no packing or interlacing to undo (the
/// header checks reject anything else).
///
/// The sim never reads pixels (biome stability is authored, §3.0.1);
/// this exists for presentation consumers — the R2 terrain palette.
/// Deterministic: inflate + unfilter are exact integer ops.
pub fn decode_indexed_png(bytes: &[u8]) -> Result<IndexedPng, &'static str> {
    const SIG: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
    if bytes.len() < SIG.len() || &bytes[..8] != SIG {
        return Err("not a PNG (bad signature)");
    }
    let mut pos = 8usize;
    let mut ihdr: Option<(u32, u32)> = None;
    let mut palette_len: Option<usize> = None;
    let mut idat = Vec::new();
    let (width, height) = loop {
        if pos + 12 > bytes.len() {
            return Err("missing IEND");
        }
        let len =
            u32::from_be_bytes(bytes[pos..pos + 4].try_into().map_err(|_| "truncated")?) as usize;
        let kind = &bytes[pos + 4..pos + 8];
        let data_start = pos + 8;
        let data_end = data_start.checked_add(len).ok_or("chunk length overflow")?;
        if data_end.checked_add(4).is_none_or(|e| e > bytes.len()) {
            return Err("truncated chunk");
        }
        match kind {
            b"IHDR" => {
                if ihdr.is_some() {
                    return Err("duplicate IHDR");
                }
                if len != 13 {
                    return Err("IHDR must be 13 bytes");
                }
                let w = u32::from_be_bytes(
                    bytes[data_start..data_start + 4]
                        .try_into()
                        .map_err(|_| "truncated")?,
                );
                let h = u32::from_be_bytes(
                    bytes[data_start + 4..data_start + 8]
                        .try_into()
                        .map_err(|_| "truncated")?,
                );
                if w == 0 || h == 0 {
                    return Err("zero-dimension image");
                }
                if bytes[data_start + 9] != 3 {
                    return Err("not palette-indexed (colour type != 3)");
                }
                if bytes[data_start + 8] != 8 {
                    return Err("palette bit depth must be 8");
                }
                if bytes[data_start + 12] != 0 {
                    return Err("interlaced maps not supported");
                }
                ihdr = Some((w, h));
            }
            b"PLTE" => {
                if len == 0 || !len.is_multiple_of(3) {
                    return Err("malformed PLTE");
                }
                palette_len = Some(len / 3);
            }
            b"IDAT" => idat.extend_from_slice(&bytes[data_start..data_end]),
            b"IEND" => break ihdr.ok_or("missing IHDR")?,
            _ => {}
        }
        pos = data_end + 4;
    };
    let palette_len = palette_len.ok_or("missing PLTE")?;
    let (w, h) = (width as usize, height as usize);
    let raw = miniz_oxide::inflate::decompress_to_vec_zlib(&idat)
        .map_err(|_| "IDAT zlib inflate failed")?;
    let indices = unfilter_scanlines(&raw, w, h)?;
    if indices.iter().any(|&i| i as usize >= palette_len) {
        return Err("pixel index outside palette");
    }
    Ok(IndexedPng {
        info: PngInfo {
            width,
            height,
            palette_len,
        },
        indices,
    })
}

/// Reverse the PNG scanline filters over a zlib-inflated payload —
/// bit depth 8 ⇒ one byte per pixel, so `bpp = 1` throughout.
fn unfilter_scanlines(raw: &[u8], w: usize, h: usize) -> Result<Vec<u8>, &'static str> {
    let stride = w + 1; // filter byte + 1 byte/px
    if raw.len() != stride * h {
        return Err("inflated size mismatch");
    }
    let mut indices = vec![0u8; w * h];
    for y in 0..h {
        let row = &raw[y * stride..y * stride + stride];
        let filter = row[0];
        let line = &row[1..];
        let (done, rest) = indices.split_at_mut(y * w);
        // The already-decoded previous row (`&[]` on row 0 — PNG treats
        // "above the image" as zeroes).
        let prev: &[u8] = if y == 0 { &[] } else { &done[(y - 1) * w..] };
        let out = &mut rest[..w];
        match filter {
            0 => out.copy_from_slice(line),
            1 => {
                for x in 0..w {
                    let a = if x == 0 { 0 } else { out[x - 1] };
                    out[x] = line[x].wrapping_add(a);
                }
            }
            2 => {
                for x in 0..w {
                    out[x] = line[x].wrapping_add(prev.get(x).copied().unwrap_or(0));
                }
            }
            3 => {
                for x in 0..w {
                    let a = u16::from(if x == 0 { 0 } else { out[x - 1] });
                    let b = u16::from(prev.get(x).copied().unwrap_or(0));
                    out[x] = line[x].wrapping_add(((a + b) / 2) as u8);
                }
            }
            4 => {
                for x in 0..w {
                    let a = i32::from(if x == 0 { 0 } else { out[x - 1] });
                    let b = i32::from(prev.get(x).copied().unwrap_or(0));
                    let c = i32::from(if x == 0 {
                        0
                    } else {
                        prev.get(x - 1).copied().unwrap_or(0)
                    });
                    let p = a + b - c;
                    let (pa, pb, pc) = ((p - a).abs(), (p - b).abs(), (p - c).abs());
                    let pr = if pa <= pb && pa <= pc {
                        a
                    } else if pb <= pc {
                        b
                    } else {
                        c
                    };
                    out[x] = line[x].wrapping_add(pr as u8);
                }
            }
            _ => return Err("unknown scanline filter"),
        }
    }
    Ok(indices)
}

#[cfg(test)]
// Tests signal failure by panicking — §2.13's no-panic rule covers
// shipping fallible paths, not test code.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// The authored home-system biome maps decode cleanly and agree
    /// with what the header inspector reports (§3.0.1 shape).
    #[test]
    fn authored_biome_maps_decode() {
        for (png, expect) in [
            (
                include_bytes!("../../../data/systems/home/terrin/biomes.png").as_slice(),
                7usize,
            ),
            (
                include_bytes!("../../../data/systems/home/rask/biomes.png").as_slice(),
                4,
            ),
            (
                include_bytes!("../../../data/systems/home/duva/biomes.png").as_slice(),
                4,
            ),
        ] {
            let img = decode_indexed_png(png).unwrap();
            let head = inspect_indexed_png(png).unwrap();
            assert_eq!(img.info, head);
            assert_eq!(
                img.indices.len(),
                (img.info.width * img.info.height) as usize
            );
            assert_eq!(img.info.palette_len, expect);
        }
    }

    /// Corrupt payloads fail honestly, never panic.
    #[test]
    fn decode_rejects_garbage() {
        assert!(decode_indexed_png(b"").is_err());
        assert!(decode_indexed_png(b"not a png at all").is_err());
        let mut truncated = include_bytes!("../../../data/systems/home/terrin/biomes.png").to_vec();
        truncated.truncate(40);
        assert!(decode_indexed_png(&truncated).is_err());
    }
}
