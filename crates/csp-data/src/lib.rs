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
//! - [`ids`] — `SystemId`, `BodyId`, `BiomeId`, `ResourceId`, `LocKey`.
//! - [`system`] — `SystemDef`, `BodyDef`, `KeplerElements`,
//!   `AtmosphereDef`, `OceanDef`, `TerrainDef`, `BiomeMapRef`,
//!   `ConcentrationMapRef`.
//! - [`biome`] — `biomes.ron` legend + indexed-PNG header inspection.
//! - [`loc`] — `en.ron` table.
//! - [`load`] — `load_system(dir)`.
//! - [`validate`] — the §3.0 rule set over a `LoadedSystem`.
//! - [`datahash`] — `data_set_hash(data/)` (§3.1).

/// Version of the data-set hashing scheme (PLAN §3.1). Recorded in the §7.1
/// determinism fingerprint (and later the §7.2 save header) so a scheme
/// change explains — rather than silently red-dening — every golden replay.
///
/// The pinned scheme: BLAKE3-256 over the concatenation, in ascending path
/// order, of `path_bytes || 0x00 || canonicalized_file_bytes` for every file
/// in the hashed set, where canonicalization is LF line endings with
/// trailing whitespace stripped (§3.1). `csp-bench` applies the same
/// construction to a frozen `data/test-fixtures/<scenario>/` directory to
/// produce the replay artifact's `data_fixture_hash` (§7.6).
///
/// Bump this in the same commit that changes the scheme, and say why.
pub const DATA_HASH_VERSION: u32 = 1;

pub mod balance;
pub mod biome;
pub mod datahash;
pub mod ids;
pub mod load;
pub mod loc;
pub mod system;
pub mod validate;

pub use balance::{BALANCE_SCHEMA_VERSION, BalanceDef};
pub use biome::{BiomeLegend, BiomeLegendEntry, PngInfo, inspect_indexed_png};
pub use datahash::{DataHashError, data_set_hash};
pub use ids::{BiomeId, BodyId, LocKey, ResourceId, SystemId};
pub use load::{LoadError, LoadedBiomeMap, LoadedBody, LoadedSystem, load_balance, load_system};
pub use loc::LocFile;
pub use system::{
    AtmosphereDef, BiomeMapRef, BodyDef, ConcentrationMapRef, KeplerElements, OceanDef,
    SCHEMA_VERSION, SystemDef, TemperatureKnot, TerrainDef,
};
pub use validate::{PERIOD_REL_TOL, ValidationError, validate_loc_keys, validate_system};
