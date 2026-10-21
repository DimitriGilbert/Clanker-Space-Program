//! The system-data loader (PLAN §2.1: csp-data is "loader + validator").
//!
//! The loader's only I/O is reading files under the `dir` the caller hands
//! it — no clock, no environment, no network — so the determinism-domain
//! rules hold trivially. Everything *semantic* (parent links, SOI/period
//! consistency, biome-map sanity) is the validator's job
//! ([`crate::validate`]); the loader enforces only file shape: presence,
//! RON syntax, and `schema_version`.
//!
//! Layout (§3.0):
//! ```text
//! data/systems/<sys>/system.ron
//! data/systems/<sys>/<body>/body.ron
//! data/systems/<sys>/<body>/biomes.ron   \ when body.ron's
//! data/systems/<sys>/<body>/biomes.png   / biome_map is Some
//! ```

use std::fmt;
use std::path::{Path, PathBuf};

use crate::balance::{BALANCE_SCHEMA_VERSION, BalanceDef};
use crate::biome::BiomeLegend;
use crate::system::{BodyDef, SCHEMA_VERSION, SystemDef};

/// A biome map as loaded: legend (parsed) plus the PNG's raw bytes.
/// The bytes stay raw — pixel indices are part of the hashed data set
/// (§3.1) and are only *validated* structurally ([`crate::biome`]).
#[derive(Clone, Debug, PartialEq)]
pub struct LoadedBiomeMap {
    /// Parsed `biomes.ron`.
    pub legend: BiomeLegend,
    /// Raw bytes of the palette-indexed PNG.
    pub png: Vec<u8>,
}

/// One body's loaded set: its `body.ron` plus, when
/// [`BodyDef::biome_map`] is `Some`, its legend + map bytes.
#[derive(Clone, Debug, PartialEq)]
pub struct LoadedBody {
    /// Parsed `body.ron`.
    pub def: BodyDef,
    /// The §3.0.1 biome map, if the def references one.
    pub biomes: Option<LoadedBiomeMap>,
}

/// Everything `load_system` read for one `data/systems/<sys>/` directory.
/// `bodies` is aligned with [`SystemDef::bodies`]: `bodies[i]` is the
/// file loaded for `system.bodies[i]` (the validator re-checks each
/// def's own `id` field against the manifest entry).
#[derive(Clone, Debug, PartialEq)]
pub struct LoadedSystem {
    /// Parsed `system.ron`.
    pub system: SystemDef,
    /// One [`LoadedBody`] per manifest entry, in manifest order.
    pub bodies: Vec<LoadedBody>,
}

/// Loader failures. Deliberately a plain error type, not
/// `csp_core::SimError` — §2.13's four classes describe the sim's failure
/// contract; a missing file or bad RON is data-tooling diagnostics (the
/// same split `csp_bench::ReplayError` uses).
#[derive(Debug)]
pub enum LoadError {
    /// A required file could not be read.
    Io {
        /// The path that failed.
        path: PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
    /// A file exists but is not valid RON / does not match its schema.
    /// `SpannedError` is boxed so `LoadError` stays small on every
    /// `Result` in the crate (clippy::result_large_err).
    Ron {
        /// The path that failed.
        path: PathBuf,
        /// The parse error (positioned — `SpannedError` carries a line/col).
        source: Box<ron::error::SpannedError>,
    },
    /// `schema_version` is present but not one this loader understands
    /// (§3.1: current and previous are supported; only v1 exists today,
    /// so anything else is rejected).
    UnsupportedSchemaVersion {
        /// The file carrying the bad version.
        path: PathBuf,
        /// What the file declared.
        found: u32,
        /// The schema version the loader for this file type knows.
        expected: u32,
    },
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(f, "io at {}: {source}", path.display()),
            Self::Ron { path, source } => write!(f, "{}: {source}", path.display()),
            Self::UnsupportedSchemaVersion {
                path,
                found,
                expected,
            } => write!(
                f,
                "{}: unsupported schema_version {found} (loader knows {expected})",
                path.display()
            ),
        }
    }
}

impl std::error::Error for LoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Ron { source, .. } => Some(source),
            Self::UnsupportedSchemaVersion { .. } => None,
        }
    }
}

fn read_ron<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, LoadError> {
    let text = std::fs::read_to_string(path).map_err(|source| LoadError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    ron::de::from_str(&text).map_err(|source| LoadError::Ron {
        path: path.to_path_buf(),
        source: Box::new(source),
    })
}

fn check_version(path: &Path, found: u32, expected: u32) -> Result<(), LoadError> {
    if found == expected {
        Ok(())
    } else {
        Err(LoadError::UnsupportedSchemaVersion {
            path: path.to_path_buf(),
            found,
            expected,
        })
    }
}

/// Load `system.ron` + every listed `<body>/body.ron` (+ biome files when
/// referenced) from a `data/systems/<sys>/` directory.
///
/// Validates file shape only — run [`crate::validate::validate_system`]
/// on the result for the §3.0 semantic rules.
pub fn load_system(dir: &Path) -> Result<LoadedSystem, LoadError> {
    let system_path = dir.join("system.ron");
    let system: SystemDef = read_ron(&system_path)?;
    check_version(&system_path, system.schema_version, SCHEMA_VERSION)?;

    let mut bodies = Vec::with_capacity(system.bodies.len());
    for id in &system.bodies {
        let body_dir = dir.join(id.as_str());
        let body_path = body_dir.join("body.ron");
        let def: BodyDef = read_ron(&body_path)?;
        check_version(&body_path, def.schema_version, SCHEMA_VERSION)?;

        let biomes = match &def.biome_map {
            None => None,
            Some(r) => {
                let legend_path = body_dir.join(&r.legend);
                let legend: BiomeLegend = read_ron(&legend_path)?;
                check_version(&legend_path, legend.schema_version, SCHEMA_VERSION)?;
                let png_path = body_dir.join(&r.png);
                let png = std::fs::read(&png_path).map_err(|source| LoadError::Io {
                    path: png_path,
                    source,
                })?;
                Some(LoadedBiomeMap { legend, png })
            }
        };
        bodies.push(LoadedBody { def, biomes });
    }
    Ok(LoadedSystem { system, bodies })
}

/// Load `data/balance.ron` — the global tuning file (§8.3). Its path is
/// the caller's choice (`data/balance.ron` in production, a fixture copy
/// in tests); file-shape checks only.
pub fn load_balance(path: &Path) -> Result<BalanceDef, LoadError> {
    let balance: BalanceDef = read_ron(path)?;
    check_version(path, balance.schema_version, BALANCE_SCHEMA_VERSION)?;
    Ok(balance)
}
