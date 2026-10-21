//! The §3.1 **data-set hash** — BLAKE3 over `path_bytes || 0x00 ||
//! canonicalized_file_bytes`, ascending path order, for every file under
//! `data/` *except*:
//!
//! - everything under `data/test-fixtures/` (§8.3: frozen golden inputs,
//!   deliberately outside the shipping data set — they are hashed
//!   separately by `csp_bench::replay::fixture_hash`, the *fixture* hash
//!   recorded in the §7.6 artifact), and
//! - files under `data/art/` whose extension is in [`ART_EXTENSIONS`]
//!   (textures/audio are presentation assets, never read by the sim).
//!   The same extension **outside** `data/art/` **is** hashed — §3.1 is
//!   explicit that `*.png` counts for biome maps and concentration maps.
//!
//! Boundary, stated because two hashes share the pinned scheme:
//! `data_set_hash` here covers the *shipping* data set and is what
//! `BuiltPart::data_set_hash` / §7.7's drift warning key on;
//! `fixture_hash` in csp-bench covers one frozen scenario dir and lives
//! in the replay artifact. Same construction, disjoint scopes — changing
//! either scheme bumps [`DATA_HASH_VERSION`](crate::DATA_HASH_VERSION).
//!
//! Canonicalization (§3.1): UTF-8 text → LF line endings, trailing
//! whitespace stripped; non-UTF-8 bytes (PNG maps) hash verbatim.

use std::fmt;
use std::path::{Path, PathBuf};

/// Extensions treated as art assets *only when under `data/art/`*
/// (§3.1). Everything else under `data/` — including `*.png` biome and
/// concentration maps — is hashed.
pub const ART_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "tga", "webp", "ktx2", "ogg", "wav", "glb",
];

/// `data_set_hash` failures — plain io diagnostics, like the loader's.
#[derive(Debug)]
pub enum DataHashError {
    /// A directory entry or file could not be read.
    Io {
        /// The path that failed.
        path: PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
}

impl fmt::Display for DataHashError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(f, "io at {}: {source}", path.display()),
        }
    }
}

impl std::error::Error for DataHashError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
        }
    }
}

/// Hash the shipping data set rooted at `data_root` (the `data/`
/// directory). Deterministic by construction: paths are sorted before
/// hashing, so filesystem enumeration order is irrelevant.
pub fn data_set_hash(data_root: &Path) -> Result<[u8; 32], DataHashError> {
    let mut h = blake3::Hasher::new();
    for rel in hashed_files(data_root)? {
        let bytes = std::fs::read(data_root.join(&rel)).map_err(|source| DataHashError::Io {
            path: data_root.join(&rel),
            source,
        })?;
        h.update(rel.as_bytes());
        h.update(&[0]);
        h.update(&canonicalize(&bytes));
    }
    Ok(*h.finalize().as_bytes())
}

/// Every hashed file under `data_root` as `/`-joined relative paths in
/// ascending order, after the §3.1 exclusions.
fn hashed_files(root: &Path) -> Result<Vec<String>, DataHashError> {
    let mut out = Vec::new();
    collect(root, "", &mut out)?;
    out.sort();
    Ok(out)
}

fn collect(dir: &Path, prefix: &str, out: &mut Vec<String>) -> Result<(), DataHashError> {
    let entries = std::fs::read_dir(dir).map_err(|source| DataHashError::Io {
        path: dir.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| DataHashError::Io {
            path: dir.to_path_buf(),
            source,
        })?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let rel = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        if entry.path().is_dir() {
            // §3.1: data/test-fixtures/ is out of the shipping data set.
            if rel == "test-fixtures" {
                continue;
            }
            collect(&entry.path(), &rel, out)?;
        } else if in_scope(&rel) {
            out.push(rel);
        }
    }
    Ok(())
}

/// §3.1 file inclusion: everything except `test-fixtures/**` (handled in
/// [`collect`]) and `art/**` files with an [`ART_EXTENSIONS`] extension.
fn in_scope(rel: &str) -> bool {
    if let Some(rest) = rel.strip_prefix("art/")
        && let Some(ext) = rest.rsplit('.').next()
        && ART_EXTENSIONS.contains(&ext)
    {
        return false;
    }
    true
}

/// §3.1 canonicalization — identical to csp-bench's fixture canonicalizer
/// by design: UTF-8 text → trim-end each line, rejoin with LF; other
/// bytes verbatim.
fn canonicalize(bytes: &[u8]) -> Vec<u8> {
    match std::str::from_utf8(bytes) {
        Ok(text) => text
            .lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n")
            .into_bytes(),
        Err(_) => bytes.to_vec(),
    }
}
