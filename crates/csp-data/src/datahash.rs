//! The §3.1 **data-set hash** — BLAKE3 over `path_bytes || 0x00 ||
//! u64-LE len || canonicalized_file_bytes`, ascending path order, for
//! every file under `data/` *except*:
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
    "png", "jpg", "jpeg", "tga", "webp", "ktx2", "ogg", "wav", "glb", "csar",
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
    /// A symlink sits inside `data/` — the hash walks the committed
    /// file tree only; a link's target could change outside it.
    Symlink {
        /// The link.
        path: PathBuf,
    },
    /// A file name is not UTF-8 — the hash keys files by path bytes,
    /// and a lossy conversion could collide two distinct names.
    NonUtf8Name {
        /// The file.
        path: PathBuf,
    },
    /// Two paths differ only by ASCII case — the committed tree hashes
    /// both on a case-sensitive filesystem, but a checkout on a
    /// case-insensitive one collapses them into a different set (and a
    /// different hash): the same repo would fingerprint differently by
    /// platform.
    CaseCollision {
        /// The first path (byte-sorted order).
        a: PathBuf,
        /// The colliding path.
        b: PathBuf,
    },
}

impl fmt::Display for DataHashError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(f, "io at {}: {source}", path.display()),
            Self::Symlink { path } => {
                write!(f, "symlink in the data set at {}", path.display())
            }
            Self::NonUtf8Name { path } => {
                write!(f, "non-UTF-8 file name at {}", path.display())
            }
            Self::CaseCollision { a, b } => {
                write!(
                    f,
                    "paths differ only by case: {} vs {}",
                    a.display(),
                    b.display()
                )
            }
        }
    }
}

impl std::error::Error for DataHashError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Symlink { .. } | Self::NonUtf8Name { .. } | Self::CaseCollision { .. } => None,
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
        feed_file(&mut h, &rel, &canonicalize(&bytes));
    }
    Ok(*h.finalize().as_bytes())
}

/// One file's contribution to the §3.1 stream: `path || 0x00 || len ||
/// content`. The `0x00` terminates the path (filenames cannot carry
/// NUL); the little-endian length prefix then makes the whole frame
/// injective — the earlier `path || 0x00 || content` framing was not
/// (file `a` containing `Xb\0Y` hashed identically to files `a`→`X` and
/// `b`→`Y`), which is why [`crate::DATA_HASH_VERSION`] bumped to 2.
fn feed_file(h: &mut blake3::Hasher, rel: &str, content: &[u8]) {
    h.update(rel.as_bytes());
    h.update(&[0]);
    h.update(&(content.len() as u64).to_le_bytes());
    h.update(content);
}

/// Every hashed file under `data_root` as `/`-joined relative paths in
/// ascending order, after the §3.1 exclusions.
fn hashed_files(root: &Path) -> Result<Vec<String>, DataHashError> {
    let mut out = Vec::new();
    collect(root, "", &mut out)?;
    out.sort();
    // Two paths that differ only by ASCII case hash fine here but
    // collapse to one file on a case-insensitive checkout — the same
    // commit would produce a different data-set hash by platform, so
    // the collision is refused like a symlink or non-UTF-8 name. The
    // fold key is a full-string compare, not an adjacency scan: byte
    // sort does NOT put case-folded-equal paths next to each other
    // (`"FOO.txt" < "Zebra" < "foo.txt"`).
    let mut folded: std::collections::BTreeMap<String, &str> = std::collections::BTreeMap::new();
    for rel in &out {
        let key = rel.to_ascii_lowercase();
        if let Some(first) = folded.insert(key, rel) {
            return Err(DataHashError::CaseCollision {
                a: PathBuf::from(first),
                b: PathBuf::from(rel),
            });
        }
    }
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
        // `file_type` does not follow symlinks: a symlink inside
        // `data/` is refused rather than silently traversed — its
        // target could change outside the hashed tree (and a link
        // pointing out of `data/` would escape the set entirely).
        let ft = entry.file_type().map_err(|source| DataHashError::Io {
            path: entry.path(),
            source,
        })?;
        if ft.is_symlink() {
            return Err(DataHashError::Symlink { path: entry.path() });
        }
        // The hash stream keys files by their relative-path BYTES —
        // a name that cannot be represented as UTF-8 has no honest
        // key (lossy conversion could collide two distinct names).
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| DataHashError::NonUtf8Name { path: entry.path() })?;
        let rel = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        if entry.path().is_dir() {
            // §3.1: data/test-fixtures/ is out of the shipping data
            // set — case-folded like `art/` so a differently-cased
            // checkout excludes the same directory.
            if rel.eq_ignore_ascii_case("test-fixtures") {
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
/// Both the `art/` prefix and the extension match are
/// ASCII-case-insensitive — `art/x.PNG` is the same presentation asset
/// as `art/x.png` (a case-sensitive compare would hash it, flipping
/// `data_set_hash` on an art-only change), and a `data/Art/` directory
/// *is* `data/art/` on a case-insensitive checkout, so folding keeps
/// the hashed set platform-identical.
fn in_scope(rel: &str) -> bool {
    let in_art = rel.len() > 4 && rel[..4].eq_ignore_ascii_case("art/");
    if in_art
        && let Some(ext) = rel[4..].rsplit('.').next()
        && ART_EXTENSIONS.iter().any(|e| ext.eq_ignore_ascii_case(e))
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

#[cfg(test)]
// Tests signal failure by panicking; §2.13's no-panic rule covers shipping
// fallible paths, not test code.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// The framing must be injective: under the pre-v2 scheme
    /// (`path || 0x00 || content`) the file set `{("a", "Xb\0Y")}`
    /// hashed byte-identically to `{("a", "X"), ("b", "Y")}` — one
    /// stream, two file sets. The length prefix separates them.
    #[test]
    fn framing_is_injective_against_content_injected_boundaries() {
        let mut one_file = blake3::Hasher::new();
        feed_file(&mut one_file, "a", b"Xb\0Y");
        let mut two_files = blake3::Hasher::new();
        feed_file(&mut two_files, "a", b"X");
        feed_file(&mut two_files, "b", b"Y");
        assert_ne!(
            one_file.finalize().as_bytes(),
            two_files.finalize().as_bytes(),
            "injected boundary must not collide with the two-file frame"
        );

        // The probe this guards against: both sequences are literally
        // `a\0Xb\0Y` under the retired framing.
        let legacy = |files: &[(&str, &[u8])]| {
            let mut h = blake3::Hasher::new();
            for (p, c) in files {
                h.update(p.as_bytes());
                h.update(&[0]);
                h.update(c);
            }
            *h.finalize().as_bytes()
        };
        assert_eq!(
            legacy(&[("a", b"Xb\0Y")]),
            legacy(&[("a", b"X"), ("b", b"Y")]),
            "the retired framing demonstrably collided"
        );
    }

    /// `art/` exclusion folds ASCII case on both the directory prefix
    /// and the extension — `art/x.PNG` is an art asset, not hashed
    /// data (a case-sensitive match would flip `data_set_hash` on an
    /// art-only change).
    #[test]
    fn in_scope_folds_case_on_art_prefix_and_extension() {
        for rel in [
            "art/tex.png",
            "art/tex.PNG",
            "Art/tex.png",
            "aRt/sub/tex.JpG",
        ] {
            assert!(!in_scope(rel), "{rel} must be excluded as art");
        }
        for rel in [
            "art/tex.ron",      // not an art extension — data
            "artistic/tex.png", // not the art dir
            "art",              // not under art/
            "artx/tex.png",
            "biomes/terrin.png", // png outside art/ IS hashed (§3.1)
        ] {
            assert!(in_scope(rel), "{rel} must be hashed");
        }
    }

    /// Two files whose paths differ only by case collide on a
    /// case-insensitive checkout — the same commit would fingerprint
    /// differently by platform, so the hash refuses them.
    #[test]
    fn case_colliding_paths_are_refused() {
        let root = std::env::temp_dir().join(format!("csp-datahash-case-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("parts")).unwrap();
        std::fs::write(root.join("parts/FOO.ron"), b"a").unwrap();
        std::fs::write(root.join("parts/foo.ron"), b"b").unwrap();
        let err = data_set_hash(&root).unwrap_err();
        let _ = std::fs::remove_dir_all(&root);
        assert!(
            matches!(err, DataHashError::CaseCollision { .. }),
            "got {err:?}"
        );
    }

    /// An uppercase art extension is excluded end-to-end: hashing a
    /// tree containing only `art/x.PNG` yields the empty-set hash.
    #[test]
    fn uppercase_art_extension_hashes_nothing() {
        let root = std::env::temp_dir().join(format!("csp-datahash-png-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("art")).unwrap();
        std::fs::write(root.join("art/x.PNG"), [0u8; 16]).unwrap();
        let h = data_set_hash(&root).unwrap();
        let _ = std::fs::remove_dir_all(&root);
        let empty = {
            let r = std::env::temp_dir().join(format!("csp-datahash-empty-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&r);
            std::fs::create_dir_all(&r).unwrap();
            let h = data_set_hash(&r).unwrap();
            let _ = std::fs::remove_dir_all(&r);
            h
        };
        assert_eq!(h, empty, "an art-only tree must hash as the empty set");
    }
}
