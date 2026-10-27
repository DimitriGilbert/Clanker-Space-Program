#![forbid(unsafe_code)]

//! Data definitions, loaders, frozen save schemas + migrations (PLAN §2.1, §7).
//!
//! Owns the RON data schemas every later milestone reads: the §3.0
//! `SystemDef`/`BodyDef` home-system format, the §3.0.1 biome legend +
//! indexed-map pairing, the loc table, the stable string IDs (§3.1), the
//! system directory loader, the §3.0/§8.2 validator, and the §3.1
//! data-set hash. **No sim logic** — nothing here advances time.
//!
//! Determinism-domain rules apply (§2.1/§2.2): no `HashMap`/`HashSet`, no
//! `std::time`, libm-only transcendentals (this crate's `clippy.toml`),
//! and the loader's only I/O is reading the directory it is handed.
//!
//! - [`ids`] — `SystemId`, `BodyId`, `BiomeId`, `ResourceId`, `LocKey`,
//!   `PartId`, `FamilyId`, `VariantId`, `TechNodeId`, `ExperimentId`,
//!   `RecipeId`.
//! - [`system`] — `SystemDef`, `BodyDef`, `KeplerElements`,
//!   `AtmosphereDef`, `OceanDef`, `TerrainDef`, `BiomeMapRef`,
//!   `ConcentrationMapRef`.
//! - [`part`] — `PartDef` and everything it carries: categories,
//!   attach nodes, scaling rules, collider/aero/thermal/structural
//!   defs, variants, `ModuleDef`, and the §3.4 `ReliabilityDef`.
//! - [`techtree`] — `data/techtree/*.ron` fragments: `TechNode`,
//!   `UpgradeTrack`/`UpgradeLevel`, and the merged `TechTree` view.
//! - [`resource`] — the §3.5 resource registry (`resources.ron`).
//! - [`biome`] — `biomes.ron` legend + indexed-PNG header inspection.
//! - [`loc`] — `en.ron` table.
//! - [`load`] — `load_system(dir)`, `load_parts(dir)`,
//!   `load_resources(path)`, `load_balance(path)`.
//! - [`validate`] — the §3.0/§3.1/§8.2 rule sets over `LoadedSystem`,
//!   `LoadedParts`, and `ResourceFile`.
//! - [`datahash`] — `data_set_hash(data/)` (§3.1).

/// Version of the data-set hashing scheme (PLAN §3.1). Recorded in the §7.1
/// determinism fingerprint (and later the §7.2 save header) so a scheme
/// change explains — rather than silently red-dening — every golden replay.
///
/// The pinned scheme: BLAKE3-256 over the concatenation, in ascending path
/// order, of `path_bytes || 0x00 || u64-LE len || canonicalized_file_bytes`
/// for every file in the hashed set, where canonicalization is LF line
/// endings with trailing whitespace stripped (§3.1). `csp-bench` applies
/// the same construction to a frozen `data/test-fixtures/<scenario>/`
/// directory to produce the replay artifact's `data_fixture_hash` (§7.6).
///
/// Bump this in the same commit that changes the scheme, and say why.
///
/// Version 2 (deep-review R3-F3): the content gained its u64-LE length
/// prefix — `path || 0x00 || content` was provably non-injective (file
/// `a` containing `Xb\0Y` hashed identically to files `a`→`X`, `b`→`Y`),
/// so two different data sets could share a fingerprint. `csp-bench`'s
/// `fixture_hash` mirrors the same frame.
pub const DATA_HASH_VERSION: u32 = 2;

pub mod balance;
pub mod biome;
pub mod colony;
pub mod contract;
pub mod datahash;
pub mod experiment;
pub mod ids;
pub mod load;
pub mod loc;
pub mod part;
pub mod recipe;
pub mod resource;
pub mod site;
pub mod system;
pub mod techtree;
pub mod validate;

pub use balance::{BALANCE_SCHEMA_VERSION, BalanceDef};
pub use biome::{
    BiomeLegend, BiomeLegendEntry, IndexedPng, PngInfo, decode_indexed_png, inspect_indexed_png,
};
pub use colony::{COLONY_SCHEMA_VERSION, ColonyDef, ColonyFile, ColonyModuleDef, ColonyStoreInit};
pub use contract::{
    CONTRACT_SCHEMA_VERSION, ContractGenerator, ContractKind, ContractParamSpace, ContractParams,
    ContractPrereq, ContractRegime, ContractRewards, ContractTemplate, ModuleReq, RegimeSpace,
};
pub use datahash::{DataHashError, data_set_hash};
pub use experiment::{EXPERIMENT_SCHEMA_VERSION, ExperimentCap, ExperimentDef};
pub use ids::{
    BiomeId, BodyId, ContractTypeId, ExperimentId, FacilityId, FamilyId, LocKey, PartId, RecipeId,
    ResourceId, SiteId, SystemId, TechNodeId, UpgradeTrackId, VariantId,
};
pub use load::{
    DataSource, EmbeddedSource, FsDataSource, LoadError, LoadedBiomeMap, LoadedBody,
    LoadedContract, LoadedContracts, LoadedExperiment, LoadedExperiments, LoadedPart, LoadedParts,
    LoadedRecipe, LoadedRecipes, LoadedSystem, LoadedTechFile, LoadedTechTree, load_balance,
    load_balance_from, load_colonies, load_colonies_from, load_contracts, load_contracts_from,
    load_experiments, load_experiments_from, load_loc, load_loc_from, load_parts, load_parts_from,
    load_recipes, load_recipes_from, load_resources, load_resources_from, load_sites,
    load_sites_from, load_system, load_system_from, load_techtree, load_techtree_from,
};
pub use loc::LocFile;
pub use part::{
    AeroDef, AeroKnot, AttachNode, ColliderDef, FailureMode, G0, ModuleDef, PART_SCHEMA_VERSION,
    PartCategory, PartDef, PartVariant, PortSize, QualCurve, QualKnot, ReliabilityDef,
    ScaleExponents, ScaleRule, Situation, SituationMask, StatPath, StressKind, StressRule,
    StructuralDef, ThermalDef, ThrottleRange,
};
pub use recipe::{RECIPE_SCHEMA_VERSION, RecipeDef};
pub use resource::{
    BUILD_MATERIALS, RESOURCE_SCHEMA_VERSION, ResourceDef, ResourceFile, is_build_material,
};
pub use site::{SITE_SCHEMA_SUPPORTED, SITE_SCHEMA_VERSION, SiteDef, SiteFile};
pub use system::{
    AtmosphereDef, BiomeMapRef, BodyDef, ConcentrationMapRef, KeplerElements, OceanDef,
    SCHEMA_VERSION, SystemDef, TemperatureKnot, TerrainDef,
};
pub use techtree::{
    TECHTREE_SCHEMA_VERSION, TechNode, TechTree, TechTreeDef, TechUnlocks, UpgradeEffect,
    UpgradeLevel, UpgradeTrack,
};
pub use validate::{
    Dataset, PERIOD_REL_TOL, ValidationError, validate_balance, validate_colonies,
    validate_colony_loc_keys, validate_contract_loc_keys, validate_contracts, validate_dataset,
    validate_experiment_loc_keys, validate_experiments, validate_loc, validate_loc_keys,
    validate_part_loc_keys, validate_parts, validate_recipe_loc_keys, validate_recipes,
    validate_resources, validate_site_loc_keys, validate_sites, validate_system, validate_techtree,
    validate_techtree_loc_keys,
};
