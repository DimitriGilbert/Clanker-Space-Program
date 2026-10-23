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

    /// Depth-first walk, each directory level sorted before recursion —
    /// equivalent to a plain ascending sort of every file path found
    /// (a subdir's contents land exactly at the subdir's sorted
    /// position).
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
        // BTreeMap key order is component-wise ascending — the same
        // order the filesystem walk produces (see FsDataSource's doc).
        Ok(self
            .files
            .keys()
            .filter(|k| k.starts_with(dir) && k.extension().is_some_and(|ext| ext == "ron"))
            .cloned()
            .collect())
    }
}

fn read_ron<T: serde::de::DeserializeOwned>(
    src: &dyn DataSource,
    path: &Path,
) -> Result<T, LoadError> {
    let text = src.read_string(path)?;
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
    let system: SystemDef = read_ron(src, &system_path)?;
    check_version(&system_path, system.schema_version, SCHEMA_VERSION)?;

    let mut bodies = Vec::with_capacity(system.bodies.len());
    for id in &system.bodies {
        let body_dir = dir.join(id.as_str());
        let body_path = body_dir.join("body.ron");
        let def: BodyDef = read_ron(src, &body_path)?;
        check_version(&body_path, def.schema_version, SCHEMA_VERSION)?;

        let biomes = match &def.biome_map {
            None => None,
            Some(r) => {
                let legend_path = body_dir.join(&r.legend);
                let legend: BiomeLegend = read_ron(src, &legend_path)?;
                check_version(&legend_path, legend.schema_version, SCHEMA_VERSION)?;
                let png_path = body_dir.join(&r.png);
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

/// [`load_balance`] against an arbitrary [`DataSource`].
pub fn load_balance_from(src: &dyn DataSource, path: &Path) -> Result<BalanceDef, LoadError> {
    let balance: BalanceDef = read_ron(src, path)?;
    check_version(path, balance.schema_version, BALANCE_SCHEMA_VERSION)?;
    Ok(balance)
}

/// Load `data/resources.ron` — the §3.5 resource registry. File-shape
/// checks only; uniqueness/positivity rules live in
/// [`crate::validate`].
pub fn load_resources(path: &Path) -> Result<ResourceFile, LoadError> {
    load_resources_from(&FsDataSource, path)
}

/// [`load_resources`] against an arbitrary [`DataSource`].
pub fn load_resources_from(src: &dyn DataSource, path: &Path) -> Result<ResourceFile, LoadError> {
    let resources: ResourceFile = read_ron(src, path)?;
    check_version(path, resources.schema_version, RESOURCE_SCHEMA_VERSION)?;
    Ok(resources)
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
    let sites: SiteFile = read_ron(src, path)?;
    // §3.1's "current and previous with explicit defaults": v1 files
    // lack `resource_yields`, which serde reads as the empty list.
    if !SITE_SCHEMA_SUPPORTED.contains(&sites.schema_version) {
        return Err(LoadError::UnsupportedSchemaVersion {
            path: path.to_path_buf(),
            found: sites.schema_version,
            expected: *SITE_SCHEMA_SUPPORTED.last().unwrap_or(&1),
        });
    }
    Ok(sites)
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
    let colonies: ColonyFile = read_ron(src, path)?;
    check_version(path, colonies.schema_version, COLONY_SCHEMA_VERSION)?;
    Ok(colonies)
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
        let def: RecipeDef = read_ron(src, &path)?;
        check_version(&path, def.schema_version, RECIPE_SCHEMA_VERSION)?;
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
    /// The defs keyed by [`ExperimentId`] — the lookup the sim and
    /// validator actually consume. First def wins on a duplicate id;
    /// the validator flags the duplicate loudly.
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
        let def: ExperimentDef = read_ron(src, &path)?;
        check_version(&path, def.schema_version, EXPERIMENT_SCHEMA_VERSION)?;
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
        let def: ContractTemplate = read_ron(src, &path)?;
        check_version(&path, def.schema_version, CONTRACT_SCHEMA_VERSION)?;
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
        let def: TechTreeDef = read_ron(src, &path)?;
        check_version(&path, def.schema_version, TECHTREE_SCHEMA_VERSION)?;
        let rel = path.strip_prefix(dir).unwrap_or(&path).to_path_buf();
        trees.push(LoadedTechFile { def, path: rel });
    }
    Ok(LoadedTechTree { trees })
}

/// Collect every `*.ron` under `dir` recursively, in ascending
/// relative-path order. Deterministic by construction — the walk sorts
/// each directory level, matching `data_set_hash`'s ordering so a
/// reordering of `read_dir` results can never change which def a
/// duplicate-ID diagnostic names first.
fn collect_ron_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), LoadError> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|source| LoadError::Io {
            path: dir.to_path_buf(),
            source,
        })?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            collect_ron_files(&path, out)?;
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
pub fn load_parts(dir: &Path) -> Result<LoadedParts, LoadError> {
    load_parts_from(&FsDataSource, dir)
}

/// [`load_parts`] against an arbitrary [`DataSource`].
pub fn load_parts_from(src: &dyn DataSource, dir: &Path) -> Result<LoadedParts, LoadError> {
    let files = src.ron_files(dir)?;
    let mut parts = Vec::with_capacity(files.len());
    for path in files {
        let def: PartDef = read_ron(src, &path)?;
        check_version(&path, def.schema_version, PART_SCHEMA_VERSION)?;
        let rel = path.strip_prefix(dir).unwrap_or(&path).to_path_buf();
        parts.push(LoadedPart { def, path: rel });
    }
    Ok(LoadedParts { parts })
}
