//! §3.0.1 biome maps: the RON legend schema plus a minimal, dependency-free
//! inspector for the palette-indexed PNG the legend refers to.
//!
//! Biomes are authored, versioned, low-resolution index maps — equirect,
//! ≤2048×1024, 8-bit palette PNG at `data/systems/<sys>/<body>/biomes.png`
//! plus a `biomes.ron` legend. They are deliberately decoupled from terrain
//! noise (science keys on `(body, biome, situation)`; §3.0.1), and the
//! §3.1 data-set hash covers them — [`crate::datahash`] treats the PNG
//! bytes as data, which is why this module inspects but never *decodes*
//! pixels into the sim.

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
