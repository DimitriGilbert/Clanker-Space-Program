//! The system-data loader (PLAN §2.1: csp-data is "loader + validator").
//!
//! The loader's only I/O is reading files under the `dir` the caller hands
//! it — no clock, no environment, no network — so the determinism-domain
//! rules hold trivially. Everything *semantic* (parent links, SOI/period
//! consistency, biome-map sanity) is the validator's job
//! ([`crate::validate`]); the loader enforces only file shape: presence,
//! RON syntax, `schema_version` (checked BEFORE the full def parse via
//! [`SchemaProbe`], so a stale file reports its version rather than the
//! missing new field its out-of-date shape would produce), and the
//! authored-name gate: every authored string that becomes a path
//! component (`system.bodies` entries, `BiomeMapRef` legend/png) must be
//! a tame single-segment relative name — `..`/absolute/separator-bearing
//! names are rejected before the `join` ([`LoadError::UnsafeName`]).
//!
//! Layout (§3.0):
//! ```text
//! data/systems/<sys>/system.ron
//! data/systems/<sys>/<body>/body.ron
//! data/systems/<sys>/<body>/biomes.ron   \ when body.ron's
//! data/systems/<sys>/<body>/biomes.png   / biome_map is Some
//! ```

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::balance::{BALANCE_SCHEMA_VERSION, BalanceDef};
use crate::biome::BiomeLegend;
use crate::colony::{COLONY_SCHEMA_VERSION, ColonyFile};
use crate::contract::{CONTRACT_SCHEMA_VERSION, ContractTemplate};
use crate::experiment::{EXPERIMENT_SCHEMA_VERSION, ExperimentDef};
use crate::ids::ExperimentId;
use crate::part::{PART_SCHEMA_VERSION, PartDef};
use crate::recipe::{RECIPE_SCHEMA_VERSION, RecipeDef};
use crate::resource::{RESOURCE_SCHEMA_VERSION, ResourceFile};
use crate::site::{SITE_SCHEMA_SUPPORTED, SITE_SCHEMA_VERSION, SiteFile};
use crate::system::{BodyDef, SCHEMA_VERSION, SystemDef};
use crate::techtree::{TECHTREE_SCHEMA_VERSION, TechTree, TechTreeDef};

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

/// One loaded part def plus the file it came from — the validator
/// checks `id` ↔ file-stem agreement (§3.1: the file *is* the ID).
#[derive(Clone, Debug, PartialEq)]
pub struct LoadedPart {
    /// Parsed def.
    pub def: PartDef,
    /// Path relative to the `data/parts/` root the loader was handed —
    /// carried for diagnostics (`parts/engines/csp.engine.sparc.ron`)
    /// and the id↔stem check.
    pub path: PathBuf,
}

/// Everything `load_parts` read under a `data/parts/` directory, in
/// ascending relative-path order (deterministic — the same order
/// `data_set_hash` hashes them in).
#[derive(Clone, Debug, PartialEq)]
pub struct LoadedParts {
    /// Parsed part defs with their source paths.
    pub parts: Vec<LoadedPart>,
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
    /// An authored name — a `system.bodies` entry or a file reference
    /// (`biomes.ron` legend/png, `heightmap`, concentration map) — is
    /// not a plain single-segment relative name. `..`/absolute/
    /// separator-bearing strings would escape the directory the caller
    /// handed the loader (`FsDataSource` joins verbatim), so the join
    /// refuses them up front.
    UnsafeName {
        /// The file the bad name was read from.
        path: PathBuf,
        /// The offending name.
        name: String,
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
            Self::UnsafeName { path, name } => write!(
                f,
                "{}: unsafe authored name {name:?} — not a single-segment relative name",
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
            Self::UnsupportedSchemaVersion { .. } | Self::UnsafeName { .. } => None,
        }
    }
}

/// The file-reading half of a `data/` tree. Native builds read the
/// filesystem ([`FsDataSource`]); the §6.3 web tier reads a
/// compile-time-embedded table ([`EmbeddedSource`]) — one trait, one
/// loader path, so the demo's reduced dataset goes through the same
/// file-shape checks as the desktop `data/` directory.
///
/// The paths a source sees are repo-relative (`data/parts/x.ron`,
/// `data/systems/home/system.ron`) — the same strings callers pass as
/// `dir`/`path` to the `load_*_from` functions.
pub trait DataSource {
    /// Read `path` as UTF-8 text.
    fn read_string(&self, path: &Path) -> Result<String, LoadError>;
    /// Read `path` as bytes.
    fn read_bytes(&self, path: &Path) -> Result<Vec<u8>, LoadError>;
    /// `true` when `path` exists — file *or* directory.
    fn exists(&self, path: &Path) -> bool;
    /// Every `*.ron` file under `dir`, ascending path order — the
    /// `collect_ron_files` determinism contract.
    fn ron_files(&self, dir: &Path) -> Result<Vec<PathBuf>, LoadError>;
}

/// Read `data/` from the filesystem — the native/default source.
#[derive(Clone, Copy, Debug, Default)]
pub struct FsDataSource;

impl DataSource for FsDataSource {
    fn read_string(&self, path: &Path) -> Result<String, LoadError> {
        std::fs::read_to_string(path).map_err(|source| LoadError::Io {
            path: path.to_path_buf(),
            source,
        })
    }

    fn read_bytes(&self, path: &Path) -> Result<Vec<u8>, LoadError> {
        std::fs::read(path).map_err(|source| LoadError::Io {
            path: path.to_path_buf(),
            source,
        })
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    /// Recursive walk sorted by the same key `data_set_hash` sorts on —
    /// the `/`-joined relative path compared byte-wise (see
    /// [`collect_ron_files`] for why a plain `Path` sort is NOT the
    /// same order).
    fn ron_files(&self, dir: &Path) -> Result<Vec<PathBuf>, LoadError> {
        let mut out = Vec::new();
        collect_ron_files(dir, &mut out)?;
        Ok(out)
    }
}

/// A compile-time-embedded `data/` tree — a static path → bytes table
/// baked in with `include_bytes!`. The §6.3 web demo ships its reduced
/// dataset this way (no filesystem on wasm); the source itself is plain
/// data, so native tests exercise the same path.
pub struct EmbeddedSource {
    files: BTreeMap<PathBuf, &'static [u8]>,
}

impl EmbeddedSource {
    /// Build from a `(path, bytes)` table — `path` is the virtual
    /// repo-relative path (`"data/balance.ron"`).
    pub fn new(files: &'static [(&'static str, &'static [u8])]) -> Self {
        Self {
            files: files
                .iter()
                .map(|(path, bytes)| (PathBuf::from(path), *bytes))
                .collect(),
        }
    }

    fn missing(path: &Path) -> LoadError {
        LoadError::Io {
            path: path.to_path_buf(),
            source: std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "not in the embedded data set",
            ),
        }
    }
}

impl DataSource for EmbeddedSource {
    fn read_string(&self, path: &Path) -> Result<String, LoadError> {
        let bytes = self
            .files
            .get(path)
            .copied()
            .ok_or_else(|| Self::missing(path))?;
        String::from_utf8(bytes.to_vec()).map_err(|e| LoadError::Io {
            path: path.to_path_buf(),
            source: std::io::Error::new(std::io::ErrorKind::InvalidData, e),
        })
    }

    fn read_bytes(&self, path: &Path) -> Result<Vec<u8>, LoadError> {
        self.files
            .get(path)
            .map(|b| b.to_vec())
            .ok_or_else(|| Self::missing(path))
    }

    /// A path "exists" when it names an embedded file or prefixes one —
    /// `exists("data/parts")` answers the `dir.exists()` checks the
    /// optional-table loaders (`load_sites`, `load_colonies`, …) make.
    /// `Path::starts_with` is component-wise, so `data/parts` never
    /// matches `data/parts.ron`.
    fn exists(&self, path: &Path) -> bool {
        self.files.contains_key(path) || self.files.keys().any(|k| k.starts_with(path))
    }

    fn ron_files(&self, dir: &Path) -> Result<Vec<PathBuf>, LoadError> {
        // BTreeMap key order is component-wise `Path` order — which is
        // NOT `data_set_hash`'s order on a `x/` vs `x.ron` prefix
        // collision, so the result is re-sorted by the same key the
        // filesystem walk uses (`collect_ron_files`).
        let mut out: Vec<PathBuf> = self
            .files
            .keys()
            .filter(|k| k.starts_with(dir) && k.extension().is_some_and(|ext| ext == "ron"))
            .cloned()
            .collect();
        out.sort_by_cached_key(|p| ron_sort_key(dir, p));
        Ok(out)
    }
}

/// A `schema_version`-only view of a versioned data file — every file's
/// first field (§3.1). [`read_versioned_ron`] probes this BEFORE the
/// full def parse: a stale-version file missing a *new* required field
/// would otherwise report the missing field (a shape error) when the
/// actionable diagnosis is the version. Probe failures are ignored —
/// the full parse's positioned [`LoadError::Ron`] is the better
/// diagnostic for a file that is not RON at all or whose
/// `schema_version` is not a `u32`.
#[derive(serde::Deserialize)]
struct SchemaProbe {
    schema_version: Option<u32>,
}

fn check_version(path: &Path, found: u32, supported: &[u32]) -> Result<(), LoadError> {
    if supported.contains(&found) {
        Ok(())
    } else {
        Err(LoadError::UnsupportedSchemaVersion {
            path: path.to_path_buf(),
            found,
            expected: *supported.last().unwrap_or(&0),
        })
    }
}

/// Read `path` as `T`, running the `supported`-band version check
/// *before* the full parse (§3.1's order — a stale file's missing new
/// fields must not mask the version problem as a shape error) and again
/// on the parsed def via `version_of`.
fn read_versioned_ron<T: serde::de::DeserializeOwned>(
    src: &dyn DataSource,
    path: &Path,
    supported: &[u32],
    version_of: impl FnOnce(&T) -> u32,
) -> Result<T, LoadError> {
    let text = src.read_string(path)?;
    if let Ok(SchemaProbe {
        schema_version: Some(found),
    }) = ron::de::from_str::<SchemaProbe>(&text)
    {
        check_version(path, found, supported)?;
    }
    let def: T = ron::de::from_str(&text).map_err(|source| LoadError::Ron {
        path: path.to_path_buf(),
        source: Box::new(source),
    })?;
    check_version(path, version_of(&def), supported)?;
    Ok(def)
}

/// `true` for a tame one-segment file/dir name — what authored names
/// (body ids, `biomes.ron`/`*.png` refs, heightmaps, concentration
/// maps) must satisfy before they may be joined under a data dir:
/// non-empty, not `.`/`..`, and free of `/`, `\`, and NUL. The
/// validator repeats the check ([`crate::validate`]) so a hand-rolled
/// `LoadedSystem` cannot smuggle an unsafe ref to later consumers.
pub(crate) fn authored_name_ok(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\', '\0'])
}

/// `dir.join(name)` for an *authored* single-segment relative name —
/// the guard against `..`/absolute/separator-bearing authored strings
/// escaping the directory the caller handed the loader.
/// `FsDataSource` joins verbatim, so an unchecked `..` in
/// `system.bodies` or a `BiomeMapRef` would read outside `data/`.
/// `declared_in` names the file the bad name came from, for the
/// diagnostic.
fn join_authored(dir: &Path, name: &str, declared_in: &Path) -> Result<PathBuf, LoadError> {
    if authored_name_ok(name) {
        Ok(dir.join(name))
    } else {
        Err(LoadError::UnsafeName {
            path: declared_in.to_path_buf(),
            name: name.to_string(),
        })
    }
}

/// Load `system.ron` + every listed `<body>/body.ron` (+ biome files when
/// referenced) from a `data/systems/<sys>/` directory on the filesystem.
///
/// Validates file shape only — run [`crate::validate::validate_system`]
/// on the result for the §3.0 semantic rules.
pub fn load_system(dir: &Path) -> Result<LoadedSystem, LoadError> {
    load_system_from(&FsDataSource, dir)
}

/// [`load_system`] against an arbitrary [`DataSource`] — the web demo's
/// embedded dataset rides this (§6.3).
pub fn load_system_from(src: &dyn DataSource, dir: &Path) -> Result<LoadedSystem, LoadError> {
    let system_path = dir.join("system.ron");
    let system: SystemDef =
        read_versioned_ron(src, &system_path, &[SCHEMA_VERSION], |d: &SystemDef| {
            d.schema_version
        })?;

    let mut bodies = Vec::with_capacity(system.bodies.len());
    for id in &system.bodies {
        // Authored names become path components here — tame names only
        // (`join_authored`); a `..`/absolute/separator-bearing id or
        // biome ref would escape `dir` under `FsDataSource`.
        let body_dir = join_authored(dir, id.as_str(), &system_path)?;
        let body_path = body_dir.join("body.ron");
        let def: BodyDef =
            read_versioned_ron(src, &body_path, &[SCHEMA_VERSION], |d: &BodyDef| {
                d.schema_version
            })?;

        let biomes = match &def.biome_map {
            None => None,
            Some(r) => {
                let legend_path = join_authored(&body_dir, &r.legend, &body_path)?;
                let legend: BiomeLegend =
                    read_versioned_ron(src, &legend_path, &[SCHEMA_VERSION], |l: &BiomeLegend| {
                        l.schema_version
                    })?;
                let png_path = join_authored(&body_dir, &r.png, &body_path)?;
                let png = src.read_bytes(&png_path)?;
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
    load_balance_from(&FsDataSource, path)
}

/// [`load_balance`] against an arbitrary [`DataSource`]. File-shape
/// checks only — the ~45 tunables' finiteness/range rules live in
/// [`crate::validate::validate_balance`].
pub fn load_balance_from(src: &dyn DataSource, path: &Path) -> Result<BalanceDef, LoadError> {
    read_versioned_ron(src, path, &[BALANCE_SCHEMA_VERSION], |b: &BalanceDef| {
        b.schema_version
    })
}

/// Load `data/resources.ron` — the §3.5 resource registry. File-shape
/// checks only; uniqueness/positivity rules live in
/// [`crate::validate`].
pub fn load_resources(path: &Path) -> Result<ResourceFile, LoadError> {
    load_resources_from(&FsDataSource, path)
}

/// [`load_resources`] against an arbitrary [`DataSource`].
pub fn load_resources_from(src: &dyn DataSource, path: &Path) -> Result<ResourceFile, LoadError> {
    read_versioned_ron(src, path, &[RESOURCE_SCHEMA_VERSION], |r: &ResourceFile| {
        r.schema_version
    })
}

/// Load `data/systems/<sys>/sites.ron` — the §3.5 surface-site
/// registry. A MISSING file reads as an empty set (the
/// `load_experiments` convention): sites are optional until a
/// `RecoverVessel` command needs one. File-shape checks only; the
/// body/geometry rules live in [`crate::validate::validate_sites`].
pub fn load_sites(path: &Path) -> Result<SiteFile, LoadError> {
    load_sites_from(&FsDataSource, path)
}

/// [`load_sites`] against an arbitrary [`DataSource`].
pub fn load_sites_from(src: &dyn DataSource, path: &Path) -> Result<SiteFile, LoadError> {
    if !src.exists(path) {
        return Ok(SiteFile {
            schema_version: SITE_SCHEMA_VERSION,
            sites: Vec::new(),
        });
    }
    // §3.1's "current and previous with explicit defaults": v1 files
    // lack `resource_yields`, which serde reads as the empty list —
    // hence the band rather than the exact-match the other loaders
    // run.
    read_versioned_ron(src, path, &SITE_SCHEMA_SUPPORTED, |s: &SiteFile| {
        s.schema_version
    })
}

/// Load `data/systems/<sys>/colonies.ron` — the §3.5.1 authored-colony
/// set. A MISSING file reads as an empty set (the `load_sites`
/// convention): a system without authored colonies starts with none
/// and `FoundColony` commands found them at runtime. File-shape
/// checks only; the rate/resource rules live in
/// [`crate::validate::validate_colonies`].
pub fn load_colonies(path: &Path) -> Result<ColonyFile, LoadError> {
    load_colonies_from(&FsDataSource, path)
}

/// [`load_colonies`] against an arbitrary [`DataSource`].
pub fn load_colonies_from(src: &dyn DataSource, path: &Path) -> Result<ColonyFile, LoadError> {
    if !src.exists(path) {
        return Ok(ColonyFile {
            schema_version: COLONY_SCHEMA_VERSION,
            colonies: Vec::new(),
        });
    }
    read_versioned_ron(src, path, &[COLONY_SCHEMA_VERSION], |c: &ColonyFile| {
        c.schema_version
    })
}

/// One loaded recipe def plus its source path — the `id` ↔ stem check
/// needs the file name, like [`LoadedExperiment`].
#[derive(Clone, Debug, PartialEq)]
pub struct LoadedRecipe {
    /// Parsed def.
    pub def: RecipeDef,
    /// Path relative to the `data/recipes/` root the loader was handed.
    pub path: PathBuf,
}

/// Everything `load_recipes` read under a `data/recipes/` directory,
/// ascending relative-path order — the deterministic order
/// `data_set_hash` hashes them in.
#[derive(Clone, Debug, PartialEq)]
pub struct LoadedRecipes {
    /// Parsed recipe defs with their source paths.
    pub recipes: Vec<LoadedRecipe>,
}

impl LoadedRecipes {
    /// The defs keyed by [`crate::ids::RecipeId`] — the table the sim
    /// installs as construction config and the validator resolves
    /// converter refs against. First def wins on a duplicate id; the
    /// validator flags the duplicate loudly.
    pub fn by_id(&self) -> std::collections::BTreeMap<crate::ids::RecipeId, &RecipeDef> {
        let mut out = std::collections::BTreeMap::new();
        for r in &self.recipes {
            out.entry(r.def.id.clone()).or_insert(&r.def);
        }
        out
    }
}

/// Load every recipe def under `data/recipes/` — `<id>.ron` files,
/// optionally grouped in subdirectories. File-shape checks only; the
/// resource refs and rate rules live in
/// [`crate::validate::validate_recipes`]. A missing directory reads
/// as an empty set — a world without recipes simply rejects
/// `FoundColony` defs carrying `Converter` modules.
pub fn load_recipes(dir: &Path) -> Result<LoadedRecipes, LoadError> {
    load_recipes_from(&FsDataSource, dir)
}

/// [`load_recipes`] against an arbitrary [`DataSource`].
pub fn load_recipes_from(src: &dyn DataSource, dir: &Path) -> Result<LoadedRecipes, LoadError> {
    if !src.exists(dir) {
        return Ok(LoadedRecipes {
            recipes: Vec::new(),
        });
    }
    let files = src.ron_files(dir)?;
    let mut recipes = Vec::with_capacity(files.len());
    for path in files {
        let def: RecipeDef =
            read_versioned_ron(src, &path, &[RECIPE_SCHEMA_VERSION], |d: &RecipeDef| {
                d.schema_version
            })?;
        let rel = path.strip_prefix(dir).unwrap_or(&path).to_path_buf();
        recipes.push(LoadedRecipe { def, path: rel });
    }
    Ok(LoadedRecipes { recipes })
}

/// One loaded experiment def plus its source path (the `id` ↔ stem
/// check needs the file name, like [`LoadedPart`]).
#[derive(Clone, Debug, PartialEq)]
pub struct LoadedExperiment {
    /// Parsed def.
    pub def: ExperimentDef,
    /// Path relative to the `data/experiments/` root the loader was
    /// handed — diagnostics only.
    pub path: PathBuf,
}

/// Everything `load_experiments` read under a `data/experiments/`
/// directory, ascending relative-path order — the deterministic order
/// `data_set_hash` hashes them in.
#[derive(Clone, Debug, PartialEq)]
pub struct LoadedExperiments {
    /// Parsed experiment defs with their source paths.
    pub experiments: Vec<LoadedExperiment>,
}

impl LoadedExperiments {
    /// The defs keyed by [`ExperimentId`] — the lookup the sim
    /// installs (`csp-view`/`csp-bench` world construction; the
    /// validator iterates `experiments` directly). First def wins on
    /// a duplicate id; the validator flags the duplicate loudly.
    pub fn by_id(&self) -> std::collections::BTreeMap<ExperimentId, &ExperimentDef> {
        let mut out = std::collections::BTreeMap::new();
        for e in &self.experiments {
            out.entry(e.def.id.clone()).or_insert(&e.def);
        }
        out
    }
}

/// Load every experiment def under `data/experiments/` — `<id>.ron`
/// files, optionally grouped in subdirectories. File-shape checks only;
/// `transmit_ratio` band and cap sanity live in
/// [`crate::validate::validate_experiments`]. A missing directory reads
/// as an empty set — experiment defs are optional until a part's
/// `Experiment` module or a science command needs them.
pub fn load_experiments(dir: &Path) -> Result<LoadedExperiments, LoadError> {
    load_experiments_from(&FsDataSource, dir)
}

/// [`load_experiments`] against an arbitrary [`DataSource`].
pub fn load_experiments_from(
    src: &dyn DataSource,
    dir: &Path,
) -> Result<LoadedExperiments, LoadError> {
    if !src.exists(dir) {
        return Ok(LoadedExperiments {
            experiments: Vec::new(),
        });
    }
    let files = src.ron_files(dir)?;
    let mut experiments = Vec::with_capacity(files.len());
    for path in files {
        let def: ExperimentDef = read_versioned_ron(
            src,
            &path,
            &[EXPERIMENT_SCHEMA_VERSION],
            |d: &ExperimentDef| d.schema_version,
        )?;
        let rel = path.strip_prefix(dir).unwrap_or(&path).to_path_buf();
        experiments.push(LoadedExperiment { def, path: rel });
    }
    Ok(LoadedExperiments { experiments })
}

/// One loaded contract template plus its source path — the `id` ↔
/// stem check needs the file name, like [`LoadedExperiment`].
#[derive(Clone, Debug, PartialEq)]
pub struct LoadedContract {
    /// Parsed def.
    pub def: ContractTemplate,
    /// Path relative to the `data/contracts/` root the loader was
    /// handed — diagnostics only.
    pub path: PathBuf,
}

/// Everything `load_contracts` read under a `data/contracts/`
/// directory, ascending relative-path order — the deterministic order
/// `data_set_hash` hashes them in.
#[derive(Clone, Debug, PartialEq)]
pub struct LoadedContracts {
    /// Parsed contract templates with their source paths.
    pub contracts: Vec<LoadedContract>,
}

impl LoadedContracts {
    /// The defs keyed by [`crate::ids::ContractTypeId`] — the lookup
    /// the sim installs. First def wins on a duplicate id; the
    /// validator flags the duplicate loudly.
    pub fn by_id(
        &self,
    ) -> std::collections::BTreeMap<crate::ids::ContractTypeId, &ContractTemplate> {
        let mut out = std::collections::BTreeMap::new();
        for c in &self.contracts {
            out.entry(c.def.id.clone()).or_insert(&c.def);
        }
        out
    }
}

/// Load every contract template under `data/contracts/` — `<id>.ron`
/// files, optionally grouped in subdirectories. File-shape checks
/// only; §3.6's structural rules (weights, rewards, prereq refs,
/// param-space sanity) live in [`crate::validate::validate_contracts`].
/// A missing directory reads as an empty set, matching
/// `load_experiments` — a world without templates simply never
/// generates offers.
pub fn load_contracts(dir: &Path) -> Result<LoadedContracts, LoadError> {
    load_contracts_from(&FsDataSource, dir)
}

/// [`load_contracts`] against an arbitrary [`DataSource`].
pub fn load_contracts_from(src: &dyn DataSource, dir: &Path) -> Result<LoadedContracts, LoadError> {
    if !src.exists(dir) {
        return Ok(LoadedContracts {
            contracts: Vec::new(),
        });
    }
    let files = src.ron_files(dir)?;
    let mut contracts = Vec::with_capacity(files.len());
    for path in files {
        let def: ContractTemplate = read_versioned_ron(
            src,
            &path,
            &[CONTRACT_SCHEMA_VERSION],
            |d: &ContractTemplate| d.schema_version,
        )?;
        let rel = path.strip_prefix(dir).unwrap_or(&path).to_path_buf();
        contracts.push(LoadedContract { def, path: rel });
    }
    Ok(LoadedContracts { contracts })
}

/// One loaded tech-tree fragment plus its source path — diagnostics
/// only, like [`LoadedExperiment`]: the merged tree is id-keyed, and
/// the validator reports errors with the file they came from.
#[derive(Clone, Debug, PartialEq)]
pub struct LoadedTechFile {
    /// Parsed fragment.
    pub def: TechTreeDef,
    /// Path relative to the `data/techtree/` root the loader was
    /// handed.
    pub path: PathBuf,
}

/// Everything `load_techtree` read under `data/techtree/`, ascending
/// relative-path order — the deterministic order `data_set_hash`
/// hashes them in, which is also the order [`Self::merged`]
/// concatenates nodes/tracks for the lookup view.
#[derive(Clone, Debug, PartialEq)]
pub struct LoadedTechTree {
    /// Parsed fragments with their source paths.
    pub trees: Vec<LoadedTechFile>,
}

impl LoadedTechTree {
    /// The flattened lookup view the sim installs — every fragment's
    /// `nodes`/`tracks` concatenated in ascending-path file order.
    /// Identity rules (duplicate ids, cycles, reachability) are the
    /// validator's; `merged` is pure concatenation so an invalid set
    /// still produces a deterministic view for the error list to
    /// describe.
    pub fn merged(&self) -> TechTree {
        let mut tree = TechTree::default();
        for f in &self.trees {
            tree.nodes.extend(f.def.nodes.iter().cloned());
            tree.tracks.extend(f.def.tracks.iter().cloned());
        }
        tree
    }
}

/// Load every tech-tree fragment under `data/techtree/` — `<name>.ron`
/// files, optionally grouped in subdirectories (organization only).
/// File-shape checks only: RON parses, `schema_version` current;
/// §3.3's structural rules (cycles, reachability, track sanity) are
/// [`crate::validate::validate_techtree`]. A missing directory reads
/// as an empty set, matching `load_experiments` — a world without a
/// tree simply rejects research commands.
pub fn load_techtree(dir: &Path) -> Result<LoadedTechTree, LoadError> {
    load_techtree_from(&FsDataSource, dir)
}

/// [`load_techtree`] against an arbitrary [`DataSource`].
pub fn load_techtree_from(src: &dyn DataSource, dir: &Path) -> Result<LoadedTechTree, LoadError> {
    if !src.exists(dir) {
        return Ok(LoadedTechTree { trees: Vec::new() });
    }
    let files = src.ron_files(dir)?;
    let mut trees = Vec::with_capacity(files.len());
    for path in files {
        let def: TechTreeDef =
            read_versioned_ron(src, &path, &[TECHTREE_SCHEMA_VERSION], |d: &TechTreeDef| {
                d.schema_version
            })?;
        let rel = path.strip_prefix(dir).unwrap_or(&path).to_path_buf();
        trees.push(LoadedTechFile { def, path: rel });
    }
    Ok(LoadedTechTree { trees })
}

/// Collect every `*.ron` under `dir` recursively, in `data_set_hash`'s
/// ascending order — the path relative to `dir` with components joined
/// by `/`, compared byte-wise ([`ron_sort_key`]). Deterministic by
/// construction: a reordering of `read_dir` results can never change
/// which def a duplicate-ID diagnostic names first.
fn collect_ron_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), LoadError> {
    gather_ron_files(dir, out)?;
    out.sort_by_cached_key(|p| ron_sort_key(dir, p));
    Ok(())
}

/// The sort key `data_set_hash` effectively orders hashed files by:
/// `path` relative to `dir`, components joined by `/`, compared
/// byte-wise. Plain `Path` ordering is component-wise and disagrees on
/// a prefix collision — `x/` sorts before `x.ron` component-wise but
/// `x.ron` sorts before `x/a.ron` byte-wise (`'.' < '/'`), and a
/// sorted-per-level DFS can never interleave a directory's contents
/// with its file siblings anyway. `as_encoded_bytes` keeps non-UTF-8
/// names deterministic on every platform.
fn ron_sort_key(dir: &Path, path: &Path) -> Vec<u8> {
    let mut key = Vec::new();
    for (i, c) in path
        .strip_prefix(dir)
        .unwrap_or(path)
        .components()
        .enumerate()
    {
        if i > 0 {
            key.push(b'/');
        }
        key.extend_from_slice(c.as_os_str().as_encoded_bytes());
    }
    key
}

/// The unsorted recursive half of [`collect_ron_files`].
fn gather_ron_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), LoadError> {
    let entries = std::fs::read_dir(dir)
        .map_err(|source| LoadError::Io {
            path: dir.to_path_buf(),
            source,
        })?
        // A failed directory entry is a load error, not a missing
        // file — `filter_map`-swallowing it would silently load a
        // partial registry.
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| LoadError::Io {
            path: dir.to_path_buf(),
            source,
        })?;
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            gather_ron_files(&path, out)?;
        } else if path.extension().is_some_and(|ext| ext == "ron") {
            out.push(path);
        }
    }
    Ok(())
}

/// Load every part def under `data/parts/` — `<id>.ron` files,
/// optionally grouped in subdirectories (a subdir is organization only;
/// it carries no semantics). File-shape checks only: RON parses,
/// `schema_version` current. Semantic rules (id↔stem, category/module
/// consistency, resource refs) are [`crate::validate::validate_parts`].
/// A missing directory reads as an empty set — the `load_experiments`
/// convention.
pub fn load_parts(dir: &Path) -> Result<LoadedParts, LoadError> {
    load_parts_from(&FsDataSource, dir)
}

/// [`load_parts`] against an arbitrary [`DataSource`]. A missing
/// directory reads as an empty set — the `load_experiments` convention
/// the other registry loaders share (previously `FsDataSource` errored
/// here where `EmbeddedSource` returned empty — an asymmetry, not a
/// deliberate gate).
pub fn load_parts_from(src: &dyn DataSource, dir: &Path) -> Result<LoadedParts, LoadError> {
    if !src.exists(dir) {
        return Ok(LoadedParts { parts: Vec::new() });
    }
    let files = src.ron_files(dir)?;
    let mut parts = Vec::with_capacity(files.len());
    for path in files {
        let def: PartDef =
            read_versioned_ron(src, &path, &[PART_SCHEMA_VERSION], |d: &PartDef| {
                d.schema_version
            })?;
        let rel = path.strip_prefix(dir).unwrap_or(&path).to_path_buf();
        parts.push(LoadedPart { def, path: rel });
    }
    Ok(LoadedParts { parts })
}

/// Load `data/loc/en.ron` — the §1.2 English localization table every
/// `LocKey` in the data set resolves against. Unlike the optional
/// registries a missing loc file is an error (every consumer treats it
/// as required — this loader is what they were hand-parsing before).
/// File-shape checks only; the `schema_version` re-check and the
/// key-resolution rules live in [`crate::validate::validate_loc`] and
/// the `validate_*_loc_keys` family.
pub fn load_loc(path: &Path) -> Result<crate::loc::LocFile, LoadError> {
    load_loc_from(&FsDataSource, path)
}

/// [`load_loc`] against an arbitrary [`DataSource`].
pub fn load_loc_from(src: &dyn DataSource, path: &Path) -> Result<crate::loc::LocFile, LoadError> {
    read_versioned_ron(src, path, &[SCHEMA_VERSION], |l: &crate::loc::LocFile| {
        l.schema_version
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// §3.1's order is version-first: a stale-version file that also
    /// fails the current schema's required-field shape must report
    /// `UnsupportedSchemaVersion`, not the missing-field `Ron` error
    /// its out-of-date shape would produce. (The pre-prescan loader
    /// reported exactly that misattributed shape error.)
    #[test]
    fn stale_version_reports_version_not_missing_field() {
        // colonies.ron v2 gained required fields — a file declaring
        // an unsupported version AND missing them is a version
        // problem, not a shape problem.
        let src = EmbeddedSource::new(&[(
            "colonies.ron",
            br#"(schema_version: 99, colonies: [])"# as &[u8],
        )]);
        let err = load_colonies_from(&src, Path::new("colonies.ron")).unwrap_err();
        assert!(
            matches!(err, LoadError::UnsupportedSchemaVersion { found: 99, .. }),
            "{err:?}"
        );
    }

    /// A file missing `schema_version` entirely has no version to
    /// report — the positioned `Ron` missing-field error is the right
    /// diagnostic (the probe stays out of the way).
    #[test]
    fn missing_version_still_reports_shape() {
        let src = EmbeddedSource::new(&[("colonies.ron", br#"(colonies: [])"# as &[u8])]);
        let err = load_colonies_from(&src, Path::new("colonies.ron")).unwrap_err();
        assert!(matches!(err, LoadError::Ron { .. }), "{err:?}");
    }

    /// An authored `system.bodies` entry that is not a tame
    /// one-segment name is refused before the join — `..` would
    /// escape the system dir under `FsDataSource`.
    #[test]
    fn unsafe_manifest_names_are_rejected() {
        for bad in ["..", "a/b", "/abs", "."] {
            let ron: &'static [u8] = format!(
                r#"(schema_version: 1, id: "sys", star: "s", bodies: ["{bad}"], epoch_note: "k")"#
            )
            .into_bytes()
            .leak();
            let table: &'static [(&'static str, &'static [u8])] =
                Box::leak(Box::new([("sys/system.ron", ron)]));
            let src = EmbeddedSource::new(table);
            let err = load_system_from(&src, Path::new("sys")).unwrap_err();
            assert!(
                matches!(err, LoadError::UnsafeName { .. }),
                "{bad:?} -> {err:?}"
            );
        }
    }

    /// A `BiomeMapRef` legend/png name gets the same gate — the join
    /// is under the body dir, so `..` would escape it.
    #[test]
    fn unsafe_biome_refs_are_rejected() {
        let body = r#"(schema_version: 1, id: "b", display_name: "k", parent: None,
            radius_m: 1.0, mu_m3_per_s2: 1.0, soi_m: None, rotation_period_s: None,
            tilt_rad: 0.0, orbit: None, atmosphere: None, ocean: None,
            terrain: (noise_seed: 0, heightmap: None),
            biome_map: Some((png: "../x.png", legend: "biomes.ron")),
            surface_resources: [], insolation_w_m2: None)"#;
        let table: &'static [(&'static str, &'static [u8])] = Box::leak(Box::new([
            (
                "sys/system.ron",
                br#"(schema_version: 1, id: "sys", star: "b", bodies: ["b"], epoch_note: "k")"#
                    as &[u8],
            ),
            ("sys/b/body.ron", body.as_bytes()),
            // The legend joins (and reads) before the png — it must
            // exist for the png gate to be reached.
            (
                "sys/b/biomes.ron",
                br#"(schema_version: 1, entries: [])"# as &[u8],
            ),
        ]));
        let src = EmbeddedSource::new(table);
        let err = load_system_from(&src, Path::new("sys")).unwrap_err();
        assert!(matches!(err, LoadError::UnsafeName { .. }), "{err:?}");
    }

    /// The `x/` vs `x.ron` prefix collision that made the old
    /// per-level DFS order diverge from `data_set_hash`: byte-wise,
    /// `x.ron` sorts before `x/sub.ron` (`'.' < '/'`); component-wise
    /// `Path` order puts the directory's contents first.
    #[test]
    fn ron_sort_key_orders_like_data_set_hash() {
        let dir = Path::new("data/parts");
        let mut paths = [
            PathBuf::from("data/parts/x/sub.ron"),
            PathBuf::from("data/parts/x.ron"),
            PathBuf::from("data/parts/y.ron"),
        ];
        paths.sort_by_cached_key(|p| ron_sort_key(dir, p));
        assert_eq!(
            paths,
            [
                PathBuf::from("data/parts/x.ron"),
                PathBuf::from("data/parts/x/sub.ron"),
                PathBuf::from("data/parts/y.ron"),
            ]
        );
    }

    /// End-to-end on the embedded source: `ron_files` emits the hash
    /// order across the same collision (its BTreeMap key order alone
    /// would keep `x/` first).
    #[test]
    fn embedded_ron_files_emit_hash_order() {
        let src = EmbeddedSource::new(&[
            ("data/parts/x/sub.ron", b"" as &[u8]),
            ("data/parts/x.ron", b"" as &[u8]),
            ("data/parts/y.ron", b"" as &[u8]),
        ]);
        let files = src.ron_files(Path::new("data/parts")).unwrap();
        assert_eq!(
            files,
            [
                PathBuf::from("data/parts/x.ron"),
                PathBuf::from("data/parts/x/sub.ron"),
                PathBuf::from("data/parts/y.ron"),
            ]
        );
    }

    /// A missing `data/parts` directory reads as an empty set — the
    /// convention the four sibling registry loaders already follow.
    #[test]
    fn missing_parts_dir_loads_empty() {
        let src = EmbeddedSource::new(&[]);
        let loaded = load_parts_from(&src, Path::new("data/parts")).unwrap();
        assert!(loaded.parts.is_empty());
    }
}
