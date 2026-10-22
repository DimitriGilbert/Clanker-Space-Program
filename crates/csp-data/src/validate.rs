//! The §3.0/§8.2 data validator. Runs over a [`LoadedSystem`] (already
//! parsed by [`crate::load`]) and returns a **typed error list** — never
//! asserts, never exits: CI and the editor both consume the same list.
//!
//! Rules implemented (§3.0 "Validator rules" + §8.2's gate row):
//!
//! - `schema_version` present and equal to [`SCHEMA_VERSION`] on every
//!   file (`system.ron`, each `body.ron`, each `biomes.ron`);
//! - the manifest's body list and the loaded defs agree exactly;
//! - every body's `parent` exists; parent chains are acyclic and
//!   terminate at `system.star`;
//! - a parent's SOI exceeds every child's apoapsis (`a·(1+e)`);
//! - an orbiting body's `period_s` is consistent with the parent's μ and
//!   `a` to 1e-9 relative: `T = 2π√(a³/μ_parent)`;
//! - an atmosphere's `top_m` exceeds its `scale_height_m`, its
//!   temperature table is a usable ≥2-knot piecewise-linear function;
//! - biome legends have unique indices/IDs that fit inside the PNG's
//!   palette, and the map is a real 8-bit palette PNG within
//!   §3.0.1's ≤2048×1024 bound;
//! - field sanity: finite, positive where physics requires — and the
//!   **body**-orbit invariants `a > 0`, `0 ≤ e < 1`. Note the scope:
//!   `KeplerElements` also carries open *vessel* orbits sim-side
//!   (`a < 0` hyperbolic, `a := −p` parabolic, `period_s = +∞` — see the
//!   type's doc); this validator only ever sees authored bodies, which
//!   are bound by construction, so the elliptic range is the right rule
//!   here, not a restriction of the type.
//!
//! §3.1/§3.4 add a second rule set over `data/parts/` and
//! `data/resources.ron` — [`validate_parts`], [`validate_resources`],
//! [`validate_part_loc_keys`]: id↔filename agreement, duplicate IDs,
//! positive masses/sizes, variant consistency (non-empty, unique IDs,
//! positive multipliers, unique stat paths), `ScaleRule` sanity
//! (discrete lists contain the 1.0 baseline; continuous exponents in
//! `[0, 4]`), attach-node well-formedness (unique IDs, unit normals),
//! category-consistent modules (an `Engine` part carries an `Engine`
//! module, …), no `Tank`/`Engine` modules on `Discrete`-scaled parts
//! (a size menu cannot scale their stats), resource references
//! resolved against `resources.ron`, the §3.4 reliability-field
//! ranges, and the M4b thermal Euler-stability bound
//! (`conduction_w_per_k ≤ heat_capacity_j_per_k` — see
//! [`ValidationError::ConductionExceedsCapacity`]).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::biome::{BIOME_MAP_MAX_H, BIOME_MAP_MAX_W, inspect_indexed_png};
use crate::ids::LocKey;
use crate::load::{LoadedPart, LoadedParts, LoadedSystem};
use crate::loc::LocFile;
use crate::part::{ColliderDef, ModuleDef, PART_SCHEMA_VERSION, PartCategory, PartDef, ScaleRule};
use crate::resource::{RESOURCE_SCHEMA_VERSION, ResourceFile};
use crate::system::SCHEMA_VERSION;

/// Relative tolerance for the §3.0 period-consistency rule:
/// `|T_authored − 2π√(a³/μ_parent)| / T_computed ≤ PERIOD_REL_TOL`.
pub const PERIOD_REL_TOL: f64 = 1e-9;

/// One validation failure. `body`/`parent` fields carry the string form
/// of IDs — diagnostics are never hashed, so plain `String`s are fine
/// (same convention as `csp_bench::ReplayError`).
#[derive(Clone, Debug, PartialEq)]
pub enum ValidationError {
    /// A file's `schema_version` differs from [`SCHEMA_VERSION`].
    SchemaVersion {
        /// Which file kind ("system.ron", `body.ron`/`biomes.ron` under a
        /// body dir — the message carries the context).
        file: String,
        /// What the file declared.
        found: u32,
    },
    /// Two loaded `body.ron` files declare the same `id`.
    DuplicateBodyId {
        /// The duplicated ID.
        id: String,
    },
    /// `system.bodies` lists an ID whose def failed to appear (id field
    /// in `body.ron` does not match its manifest entry/directory).
    ListedIdNotDeclared {
        /// The manifest entry with no matching def.
        id: String,
    },
    /// A loaded `body.ron` declares an ID absent from `system.bodies`.
    DeclaredIdNotListed {
        /// The unlisted ID.
        id: String,
    },
    /// `system.star` is not one of the loaded bodies.
    StarNotFound {
        /// The star ID the manifest names.
        id: String,
    },
    /// The star itself declares a parent — roots have none.
    StarHasParent {
        /// The star's ID.
        id: String,
    },
    /// A body other than the star has no parent — a second root.
    RootNotStar {
        /// The parentless non-star body.
        id: String,
    },
    /// `body.parent` names an ID that is not a loaded body.
    ParentNotFound {
        /// The body with the dangling parent ref.
        body: String,
        /// The missing parent ID.
        parent: String,
    },
    /// Following `parent` links from `body` returns to `body` — a cycle.
    ParentCycle {
        /// A body on the cycle.
        body: String,
    },
    /// A body's parent chain terminates at a root that is not the star —
    /// the body is unreachable from the system root (§8.2 "reachability").
    DisconnectedFromStar {
        /// The body whose chain misses the star.
        body: String,
    },
    /// A parented body lacks `orbit` elements.
    OrbitMissing {
        /// The body.
        body: String,
    },
    /// A parentless body carries `orbit` elements — roots do not orbit.
    OrbitOnRoot {
        /// The body.
        body: String,
    },
    /// An ID is empty or contains characters unsafe as a directory name
    /// (`/`, `\`, `.`-only segments). IDs are path components under
    /// `data/systems/`, so they must be tame.
    MalformedId {
        /// The offending ID text.
        id: String,
    },
    /// A numeric field is NaN or infinite.
    NonFinite {
        /// Owning body.
        body: String,
        /// Field name.
        field: &'static str,
    },
    /// A numeric field that must be positive is not.
    NonPositive {
        /// Owning body.
        body: String,
        /// Field name.
        field: &'static str,
        /// The offending value.
        value: f64,
    },
    /// `eccentricity` outside the `[0, 1)` bound the §3.0 rules place on
    /// *authored body* orbits (bodies are bound by construction; the
    /// open-orbit `KeplerElements` contract for vessels lives in
    /// `csp_sim::orbit` and never reaches this validator).
    EccentricityOutOfRange {
        /// The body.
        body: String,
        /// The offending eccentricity.
        e: f64,
    },
    /// §3.0 rule: a child's apoapsis must lie inside its parent's SOI.
    SoiTooSmall {
        /// The parent whose SOI is too small.
        parent: String,
        /// The child poking out of it.
        child: String,
        /// The parent's authored SOI (m).
        soi_m: f64,
        /// The child's apoapsis `a·(1+e)` (m).
        apoapsis_m: f64,
    },
    /// §3.0 rule: `period_s` must match `2π√(a³/μ_parent)` within
    /// [`PERIOD_REL_TOL`] relative.
    PeriodInconsistent {
        /// The body.
        body: String,
        /// The authored `period_s`.
        authored_s: f64,
        /// `2π√(a³/μ_parent)` from the loaded `a` and parent μ.
        computed_s: f64,
    },
    /// §3.0 rule: atmosphere top must exceed scale height.
    AtmosphereTopTooLow {
        /// The body.
        body: String,
        /// Authored `top_m`.
        top_m: f64,
        /// Authored `scale_height_m`.
        scale_height_m: f64,
    },
    /// The atmosphere's temperature table is not a usable
    /// piecewise-linear function.
    BadTemperatureTable {
        /// The body.
        body: String,
        /// Why it failed (stable reason token).
        reason: &'static str,
    },
    /// A biome legend repeats a palette index or a biome ID.
    BiomeLegendDuplicate {
        /// The body.
        body: String,
        /// What was duplicated ("index" or "id") and its value.
        detail: String,
    },
    /// A legend index has no palette entry in the PNG.
    BiomeIndexExceedsPalette {
        /// The body.
        body: String,
        /// The legend index that overflows.
        index: u8,
        /// The PNG's palette size.
        palette_len: usize,
    },
    /// The referenced PNG is not structurally a valid palette PNG.
    BiomeMapPngInvalid {
        /// The body.
        body: String,
        /// [`crate::biome::inspect_indexed_png`]'s reason.
        reason: &'static str,
    },
    /// §3.0.1's size bound: equirect, ≤[`BIOME_MAP_MAX_W`]×[`BIOME_MAP_MAX_H`].
    BiomeMapTooLarge {
        /// The body.
        body: String,
        /// Image width.
        width: u32,
        /// Image height.
        height: u32,
    },
    /// A `LocKey` referenced by the data set does not resolve in the
    /// locale file.
    UnresolvedLocKey {
        /// Where the key was referenced from.
        context: String,
        /// The unresolved key.
        key: String,
    },
    // ---- part defs + resource registry (§3.1, §3.4) ----
    /// Two `data/parts/**` files declare the same part `id`.
    DuplicatePartId {
        /// The duplicated ID.
        id: String,
    },
    /// A def's `id` does not match its file stem — §3.1's "the file is
    /// the ID" convention (`data/parts/<id>.ron`).
    PartIdFileMismatch {
        /// The declared ID.
        id: String,
        /// The file it was declared in (relative to `data/parts/`).
        file: String,
    },
    /// A numeric field in a part or resource file is NaN or infinite.
    PartNonFinite {
        /// Owning part (or `"resources.ron"`).
        part: String,
        /// Field name.
        field: &'static str,
    },
    /// A numeric field in a part or resource file that must be positive
    /// is not (zero is rejected unless the rule says `>= 0` — the
    /// reason is in the field name's doc on [`crate::part`]).
    PartNonPositive {
        /// Owning part (or `"resources.ron"`).
        part: String,
        /// Field name.
        field: &'static str,
        /// The offending value.
        value: f64,
    },
    /// A `ResourceId` a part (or part-bearing file) references is not
    /// in `data/resources.ron`.
    UnknownResource {
        /// The referencing part.
        part: String,
        /// The unknown resource ID.
        resource: String,
    },
    /// A resource ID appears twice where uniqueness is required
    /// (within a tank's capacities, an engine's propellants, a build
    /// bill of materials, or `resources.ron` itself).
    DuplicateResource {
        /// Where the duplicate occurred.
        context: String,
        /// The duplicated resource ID.
        resource: String,
    },
    /// `PartDef::variants` is empty — a `BuiltPart` always names a
    /// variant, so a def must author at least one.
    EmptyVariants {
        /// The part.
        part: String,
    },
    /// Two variants of one part share an ID.
    DuplicateVariantId {
        /// The part.
        part: String,
        /// The duplicated variant ID.
        variant: String,
    },
    /// A variant multiplies the same [`crate::part::StatPath`] twice.
    DuplicateVariantStat {
        /// The part.
        part: String,
        /// The variant.
        variant: String,
        /// The duplicated stat.
        stat: &'static str,
    },
    /// A variant's `cost_multiplier` or stat multiplier is not finite
    /// and positive.
    BadVariantMultiplier {
        /// The part.
        part: String,
        /// The variant.
        variant: String,
        /// `StatPath::name`, or `"cost_multiplier"`.
        stat: &'static str,
        /// The offending value.
        value: f64,
    },
    /// The `scale` rule is malformed: a `Discrete` list empty,
    /// non-positive, unsorted, or missing the `1.0` baseline; or a
    /// `Continuous` range not containing 1.0 / with an exponent outside
    /// `[0, 4]`.
    BadScaleRule {
        /// The part.
        part: String,
        /// Why it failed (stable reason token).
        reason: &'static str,
    },
    /// An attach node is malformed: duplicated/empty ID, non-finite
    /// position, non-unit direction, or non-positive diameter.
    BadAttachNode {
        /// The part.
        part: String,
        /// Node context (the node ID or its index when the ID is
        /// unusable).
        node: String,
        /// Why it failed (stable reason token).
        reason: &'static str,
    },
    /// A part's `category` requires a module it does not carry
    /// (§3.1 category-consistent stats).
    CategoryModuleMismatch {
        /// The part.
        part: String,
        /// The category.
        category: &'static str,
        /// The module(s) the category requires.
        required: &'static str,
    },
    /// A module's own fields are inconsistent or non-physical
    /// (engine Isp ordering, throttle band, propellant fractions,
    /// duplicate engine module, …).
    BadModule {
        /// The part.
        part: String,
        /// Why it failed (stable reason token).
        reason: &'static str,
    },
    /// A `ReliabilityDef` field violates §3.4's stated ranges
    /// (qualification-curve shape, stress-rule multipliers, dormant
    /// stress bounds, failure-mode weights).
    BadReliability {
        /// The part.
        part: String,
        /// Why it failed (stable reason token).
        reason: &'static str,
    },
    /// A `ColliderDef` is degenerate or non-physical.
    BadCollider {
        /// The part.
        part: String,
        /// Why it failed (stable reason token).
        reason: &'static str,
    },
    /// An aero coefficient curve is not a usable piecewise-linear
    /// function (empty, non-increasing Mach, negative/NaN coefficient).
    BadAeroCurve {
        /// The part.
        part: String,
        /// Why it failed (stable reason token).
        reason: &'static str,
    },
    /// §3.4 thermal stability bound: `conduction_w_per_k` must not
    /// exceed `heat_capacity_j_per_k`. M4b integrates the conduction
    /// net with explicit Euler at the pinned `1/64` s tick — stable
    /// while each joint satisfies `dt·G·Σ(1/C) ≤ ~0.03`; a joint's
    /// conductance is the harmonic combination of the two parts' k
    /// (`≤ 2·min(kᵢ, kⱼ)`), so `k ≤ C` on every part bounds every
    /// edge the graph can form.
    ConductionExceedsCapacity {
        /// The part.
        part: String,
        /// Authored `conduction_w_per_k` (W/K).
        conduction_w_per_k: f64,
        /// Authored `heat_capacity_j_per_k` (J/K).
        heat_capacity_j_per_k: f64,
    },
    /// `data/resources.ron` itself is malformed (schema_version aside —
    /// that reports as [`ValidationError::SchemaVersion`]).
    BadResourceTable {
        /// Why it failed (stable reason token).
        reason: &'static str,
    },
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SchemaVersion { file, found } => {
                write!(
                    f,
                    "{file}: schema_version {found}, expected {SCHEMA_VERSION}"
                )
            }
            Self::DuplicateBodyId { id } => write!(f, "duplicate body id {id:?}"),
            Self::ListedIdNotDeclared { id } => {
                write!(f, "manifest lists {id:?} but no body.ron declares it")
            }
            Self::DeclaredIdNotListed { id } => {
                write!(f, "body {id:?} is declared but absent from the manifest")
            }
            Self::StarNotFound { id } => write!(f, "star {id:?} is not a loaded body"),
            Self::StarHasParent { id } => write!(f, "star {id:?} declares a parent"),
            Self::RootNotStar { id } => write!(f, "non-star body {id:?} has no parent"),
            Self::ParentNotFound { body, parent } => {
                write!(f, "{body:?}: parent {parent:?} is not a loaded body")
            }
            Self::ParentCycle { body } => write!(f, "{body:?}: parent chain cycles"),
            Self::DisconnectedFromStar { body } => {
                write!(f, "{body:?}: parent chain does not reach the star")
            }
            Self::OrbitMissing { body } => write!(f, "{body:?}: parented body has no orbit"),
            Self::OrbitOnRoot { body } => write!(f, "{body:?}: root body carries orbit elements"),
            Self::MalformedId { id } => write!(f, "malformed id {id:?}"),
            Self::NonFinite { body, field } => {
                write!(f, "{body:?}.{field}: not finite")
            }
            Self::NonPositive { body, field, value } => {
                write!(f, "{body:?}.{field}: {value} is not positive")
            }
            Self::EccentricityOutOfRange { body, e } => {
                write!(f, "{body:?}: eccentricity {e} outside [0, 1)")
            }
            Self::SoiTooSmall {
                parent,
                child,
                soi_m,
                apoapsis_m,
            } => write!(
                f,
                "{child:?} apoapsis {apoapsis_m} m exceeds parent {parent:?} SOI {soi_m} m"
            ),
            Self::PeriodInconsistent {
                body,
                authored_s,
                computed_s,
            } => write!(
                f,
                "{body:?}: period {authored_s} s vs computed {computed_s} s (rel diff exceeds {PERIOD_REL_TOL:e})"
            ),
            Self::AtmosphereTopTooLow {
                body,
                top_m,
                scale_height_m,
            } => write!(
                f,
                "{body:?}: atmosphere top {top_m} m does not exceed scale height {scale_height_m} m"
            ),
            Self::BadTemperatureTable { body, reason } => {
                write!(f, "{body:?}: temperature table: {reason}")
            }
            Self::BiomeLegendDuplicate { body, detail } => {
                write!(f, "{body:?}: biome legend duplicates {detail}")
            }
            Self::BiomeIndexExceedsPalette {
                body,
                index,
                palette_len,
            } => write!(
                f,
                "{body:?}: biome index {index} >= palette size {palette_len}"
            ),
            Self::BiomeMapPngInvalid { body, reason } => {
                write!(f, "{body:?}: biomes.png invalid: {reason}")
            }
            Self::BiomeMapTooLarge {
                body,
                width,
                height,
            } => write!(
                f,
                "{body:?}: biome map {width}x{height} exceeds {BIOME_MAP_MAX_W}x{BIOME_MAP_MAX_H}"
            ),
            Self::UnresolvedLocKey { context, key } => {
                write!(f, "{context}: loc key {key:?} has no entry in en.ron")
            }
            // Part/resource diagnostics are grouped into `fmt_part` —
            // one arm per variant would triple this function's length.
            _ => self.fmt_part(f),
        }
    }
}

impl ValidationError {
    /// Display for the part/resource rule variants — split out of
    /// [`fmt::Display`] to keep both functions readable (one arm per
    /// variant is the honest shape here). The `_` arm is unreachable:
    /// `fmt`'s catch-all routes only part/resource variants here.
    fn fmt_part(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicatePartId { id } => write!(f, "duplicate part id {id:?}"),
            Self::PartIdFileMismatch { id, file } => {
                write!(f, "part id {id:?} does not match file stem {file:?}")
            }
            Self::PartNonFinite { part, field } => {
                write!(f, "{part:?}.{field}: not finite")
            }
            Self::PartNonPositive { part, field, value } => {
                write!(f, "{part:?}.{field}: {value} is not positive")
            }
            Self::UnknownResource { part, resource } => {
                write!(f, "{part:?}: resource {resource:?} is not in resources.ron")
            }
            Self::DuplicateResource { context, resource } => {
                write!(f, "{context}: duplicate resource id {resource:?}")
            }
            Self::EmptyVariants { part } => {
                write!(
                    f,
                    "{part:?}: variants is empty — author at least a stock variant"
                )
            }
            Self::DuplicateVariantId { part, variant } => {
                write!(f, "{part:?}: duplicate variant id {variant:?}")
            }
            Self::DuplicateVariantStat {
                part,
                variant,
                stat,
            } => {
                write!(f, "{part:?}: variant {variant:?} multiplies {stat} twice")
            }
            Self::BadVariantMultiplier {
                part,
                variant,
                stat,
                value,
            } => {
                write!(
                    f,
                    "{part:?}: variant {variant:?} multiplier on {stat} is {value}, expected finite > 0"
                )
            }
            Self::BadScaleRule { part, reason } => {
                write!(f, "{part:?}: scale rule: {reason}")
            }
            Self::BadAttachNode { part, node, reason } => {
                write!(f, "{part:?}: attach node {node:?}: {reason}")
            }
            Self::CategoryModuleMismatch {
                part,
                category,
                required,
            } => {
                write!(
                    f,
                    "{part:?}: category {category} requires module(s) {required}"
                )
            }
            Self::BadModule { part, reason } => write!(f, "{part:?}: module: {reason}"),
            Self::BadReliability { part, reason } => {
                write!(f, "{part:?}: reliability: {reason}")
            }
            Self::BadCollider { part, reason } => {
                write!(f, "{part:?}: collider: {reason}")
            }
            Self::BadAeroCurve { part, reason } => {
                write!(f, "{part:?}: aero curve: {reason}")
            }
            Self::ConductionExceedsCapacity {
                part,
                conduction_w_per_k,
                heat_capacity_j_per_k,
            } => write!(
                f,
                "{part:?}: conduction {conduction_w_per_k} W/K exceeds heat capacity {heat_capacity_j_per_k} J/K — the explicit-Euler thermal tick would be unstable"
            ),
            Self::BadResourceTable { reason } => {
                write!(f, "resources.ron: {reason}")
            }
            _ => Ok(()),
        }
    }
}

impl std::error::Error for ValidationError {}

/// IDs are directory names under `data/systems/<sys>/` — keep them tame:
/// non-empty, `[a-z0-9._-]` only, never a `.`/`..` segment.
fn id_well_formed(id: &str) -> bool {
    !id.is_empty()
        && id != "."
        && id != ".."
        && id.chars().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '_' || c == '-'
        })
}

fn check_id(id: &str, out: &mut Vec<ValidationError>) {
    if !id_well_formed(id) {
        out.push(ValidationError::MalformedId { id: id.to_string() });
    }
}

fn check_finite(body: &str, field: &'static str, v: f64, out: &mut Vec<ValidationError>) {
    if !v.is_finite() {
        out.push(ValidationError::NonFinite {
            body: body.to_string(),
            field,
        });
    }
}

fn check_positive(body: &str, field: &'static str, v: f64, out: &mut Vec<ValidationError>) {
    if !v.is_finite() {
        out.push(ValidationError::NonFinite {
            body: body.to_string(),
            field,
        });
    } else if v <= 0.0 {
        out.push(ValidationError::NonPositive {
            body: body.to_string(),
            field,
            value: v,
        });
    }
}

/// Body defs keyed by ID — built once by [`build_def_map`] and shared by
/// the graph and orbital rules. `BTreeMap` keeps iteration deterministic.
type DefMap<'a> = BTreeMap<&'a str, &'a crate::load::LoadedBody>;

/// The §3.0 validator over a fully loaded system. Pure: every input is
/// already in memory, so results never depend on the filesystem, the
/// clock, or iteration order of a hash container.
pub fn validate_system(sys: &LoadedSystem) -> Vec<ValidationError> {
    let mut out = Vec::new();
    check_schema_versions(sys, &mut out);
    let defs = build_def_map(sys, &mut out);
    check_body_set(sys, &defs, &mut out);
    check_graph(sys, &defs, &mut out);
    for b in &sys.bodies {
        check_body_fields(b, &mut out);
        check_orbit_rules(b, &defs, &mut out);
        if b.def.atmosphere.is_some() {
            check_atmosphere(b, &mut out);
        }
        if let Some(bm) = &b.biomes {
            check_biomes(b, bm, &mut out);
        }
    }
    out
}

/// §8.2's "schema_version present" row — presence is enforced by serde at
/// parse time; this re-checks the *value* on every loaded file.
fn check_schema_versions(sys: &LoadedSystem, out: &mut Vec<ValidationError>) {
    if sys.system.schema_version != SCHEMA_VERSION {
        out.push(ValidationError::SchemaVersion {
            file: "system.ron".to_string(),
            found: sys.system.schema_version,
        });
    }
    for b in &sys.bodies {
        if b.def.schema_version != SCHEMA_VERSION {
            out.push(ValidationError::SchemaVersion {
                file: format!("{}/body.ron", b.def.id),
                found: b.def.schema_version,
            });
        }
        if let Some(bm) = &b.biomes
            && bm.legend.schema_version != SCHEMA_VERSION
        {
            out.push(ValidationError::SchemaVersion {
                file: format!(
                    "{}/{}",
                    b.def.id,
                    b.def
                        .biome_map
                        .as_ref()
                        .map_or("biomes.ron", |r| r.legend.as_str())
                ),
                found: bm.legend.schema_version,
            });
        }
    }
}

/// Index loaded defs by ID, reporting duplicates.
fn build_def_map<'a>(sys: &'a LoadedSystem, out: &mut Vec<ValidationError>) -> DefMap<'a> {
    let mut defs = DefMap::new();
    for b in &sys.bodies {
        let id = b.def.id.as_str();
        if defs.insert(id, b).is_some() {
            out.push(ValidationError::DuplicateBodyId { id: id.to_string() });
        }
    }
    defs
}

/// Manifest ↔ loaded-set agreement, plus ID hygiene on both sides.
fn check_body_set(sys: &LoadedSystem, defs: &DefMap, out: &mut Vec<ValidationError>) {
    check_id(sys.system.id.as_str(), out);
    check_id(sys.system.star.as_str(), out);
    let mut listed: BTreeSet<&str> = BTreeSet::new();
    for id in &sys.system.bodies {
        listed.insert(id.as_str());
        check_id(id.as_str(), out);
        if !defs.contains_key(id.as_str()) {
            out.push(ValidationError::ListedIdNotDeclared {
                id: id.as_str().to_string(),
            });
        }
    }
    for &id in defs.keys() {
        check_id(id, out);
        if !listed.contains(id) {
            out.push(ValidationError::DeclaredIdNotListed { id: id.to_string() });
        }
    }
}

/// Star shape, parent-exists / orbit-shape checks, acyclicity, and
/// reachability of every body from `system.star`.
fn check_graph(sys: &LoadedSystem, defs: &DefMap, out: &mut Vec<ValidationError>) {
    let star_id = sys.system.star.as_str();
    let star_known = if let Some(star) = defs.get(star_id) {
        if star.def.parent.is_some() {
            out.push(ValidationError::StarHasParent {
                id: star_id.to_string(),
            });
        }
        if star.def.orbit.is_some() {
            out.push(ValidationError::OrbitOnRoot {
                body: star_id.to_string(),
            });
        }
        true
    } else {
        out.push(ValidationError::StarNotFound {
            id: star_id.to_string(),
        });
        false
    };
    for b in &sys.bodies {
        let id = b.def.id.as_str();
        match &b.def.parent {
            None => {
                if id != star_id {
                    out.push(ValidationError::RootNotStar { id: id.to_string() });
                }
            }
            Some(p) => {
                if !defs.contains_key(p.as_str()) {
                    out.push(ValidationError::ParentNotFound {
                        body: id.to_string(),
                        parent: p.as_str().to_string(),
                    });
                }
                if b.def.orbit.is_none() {
                    out.push(ValidationError::OrbitMissing {
                        body: id.to_string(),
                    });
                }
            }
        }
    }
    if star_known {
        check_parent_chains(sys, defs, star_id, out);
    }
}

/// Walk every parent chain: a revisit is a cycle; a resolved chain that
/// terminates anywhere but the star is unreachable (§8.2 "reachability").
fn check_parent_chains(
    sys: &LoadedSystem,
    defs: &DefMap,
    star_id: &str,
    out: &mut Vec<ValidationError>,
) {
    for b in &sys.bodies {
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        let mut cur = b.def.id.as_str();
        seen.insert(cur);
        let mut parent = b.def.parent.as_ref();
        while let Some(p) = parent {
            let pid = p.as_str();
            if !seen.insert(pid) {
                out.push(ValidationError::ParentCycle {
                    body: b.def.id.as_str().to_string(),
                });
                break;
            }
            match defs.get(pid) {
                None => break, // ParentNotFound already reported
                Some(pd) => {
                    cur = pid;
                    parent = pd.def.parent.as_ref();
                }
            }
        }
        // A chain ending on `parent.is_none()` landed on the terminal
        // root `cur`; it should be the star. (Parentless non-star bodies
        // already reported as RootNotStar.)
        if b.def.parent.is_some() && parent.is_none() && cur != star_id {
            out.push(ValidationError::DisconnectedFromStar {
                body: b.def.id.as_str().to_string(),
            });
        }
    }
}

/// Field sanity on one body: finite where required, positive where
/// physics demands it.
fn check_body_fields(b: &crate::load::LoadedBody, out: &mut Vec<ValidationError>) {
    let body = b.def.id.as_str();
    check_positive(body, "radius_m", b.def.radius_m, out);
    check_positive(body, "mu_m3_per_s2", b.def.mu_m3_per_s2, out);
    check_finite(body, "tilt_rad", b.def.tilt_rad, out);
    if let Some(soi) = b.def.soi_m {
        check_positive(body, "soi_m", soi, out);
    }
    if let Some(rot) = b.def.rotation_period_s {
        check_positive(body, "rotation_period_s", rot, out);
    }
    if let Some(ins) = b.def.insolation_w_m2 {
        check_positive(body, "insolation_w_m2", ins, out);
    }
    if let Some(oc) = &b.def.ocean {
        check_positive(body, "ocean.datum_radius_m", oc.datum_radius_m, out);
    }
}

/// Orbit-element sanity plus the two §3.0 orbital rules: child apoapsis
/// inside parent SOI, and `period_s` consistent with `2π√(a³/μ_parent)`
/// to [`PERIOD_REL_TOL`] relative.
fn check_orbit_rules(b: &crate::load::LoadedBody, defs: &DefMap, out: &mut Vec<ValidationError>) {
    let body = b.def.id.as_str();
    let Some(o) = &b.def.orbit else { return };
    check_positive(body, "orbit.semi_major_axis_m", o.semi_major_axis_m, out);
    check_finite(body, "orbit.inclination_rad", o.inclination_rad, out);
    check_finite(body, "orbit.ascending_node_rad", o.ascending_node_rad, out);
    check_finite(body, "orbit.arg_periapsis_rad", o.arg_periapsis_rad, out);
    check_finite(
        body,
        "orbit.mean_anomaly_epoch_rad",
        o.mean_anomaly_epoch_rad,
        out,
    );
    check_positive(body, "orbit.period_s", o.period_s, out);
    if !(0.0..1.0).contains(&o.eccentricity) {
        out.push(ValidationError::EccentricityOutOfRange {
            body: body.to_string(),
            e: o.eccentricity,
        });
    }

    // §3.0 rules need a resolvable, loaded parent.
    let Some(p) = b.def.parent.as_ref().and_then(|p| defs.get(p.as_str())) else {
        return;
    };
    let apoapsis = o.semi_major_axis_m * (1.0 + o.eccentricity);
    if let Some(soi) = p.def.soi_m
        && apoapsis >= soi
    {
        out.push(ValidationError::SoiTooSmall {
            parent: p.def.id.as_str().to_string(),
            child: body.to_string(),
            soi_m: soi,
            apoapsis_m: apoapsis,
        });
    }
    // T = 2π√(a³/μ_parent). libm::sqrt: f64::sqrt is banned by this
    // crate's clippy.toml (§8.2 determinism lints).
    let a = o.semi_major_axis_m;
    let computed = 2.0 * core::f64::consts::PI * libm::sqrt(a * a * a / p.def.mu_m3_per_s2);
    let rel = ((o.period_s - computed) / computed).abs();
    if rel > PERIOD_REL_TOL {
        out.push(ValidationError::PeriodInconsistent {
            body: body.to_string(),
            authored_s: o.period_s,
            computed_s: computed,
        });
    }
}

/// §3.0 atmosphere model: positive model constants, γ > 1, the
/// `top_m > scale_height_m` rule, and a usable temperature table.
fn check_atmosphere(b: &crate::load::LoadedBody, out: &mut Vec<ValidationError>) {
    let Some(atm) = &b.def.atmosphere else { return };
    let body = b.def.id.as_str();
    check_positive(
        body,
        "atmosphere.sea_level_pressure_pa",
        atm.sea_level_pressure_pa,
        out,
    );
    check_positive(body, "atmosphere.scale_height_m", atm.scale_height_m, out);
    check_positive(body, "atmosphere.top_m", atm.top_m, out);
    check_positive(
        body,
        "atmosphere.molar_mass_kg_per_mol",
        atm.molar_mass_kg_per_mol,
        out,
    );
    if !(atm.heat_capacity_ratio.is_finite() && atm.heat_capacity_ratio > 1.0) {
        out.push(ValidationError::NonPositive {
            body: body.to_string(),
            field: "atmosphere.heat_capacity_ratio",
            value: atm.heat_capacity_ratio,
        });
    }
    if atm.top_m <= atm.scale_height_m {
        out.push(ValidationError::AtmosphereTopTooLow {
            body: body.to_string(),
            top_m: atm.top_m,
            scale_height_m: atm.scale_height_m,
        });
    }
    check_temperature_table(b, out);
}

fn check_temperature_table(b: &crate::load::LoadedBody, out: &mut Vec<ValidationError>) {
    let Some(atm) = &b.def.atmosphere else { return };
    let body = b.def.id.as_str();
    let mut fail = |reason: &'static str| {
        out.push(ValidationError::BadTemperatureTable {
            body: body.to_string(),
            reason,
        })
    };
    let knots = &atm.temperature_knots;
    if knots.len() < 2 {
        fail("needs at least 2 knots");
        return;
    }
    let mut prev = f64::NEG_INFINITY;
    for k in knots {
        if !k.altitude_m.is_finite() || !k.temperature_k.is_finite() {
            fail("non-finite knot");
            return;
        }
        if k.altitude_m <= prev {
            fail("altitudes not strictly increasing");
            return;
        }
        if k.altitude_m < 0.0 || k.altitude_m > atm.top_m {
            fail("knot outside [0, top_m]");
            return;
        }
        if k.temperature_k <= 0.0 {
            fail("non-positive temperature");
            return;
        }
        prev = k.altitude_m;
    }
}

fn check_biomes(
    b: &crate::load::LoadedBody,
    bm: &crate::load::LoadedBiomeMap,
    out: &mut Vec<ValidationError>,
) {
    let body = b.def.id.as_str();
    let mut indices = BTreeSet::new();
    let mut ids = BTreeSet::new();
    for e in &bm.legend.entries {
        if !indices.insert(e.index) {
            out.push(ValidationError::BiomeLegendDuplicate {
                body: body.to_string(),
                detail: format!("index {}", e.index),
            });
        }
        if !ids.insert(e.id.as_str()) {
            out.push(ValidationError::BiomeLegendDuplicate {
                body: body.to_string(),
                detail: format!("id {:?}", e.id),
            });
        }
    }
    match inspect_indexed_png(&bm.png) {
        Err(reason) => out.push(ValidationError::BiomeMapPngInvalid {
            body: body.to_string(),
            reason,
        }),
        Ok(info) => {
            if info.width > BIOME_MAP_MAX_W || info.height > BIOME_MAP_MAX_H {
                out.push(ValidationError::BiomeMapTooLarge {
                    body: body.to_string(),
                    width: info.width,
                    height: info.height,
                });
            }
            for e in &bm.legend.entries {
                if usize::from(e.index) >= info.palette_len {
                    out.push(ValidationError::BiomeIndexExceedsPalette {
                        body: body.to_string(),
                        index: e.index,
                        palette_len: info.palette_len,
                    });
                }
            }
        }
    }
}

/// §8.2's "ID stability"/loc-resolution check: every `LocKey` the system
/// references (`epoch_note`, display names, biome names) must resolve in
/// the locale file. Separate from [`validate_system`] because the loc
/// file lives outside the system directory.
pub fn validate_loc_keys(sys: &LoadedSystem, loc: &LocFile) -> Vec<ValidationError> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    let mut check = |context: String, key: &LocKey| {
        if seen.insert(key.as_str().to_string()) && !loc.contains(key) {
            out.push(ValidationError::UnresolvedLocKey {
                context,
                key: key.as_str().to_string(),
            });
        }
    };
    check("system.ron".to_string(), &sys.system.epoch_note);
    for b in &sys.bodies {
        check(format!("{}/body.ron", b.def.id), &b.def.display_name);
        if let Some(bm) = &b.biomes {
            for e in &bm.legend.entries {
                check(format!("{}/biomes.ron", b.def.id), &e.name);
            }
        }
    }
    out
}

// ---------- part defs + resource registry (§3.1, §3.4) ----------

/// Upper bound for [`crate::part::ScaleExponents`] — see the type's
/// doc: exponents beyond 4 are authoring errors, not supported physics.
const SCALE_EXPONENT_MAX: f64 = 4.0;

/// `‖direction‖ = 1` tolerance for attach-node normals.
const NODE_DIR_UNIT_TOL: f64 = 1e-6;

/// Relative/absolute tolerance on the engine propellant-fraction sum
/// (fractions are authored and must total exactly 1 — within float
/// authoring noise).
const PROPELLANT_SUM_TOL: f64 = 1e-6;

/// §3.5's `resources.ron` checks: `schema_version`, unique well-formed
/// IDs, and `mass_per_unit_kg` finite and `≥ 0` (zero is the legal
/// massless-resource case — `ElectricCharge`).
pub fn validate_resources(res: &ResourceFile) -> Vec<ValidationError> {
    let mut out = Vec::new();
    if res.schema_version != RESOURCE_SCHEMA_VERSION {
        out.push(ValidationError::SchemaVersion {
            file: "resources.ron".to_string(),
            found: res.schema_version,
        });
    }
    let mut seen = BTreeSet::new();
    for r in &res.resources {
        check_id(r.id.as_str(), &mut out);
        if !seen.insert(r.id.as_str()) {
            out.push(ValidationError::DuplicateResource {
                context: "resources.ron".to_string(),
                resource: r.id.as_str().to_string(),
            });
        }
        if !r.mass_per_unit_kg.is_finite() {
            out.push(ValidationError::PartNonFinite {
                part: "resources.ron".to_string(),
                field: "mass_per_unit_kg",
            });
        } else if r.mass_per_unit_kg < 0.0 {
            out.push(ValidationError::PartNonPositive {
                part: "resources.ron".to_string(),
                field: "mass_per_unit_kg",
                value: r.mass_per_unit_kg,
            });
        }
    }
    out
}

fn part_positive(part: &str, field: &'static str, v: f64, out: &mut Vec<ValidationError>) {
    if !v.is_finite() {
        out.push(ValidationError::PartNonFinite {
            part: part.to_string(),
            field,
        });
    } else if v <= 0.0 {
        out.push(ValidationError::PartNonPositive {
            part: part.to_string(),
            field,
            value: v,
        });
    }
}

/// `(resource, amount)` list checks shared by `build_resources`,
/// `Tank.capacity`, and `Engine.propellants`: IDs unique, known to
/// `resources.ron`, amounts finite and positive.
fn check_resource_list(
    part: &str,
    context: &'static str,
    list: &[(crate::ids::ResourceId, f64)],
    res: &ResourceFile,
    out: &mut Vec<ValidationError>,
) {
    let mut seen = BTreeSet::new();
    for (id, amount) in list {
        if !seen.insert(id.as_str()) {
            out.push(ValidationError::DuplicateResource {
                context: format!("{part} {context}"),
                resource: id.as_str().to_string(),
            });
        }
        if !res.contains(id) {
            out.push(ValidationError::UnknownResource {
                part: part.to_string(),
                resource: id.as_str().to_string(),
            });
        }
        part_positive(part, context, *amount, out);
    }
}

fn check_vec3_finite(
    part: &str,
    field: &'static str,
    v: &csp_core::DVec3,
    out: &mut Vec<ValidationError>,
) {
    if !(v.x.is_finite() && v.y.is_finite() && v.z.is_finite()) {
        out.push(ValidationError::PartNonFinite {
            part: part.to_string(),
            field,
        });
    }
}

fn check_scale_rule(part: &str, scale: &ScaleRule, out: &mut Vec<ValidationError>) {
    match scale {
        ScaleRule::None => {}
        ScaleRule::Discrete(factors) => {
            if factors.is_empty() {
                out.push(ValidationError::BadScaleRule {
                    part: part.to_string(),
                    reason: "discrete list is empty",
                });
                return;
            }
            let mut prev = 0.0;
            for f in factors {
                if !f.is_finite() || *f <= 0.0 {
                    out.push(ValidationError::BadScaleRule {
                        part: part.to_string(),
                        reason: "discrete factor non-positive or non-finite",
                    });
                    return;
                }
                if *f <= prev {
                    out.push(ValidationError::BadScaleRule {
                        part: part.to_string(),
                        reason: "discrete factors not strictly increasing",
                    });
                    return;
                }
                prev = *f;
            }
            if !factors.contains(&1.0) {
                out.push(ValidationError::BadScaleRule {
                    part: part.to_string(),
                    reason: "discrete list lacks the 1.0 baseline",
                });
            }
        }
        ScaleRule::Continuous {
            min,
            max,
            exponents,
        } => {
            if !(min.is_finite() && max.is_finite()) {
                out.push(ValidationError::BadScaleRule {
                    part: part.to_string(),
                    reason: "range bound non-finite",
                });
            } else if !(*min > 0.0 && *min <= 1.0 && *max >= 1.0) {
                out.push(ValidationError::BadScaleRule {
                    part: part.to_string(),
                    reason: "range must satisfy 0 < min <= 1 <= max",
                });
            }
            for (name, e) in [
                ("mass", exponents.mass),
                ("cost", exponents.cost),
                ("thrust", exponents.thrust),
                ("capacity", exponents.capacity),
                ("structural", exponents.structural),
            ] {
                if !(e.is_finite() && (0.0..=SCALE_EXPONENT_MAX).contains(&e)) {
                    out.push(ValidationError::BadScaleRule {
                        part: part.to_string(),
                        reason: match name {
                            "mass" => "mass exponent outside [0, 4]",
                            "cost" => "cost exponent outside [0, 4]",
                            "thrust" => "thrust exponent outside [0, 4]",
                            "capacity" => "capacity exponent outside [0, 4]",
                            _ => "structural exponent outside [0, 4]",
                        },
                    });
                }
            }
        }
    }
}

fn check_attach_nodes(part: &PartDef, out: &mut Vec<ValidationError>) {
    let part_id = part.id.as_str();
    if part.attach_nodes.is_empty() {
        out.push(ValidationError::BadAttachNode {
            part: part_id.to_string(),
            node: "(part)".to_string(),
            reason: "part declares no attach nodes",
        });
    }
    let mut seen = BTreeSet::new();
    for (i, n) in part.attach_nodes.iter().enumerate() {
        let node = if n.id.is_empty() {
            format!("#{i}")
        } else {
            n.id.clone()
        };
        if n.id.is_empty() {
            out.push(ValidationError::BadAttachNode {
                part: part_id.to_string(),
                node: node.clone(),
                reason: "empty node id",
            });
        } else if !seen.insert(n.id.as_str()) {
            out.push(ValidationError::BadAttachNode {
                part: part_id.to_string(),
                node: node.clone(),
                reason: "duplicate node id",
            });
        }
        check_vec3_finite(part_id, "attach_nodes.position_m", &n.position_m, out);
        let len = libm::sqrt(
            n.direction.x * n.direction.x
                + n.direction.y * n.direction.y
                + n.direction.z * n.direction.z,
        );
        if !(len.is_finite() && (len - 1.0).abs() <= NODE_DIR_UNIT_TOL) {
            out.push(ValidationError::BadAttachNode {
                part: part_id.to_string(),
                node,
                reason: "direction is not unit length",
            });
        }
        part_positive(part_id, "attach_nodes.diameter_m", n.diameter_m, out);
    }
}

fn check_collider(part: &str, collider: &ColliderDef, out: &mut Vec<ValidationError>) {
    match collider {
        ColliderDef::Box { half_extents_m } => {
            for v in [half_extents_m.x, half_extents_m.y, half_extents_m.z] {
                part_positive(part, "collider.half_extents_m", v, out);
            }
        }
        ColliderDef::Cylinder {
            radius_m,
            half_height_m,
        } => {
            part_positive(part, "collider.radius_m", *radius_m, out);
            part_positive(part, "collider.half_height_m", *half_height_m, out);
        }
        ColliderDef::Convex { points } => {
            if points.len() < 4 {
                out.push(ValidationError::BadCollider {
                    part: part.to_string(),
                    reason: "convex hull needs at least 4 points",
                });
            }
            for p in points {
                check_vec3_finite(part, "collider.points", p, out);
            }
        }
    }
}

fn check_aero(part: &PartDef, out: &mut Vec<ValidationError>) {
    let part_id = part.id.as_str();
    for curve in [&part.aero.cd_curve, &part.aero.cl_curve] {
        if curve.is_empty() {
            out.push(ValidationError::BadAeroCurve {
                part: part_id.to_string(),
                reason: "curve is empty",
            });
            continue;
        }
        let mut prev = f64::NEG_INFINITY;
        for k in curve {
            if !k.mach.is_finite() || !k.coeff.is_finite() {
                out.push(ValidationError::BadAeroCurve {
                    part: part_id.to_string(),
                    reason: "non-finite knot",
                });
                break;
            }
            if k.mach < 0.0 {
                out.push(ValidationError::BadAeroCurve {
                    part: part_id.to_string(),
                    reason: "negative mach",
                });
                break;
            }
            if k.mach <= prev {
                out.push(ValidationError::BadAeroCurve {
                    part: part_id.to_string(),
                    reason: "mach not strictly increasing",
                });
                break;
            }
            if k.coeff < 0.0 {
                out.push(ValidationError::BadAeroCurve {
                    part: part_id.to_string(),
                    reason: "negative coefficient",
                });
                break;
            }
            prev = k.mach;
        }
    }
    part_positive(part_id, "aero.ref_len_m", part.aero.ref_len_m, out);
}

fn check_variants(part: &PartDef, out: &mut Vec<ValidationError>) {
    let part_id = part.id.as_str();
    if part.variants.is_empty() {
        out.push(ValidationError::EmptyVariants {
            part: part_id.to_string(),
        });
    }
    let mut seen = BTreeSet::new();
    for v in &part.variants {
        check_id(v.id.as_str(), out);
        if !seen.insert(v.id.as_str()) {
            out.push(ValidationError::DuplicateVariantId {
                part: part_id.to_string(),
                variant: v.id.as_str().to_string(),
            });
        }
        if !(v.cost_multiplier.is_finite() && v.cost_multiplier > 0.0) {
            out.push(ValidationError::BadVariantMultiplier {
                part: part_id.to_string(),
                variant: v.id.as_str().to_string(),
                stat: "cost_multiplier",
                value: v.cost_multiplier,
            });
        }
        let mut stats = BTreeSet::new();
        for (stat, m) in &v.stat_multipliers {
            if !stats.insert(*stat) {
                out.push(ValidationError::DuplicateVariantStat {
                    part: part_id.to_string(),
                    variant: v.id.as_str().to_string(),
                    stat: stat.name(),
                });
            }
            if !(m.is_finite() && *m > 0.0) {
                out.push(ValidationError::BadVariantMultiplier {
                    part: part_id.to_string(),
                    variant: v.id.as_str().to_string(),
                    stat: stat.name(),
                    value: *m,
                });
            }
        }
    }
}

/// Per-module field checks — dispatch is in [`check_modules`]; the
/// engine's checks live in [`check_engine_module`].
fn check_module(part_id: &str, res: &ResourceFile, m: &ModuleDef, out: &mut Vec<ValidationError>) {
    match m {
        ModuleDef::Engine { .. } => check_engine_module(part_id, res, m, out),
        ModuleDef::Tank { capacity } => {
            if capacity.is_empty() {
                out.push(ValidationError::BadModule {
                    part: part_id.to_string(),
                    reason: "tank declares no capacities",
                });
            } else {
                check_resource_list(part_id, "tank.capacity", capacity, res, out);
            }
        }
        ModuleDef::Control { torque_nm, power_w } => {
            if !(torque_nm.is_finite() && *torque_nm >= 0.0) {
                out.push(ValidationError::BadModule {
                    part: part_id.to_string(),
                    reason: "control torque_nm negative or non-finite",
                });
            }
            if !(power_w.is_finite() && *power_w >= 0.0) {
                out.push(ValidationError::BadModule {
                    part: part_id.to_string(),
                    reason: "control power_w negative or non-finite",
                });
            }
        }
        ModuleDef::Decoupler => {}
        ModuleDef::DockPort { .. } => {}
        ModuleDef::CrewSeat { slots } => {
            if *slots == 0 {
                out.push(ValidationError::BadModule {
                    part: part_id.to_string(),
                    reason: "crew seat with zero slots",
                });
            }
        }
        ModuleDef::Experiment {
            data_units,
            situations,
            ..
        } => {
            part_positive(part_id, "experiment.data_units", *data_units, out);
            if situations.is_empty() {
                out.push(ValidationError::BadModule {
                    part: part_id.to_string(),
                    reason: "experiment can run in no situation",
                });
            }
        }
        ModuleDef::Drill { .. } => check_drill_module(part_id, res, m, out),
        ModuleDef::Converter { power_w, .. } => {
            if !(power_w.is_finite() && *power_w >= 0.0) {
                out.push(ValidationError::BadModule {
                    part: part_id.to_string(),
                    reason: "converter power_w negative or non-finite",
                });
            }
        }
        ModuleDef::Lab {
            throughput_data_per_s,
            multiplier,
            researcher_slots,
        } => {
            part_positive(
                part_id,
                "lab.throughput_data_per_s",
                *throughput_data_per_s,
                out,
            );
            part_positive(part_id, "lab.multiplier", *multiplier, out);
            if *researcher_slots == 0 {
                out.push(ValidationError::BadModule {
                    part: part_id.to_string(),
                    reason: "lab with zero researcher slots",
                });
            }
        }
    }
}

/// The `Drill` module's rules: a known resource, positive rate,
/// `min_concentration ∈ [0, 1]`, non-negative power draw.
fn check_drill_module(
    part_id: &str,
    res: &ResourceFile,
    m: &ModuleDef,
    out: &mut Vec<ValidationError>,
) {
    let ModuleDef::Drill {
        resource,
        base_rate_per_s,
        min_concentration,
        power_w,
    } = m
    else {
        return;
    };
    if !res.contains(resource) {
        out.push(ValidationError::UnknownResource {
            part: part_id.to_string(),
            resource: resource.as_str().to_string(),
        });
    }
    part_positive(part_id, "drill.base_rate_per_s", *base_rate_per_s, out);
    if !(min_concentration.is_finite() && (0.0..=1.0).contains(min_concentration)) {
        out.push(ValidationError::BadModule {
            part: part_id.to_string(),
            reason: "drill min_concentration outside [0, 1]",
        });
    }
    if !(power_w.is_finite() && *power_w >= 0.0) {
        out.push(ValidationError::BadModule {
            part: part_id.to_string(),
            reason: "drill power_w negative or non-finite",
        });
    }
}

/// The `Engine` module's own rules: positive thrust/Isp, the
/// `isp_sl ≤ isp_vac` ordering, a sane throttle band, non-degenerate
/// gimbal/ignition/rated-burn fields, and propellant fractions that
/// are unique, known, positive, and total 1.
fn check_engine_module(
    part_id: &str,
    res: &ResourceFile,
    m: &ModuleDef,
    out: &mut Vec<ValidationError>,
) {
    let ModuleDef::Engine {
        thrust_vac_n,
        isp_vac_s,
        isp_sl_s,
        throttle_range,
        gimbal_deg,
        ignitions,
        rated_burn_s,
        propellants,
        ullage_required: _,
    } = m
    else {
        return;
    };
    part_positive(part_id, "engine.thrust_vac_n", *thrust_vac_n, out);
    part_positive(part_id, "engine.isp_vac_s", *isp_vac_s, out);
    part_positive(part_id, "engine.isp_sl_s", *isp_sl_s, out);
    if isp_sl_s.is_finite() && isp_vac_s.is_finite() && isp_sl_s > isp_vac_s {
        out.push(ValidationError::BadModule {
            part: part_id.to_string(),
            reason: "isp_sl_s exceeds isp_vac_s",
        });
    }
    if !(throttle_range.min.is_finite()
        && throttle_range.max.is_finite()
        && throttle_range.min >= 0.0
        && throttle_range.min < throttle_range.max
        && throttle_range.max <= 1.0)
    {
        out.push(ValidationError::BadModule {
            part: part_id.to_string(),
            reason: "throttle_range outside 0 <= min < max <= 1",
        });
    }
    if !(gimbal_deg.is_finite() && *gimbal_deg >= 0.0) {
        out.push(ValidationError::BadModule {
            part: part_id.to_string(),
            reason: "gimbal_deg negative or non-finite",
        });
    }
    if let Some(n) = ignitions
        && *n == 0
    {
        out.push(ValidationError::BadModule {
            part: part_id.to_string(),
            reason: "ignitions is Some(0) — use None for unlimited",
        });
    }
    part_positive(part_id, "engine.rated_burn_s", *rated_burn_s, out);
    if propellants.is_empty() {
        out.push(ValidationError::BadModule {
            part: part_id.to_string(),
            reason: "engine declares no propellants",
        });
    } else {
        check_resource_list(part_id, "engine.propellants", propellants, res, out);
        let sum: f64 = propellants.iter().map(|(_, f)| f).sum();
        if sum.is_finite() && (sum - 1.0).abs() > PROPELLANT_SUM_TOL {
            out.push(ValidationError::BadModule {
                part: part_id.to_string(),
                reason: "propellant mass fractions do not sum to 1",
            });
        }
    }
}

fn check_modules(part: &PartDef, res: &ResourceFile, out: &mut Vec<ValidationError>) {
    let part_id = part.id.as_str();
    for m in &part.modules {
        check_module(part_id, res, m, out);
    }
    let has = |pred: fn(&ModuleDef) -> bool| part.modules.iter().any(pred);
    let has_engine = has(|m| matches!(m, ModuleDef::Engine { .. }));
    let has_tank = has(|m| matches!(m, ModuleDef::Tank { .. }));
    let has_crew_or_control =
        has(|m| matches!(m, ModuleDef::CrewSeat { .. } | ModuleDef::Control { .. }));
    let has_tank_or_dock =
        has(|m| matches!(m, ModuleDef::Tank { .. } | ModuleDef::DockPort { .. }));
    let has_experiment = has(|m| matches!(m, ModuleDef::Experiment { .. }));
    if part
        .modules
        .iter()
        .filter(|m| matches!(m, ModuleDef::Engine { .. }))
        .count()
        > 1
    {
        out.push(ValidationError::BadModule {
            part: part_id.to_string(),
            reason: "more than one engine module",
        });
    }
    // `ScaleRule::Discrete` carries no exponents, so the resolver
    // scales a discrete part's geometry only — a `Tank` or `Engine`
    // module on one would advertise sizes whose stats silently do not
    // scale. Forbidden outright (parts that need stat scaling author
    // `Continuous`); passive modules stay legal.
    if matches!(part.scale, ScaleRule::Discrete(_)) && (has_engine || has_tank) {
        out.push(ValidationError::BadModule {
            part: part_id.to_string(),
            reason: "discrete scaling carries no exponents — tank/engine stats would not scale",
        });
    }
    // M6-B: a `DockPort`'s `node` must name one of the part's attach
    // nodes — it is the docking face the sim anchors soft-capture at —
    // and two port modules may not claim the same node (one face, one
    // capture point; a second face authors a second node).
    let mut port_nodes = BTreeSet::new();
    for m in &part.modules {
        if let ModuleDef::DockPort { node, .. } = m {
            if !part.attach_nodes.iter().any(|n| &n.id == node) {
                out.push(ValidationError::BadModule {
                    part: part_id.to_string(),
                    reason: "dock_port node names no attach node",
                });
            } else if !port_nodes.insert(node.as_str()) {
                out.push(ValidationError::BadModule {
                    part: part_id.to_string(),
                    reason: "two dock ports claim the same attach node",
                });
            }
        }
    }
    // §3.1 category-consistent stats: the category names the module the
    // part must carry (mapping documented on PartCategory).
    let required: Option<&'static str> = match part.category {
        PartCategory::Engine if !has_engine => Some("engine"),
        PartCategory::FuelTank if !has_tank => Some("tank"),
        PartCategory::Control if !has_crew_or_control => Some("crew_seat or control"),
        PartCategory::Payload if !has_tank_or_dock => Some("tank or dock_port"),
        PartCategory::Science if !has_experiment => Some("experiment"),
        _ => None,
    };
    if let Some(required) = required {
        out.push(ValidationError::CategoryModuleMismatch {
            part: part_id.to_string(),
            category: part.category.name(),
            required,
        });
    }
}

fn check_reliability(part: &PartDef, out: &mut Vec<ValidationError>) {
    let part_id = part.id.as_str();
    let r = &part.reliability;
    part_positive(part_id, "reliability.base_mtbf_s", r.base_mtbf_s, out);
    if !(r.infant_mortality.is_finite() && (0.0..1.0).contains(&r.infant_mortality)) {
        out.push(ValidationError::BadReliability {
            part: part_id.to_string(),
            reason: "infant_mortality outside [0, 1)",
        });
    }
    if !(r.dormant_stress.is_finite() && (0.0..=1.0).contains(&r.dormant_stress)) {
        out.push(ValidationError::BadReliability {
            part: part_id.to_string(),
            reason: "dormant_stress outside [0, 1]",
        });
    }
    let knots = &r.qualification_curve.knots;
    if knots.is_empty() {
        out.push(ValidationError::BadReliability {
            part: part_id.to_string(),
            reason: "qualification curve is empty",
        });
    }
    let mut prev = f64::NEG_INFINITY;
    for k in knots {
        if !k.qualification_units.is_finite() || !k.hazard_multiplier.is_finite() {
            out.push(ValidationError::BadReliability {
                part: part_id.to_string(),
                reason: "non-finite qualification knot",
            });
            break;
        }
        if k.qualification_units < 0.0 || k.qualification_units <= prev {
            out.push(ValidationError::BadReliability {
                part: part_id.to_string(),
                reason: "qualification units negative or not strictly increasing",
            });
            break;
        }
        if !(0.0..=1.0).contains(&k.hazard_multiplier) || k.hazard_multiplier <= 0.0 {
            out.push(ValidationError::BadReliability {
                part: part_id.to_string(),
                reason: "hazard multiplier outside (0, 1]",
            });
            break;
        }
        prev = k.qualification_units;
    }
    for s in &r.regime_stress {
        if !(s.threshold.is_finite() && s.threshold >= 0.0) {
            out.push(ValidationError::BadReliability {
                part: part_id.to_string(),
                reason: "stress threshold negative or non-finite",
            });
        }
        if !(s.multiplier.is_finite() && s.multiplier >= 1.0) {
            out.push(ValidationError::BadReliability {
                part: part_id.to_string(),
                reason: "stress multiplier below 1 — stress raises hazard",
            });
        }
    }
    if r.failure_modes.is_empty() {
        out.push(ValidationError::BadReliability {
            part: part_id.to_string(),
            reason: "no failure modes",
        });
    }
    for (_, w) in &r.failure_modes {
        if !(w.is_finite() && *w > 0.0) {
            out.push(ValidationError::BadReliability {
                part: part_id.to_string(),
                reason: "failure-mode weight not positive",
            });
        }
    }
}

/// §3.1 part rules over the whole `data/parts/` set: per-def field and
/// module checks plus set-level ID uniqueness and id↔filename
/// agreement. `res` resolves every `ResourceId` a part references
/// (`build_resources`, tank capacities, propellants, drill outputs).
pub fn validate_parts(loaded: &LoadedParts, res: &ResourceFile) -> Vec<ValidationError> {
    let mut out = Vec::new();
    let mut ids = BTreeSet::new();
    for p in &loaded.parts {
        check_part(p, res, &mut out);
        let id = p.def.id.as_str();
        check_id(id, &mut out);
        if !ids.insert(id) {
            out.push(ValidationError::DuplicatePartId { id: id.to_string() });
        }
        let stem = p
            .path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        if stem != id {
            out.push(ValidationError::PartIdFileMismatch {
                id: id.to_string(),
                file: p.path.display().to_string(),
            });
        }
    }
    out
}

/// All per-def checks for one loaded part.
fn check_part(p: &LoadedPart, res: &ResourceFile, out: &mut Vec<ValidationError>) {
    let part = &p.def;
    let part_id = part.id.as_str();
    if part.schema_version != PART_SCHEMA_VERSION {
        out.push(ValidationError::SchemaVersion {
            file: format!("parts/{}", p.path.display()),
            found: part.schema_version,
        });
    }
    check_id(part.family.as_str(), out);
    check_id(part.tech_requirement.as_str(), out);
    part_positive(part_id, "mass_dry_kg", part.mass_dry_kg, out);
    part_positive(part_id, "cost_funds", part.cost_funds, out);
    for v in [part.size_m.x, part.size_m.y, part.size_m.z] {
        part_positive(part_id, "size_m", v, out);
    }
    check_resource_list(part_id, "build_resources", &part.build_resources, res, out);
    check_attach_nodes(part, out);
    check_scale_rule(part_id, &part.scale, out);
    check_collider(part_id, &part.collider, out);
    check_aero(part, out);
    part_positive(
        part_id,
        "thermal.heat_capacity_j_per_k",
        part.thermal.heat_capacity_j_per_k,
        out,
    );
    part_positive(part_id, "thermal.max_temp_k", part.thermal.max_temp_k, out);
    if !(part.thermal.emissivity.is_finite()
        && part.thermal.emissivity > 0.0
        && part.thermal.emissivity <= 1.0)
    {
        out.push(ValidationError::PartNonPositive {
            part: part_id.to_string(),
            field: "thermal.emissivity",
            value: part.thermal.emissivity,
        });
    }
    if !(part.thermal.conduction_w_per_k.is_finite() && part.thermal.conduction_w_per_k >= 0.0) {
        out.push(ValidationError::PartNonPositive {
            part: part_id.to_string(),
            field: "thermal.conduction_w_per_k",
            value: part.thermal.conduction_w_per_k,
        });
    }
    // §3.4's explicit-Euler stability bound: a joint's conductance is
    // the harmonic combination of the two parts' `conduction_w_per_k`
    // (≤ 2·min(kᵢ, kⱼ)), so `k ≤ C` on every part keeps every edge's
    // dt·G·Σ(1/C) at ~0.03 or below — authored data cannot build an
    // unstable net. Gated on the scalar checks passing so one bad
    // field reports exactly one error.
    if part.thermal.heat_capacity_j_per_k.is_finite()
        && part.thermal.heat_capacity_j_per_k > 0.0
        && part.thermal.conduction_w_per_k > part.thermal.heat_capacity_j_per_k
    {
        out.push(ValidationError::ConductionExceedsCapacity {
            part: part_id.to_string(),
            conduction_w_per_k: part.thermal.conduction_w_per_k,
            heat_capacity_j_per_k: part.thermal.heat_capacity_j_per_k,
        });
    }
    part_positive(
        part_id,
        "structural.max_axial_n",
        part.structural.max_axial_n,
        out,
    );
    part_positive(
        part_id,
        "structural.max_shear_n",
        part.structural.max_shear_n,
        out,
    );
    part_positive(
        part_id,
        "structural.max_torque_nm",
        part.structural.max_torque_nm,
        out,
    );
    check_variants(part, out);
    check_modules(part, res, out);
    check_reliability(part, out);
}

/// Loc resolution for the part set (§8.2 "ID stability" row): part
/// display names, variant names, and `resources.ron` display names
/// must all resolve in `en.ron`. Separate from [`validate_parts`]
/// because the loc file lives outside the parts directory.
pub fn validate_part_loc_keys(
    loaded: &LoadedParts,
    res: &ResourceFile,
    loc: &LocFile,
) -> Vec<ValidationError> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    let mut check = |context: String, key: &LocKey| {
        if seen.insert(key.as_str().to_string()) && !loc.contains(key) {
            out.push(ValidationError::UnresolvedLocKey {
                context,
                key: key.as_str().to_string(),
            });
        }
    };
    for p in &loaded.parts {
        check(format!("parts/{}", p.def.id), &p.def.display_name);
        for v in &p.def.variants {
            check(format!("parts/{} variant", p.def.id), &v.display_name);
        }
    }
    for r in &res.resources {
        check("resources.ron".to_string(), &r.display_name);
    }
    out
}

// §2.13's no-panic rule covers shipping fallible paths; tests signal
// failure by panicking anyway, so unwrap/expect are allowed here.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::biome::BiomeLegendEntry;
    use crate::ids::{BiomeId, SystemId};
    use crate::load::{LoadedBiomeMap, LoadedBody};
    use crate::system::{
        AtmosphereDef, BodyDef, KeplerElements, OceanDef, SystemDef, TemperatureKnot, TerrainDef,
    };

    /// A minimal but *valid* two-body system (star + one planet) — every
    /// negative test mutates this fixture so exactly one rule fires.
    fn fixture() -> LoadedSystem {
        let mu_star = 1.0e14;
        let a = 1.0e9;
        let period = 2.0 * core::f64::consts::PI * libm::sqrt(a * a * a / mu_star);
        let star = LoadedBody {
            def: BodyDef {
                schema_version: SCHEMA_VERSION,
                id: "star".into(),
                display_name: "body.star.name".into(),
                parent: None,
                radius_m: 1.0e6,
                mu_m3_per_s2: mu_star,
                soi_m: None,
                rotation_period_s: Some(100_000.0),
                tilt_rad: 0.0,
                orbit: None,
                atmosphere: None,
                ocean: None,
                terrain: TerrainDef {
                    noise_seed: 1,
                    heightmap: None,
                },
                biome_map: None,
                surface_resources: Vec::new(),
                insolation_w_m2: Some(1361.0),
            },
            biomes: None,
        };
        let planet = LoadedBody {
            def: BodyDef {
                schema_version: SCHEMA_VERSION,
                id: "planet".into(),
                display_name: "body.planet.name".into(),
                parent: Some("star".into()),
                radius_m: 1.0e5,
                mu_m3_per_s2: 1.0e11,
                soi_m: Some(1.0e8),
                rotation_period_s: None, // tidally locked is legal
                tilt_rad: 0.0,
                orbit: Some(KeplerElements {
                    semi_major_axis_m: a,
                    eccentricity: 0.0,
                    inclination_rad: 0.0,
                    ascending_node_rad: 0.0,
                    arg_periapsis_rad: 0.0,
                    mean_anomaly_epoch_rad: 0.0,
                    period_s: period,
                }),
                atmosphere: None,
                ocean: Some(OceanDef {
                    datum_radius_m: 1.0e5,
                }),
                terrain: TerrainDef {
                    noise_seed: 2,
                    heightmap: None,
                },
                biome_map: None,
                surface_resources: Vec::new(),
                insolation_w_m2: Some(100.0),
            },
            biomes: None,
        };
        LoadedSystem {
            system: SystemDef {
                schema_version: SCHEMA_VERSION,
                id: SystemId("test".into()),
                star: "star".into(),
                bodies: vec!["star".into(), "planet".into()],
                epoch_note: "system.test.epoch_note".into(),
            },
            bodies: vec![star, planet],
        }
    }

    fn planet(sys: &mut LoadedSystem) -> &mut BodyDef {
        &mut sys.bodies[1].def
    }

    /// The fixture itself must be clean — otherwise the negative tests
    /// below attribute nothing.
    #[test]
    fn fixture_is_valid() {
        assert!(validate_system(&fixture()).is_empty());
    }

    #[test]
    fn soi_below_child_apoapsis_is_rejected() {
        let mut sys = fixture();
        // Star's SOI smaller than the planet's 1e9 m apoapsis.
        sys.bodies[0].def.soi_m = Some(5.0e8);
        let errs = validate_system(&sys);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::SoiTooSmall { .. })),
            "{errs:?}"
        );
    }

    #[test]
    fn parent_cycle_is_rejected() {
        let mut sys = fixture();
        // Add a moon pair that orbit each other.
        let mut a = sys.bodies[1].clone();
        a.def.id = "a".into();
        a.def.parent = Some("b".into());
        let mut b = sys.bodies[1].clone();
        b.def.id = "b".into();
        b.def.parent = Some("a".into());
        sys.bodies.push(a);
        sys.bodies.push(b);
        sys.system.bodies.push("a".into());
        sys.system.bodies.push("b".into());
        let errs = validate_system(&sys);
        assert!(
            errs.iter()
                .filter(|e| matches!(e, ValidationError::ParentCycle { .. }))
                .count()
                >= 2,
            "both cycle members report: {errs:?}"
        );
    }

    #[test]
    fn period_inconsistent_with_parent_mu_is_rejected() {
        let mut sys = fixture();
        let o = planet(&mut sys).orbit.as_mut().unwrap();
        o.period_s *= 1.000_001;
        let errs = validate_system(&sys);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::PeriodInconsistent { .. })),
            "{errs:?}"
        );
    }

    #[test]
    fn atmosphere_top_below_scale_height_is_rejected() {
        let mut sys = fixture();
        planet(&mut sys).atmosphere = Some(AtmosphereDef {
            sea_level_pressure_pa: 101_325.0,
            scale_height_m: 5_600.0,
            top_m: 5_000.0, // below H — §3.0 violation
            molar_mass_kg_per_mol: 0.029,
            heat_capacity_ratio: 1.4,
            temperature_knots: vec![
                TemperatureKnot {
                    altitude_m: 0.0,
                    temperature_k: 288.0,
                },
                TemperatureKnot {
                    altitude_m: 5_000.0,
                    temperature_k: 200.0,
                },
            ],
        });
        let errs = validate_system(&sys);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::AtmosphereTopTooLow { .. })),
            "{errs:?}"
        );
    }

    #[test]
    fn dangling_parent_is_rejected() {
        let mut sys = fixture();
        planet(&mut sys).parent = Some("ghost".into());
        let errs = validate_system(&sys);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::ParentNotFound { .. })),
            "{errs:?}"
        );
    }

    #[test]
    fn second_root_is_rejected() {
        let mut sys = fixture();
        let mut stray = sys.bodies[1].clone();
        stray.def.id = "stray".into();
        stray.def.parent = None;
        stray.def.orbit = None;
        sys.bodies.push(stray);
        sys.system.bodies.push("stray".into());
        let errs = validate_system(&sys);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::RootNotStar { .. })),
            "{errs:?}"
        );
    }

    #[test]
    fn bad_schema_version_is_rejected() {
        let mut sys = fixture();
        planet(&mut sys).schema_version = 99;
        let errs = validate_system(&sys);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::SchemaVersion { found: 99, .. })),
            "{errs:?}"
        );
    }

    #[test]
    fn hyperbolic_and_negative_eccentricity_are_rejected() {
        for e in [1.0, 1.5, -0.1] {
            let mut sys = fixture();
            planet(&mut sys).orbit.as_mut().unwrap().eccentricity = e;
            let errs = validate_system(&sys);
            assert!(
                errs.iter()
                    .any(|x| matches!(x, ValidationError::EccentricityOutOfRange { .. })),
                "e={e}: {errs:?}"
            );
        }
    }

    #[test]
    fn non_finite_radius_is_rejected() {
        let mut sys = fixture();
        planet(&mut sys).radius_m = f64::NAN;
        let errs = validate_system(&sys);
        assert!(
            errs.iter().any(|e| matches!(
                e,
                ValidationError::NonFinite {
                    field: "radius_m",
                    ..
                }
            )),
            "{errs:?}"
        );
    }

    /// A minimal valid palette-PNG byte stream for validator tests — the
    /// inspector never checks CRCs, so chunks can be hand-assembled.
    fn tiny_png(w: u32, h: u32, palette_entries: usize) -> Vec<u8> {
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&w.to_be_bytes());
        ihdr.extend_from_slice(&h.to_be_bytes());
        ihdr.extend_from_slice(&[8, 3, 0, 0, 0]);
        for (kind, data) in [
            (*b"IHDR", ihdr),
            (*b"PLTE", vec![0u8; palette_entries * 3]),
            (*b"IEND", Vec::new()),
        ] {
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            out.extend_from_slice(&kind);
            out.extend_from_slice(&data);
            out.extend_from_slice(&[0, 0, 0, 0]); // unchecked CRC
        }
        out
    }

    fn with_biomes(sys: &mut LoadedSystem, legend_idx: u8, png: Vec<u8>) {
        let b = &mut sys.bodies[1];
        b.def.biome_map = Some(crate::system::BiomeMapRef {
            png: "biomes.png".into(),
            legend: "biomes.ron".into(),
        });
        b.biomes = Some(LoadedBiomeMap {
            legend: crate::biome::BiomeLegend {
                schema_version: SCHEMA_VERSION,
                entries: vec![
                    BiomeLegendEntry {
                        index: 0,
                        id: BiomeId("planet.one".into()),
                        name: "biome.planet.one".into(),
                    },
                    BiomeLegendEntry {
                        index: legend_idx,
                        id: BiomeId("planet.two".into()),
                        name: "biome.planet.two".into(),
                    },
                ],
            },
            png,
        });
    }

    #[test]
    fn biome_legend_index_beyond_palette_is_rejected() {
        let mut sys = fixture();
        with_biomes(&mut sys, 5, tiny_png(4, 4, 2)); // palette of 2, index 5
        let errs = validate_system(&sys);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::BiomeIndexExceedsPalette { .. })),
            "{errs:?}"
        );
    }

    #[test]
    fn non_png_biome_map_is_rejected() {
        let mut sys = fixture();
        with_biomes(&mut sys, 1, b"not a png".to_vec());
        let errs = validate_system(&sys);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::BiomeMapPngInvalid { .. })),
            "{errs:?}"
        );
    }

    #[test]
    fn oversized_biome_map_is_rejected() {
        let mut sys = fixture();
        with_biomes(&mut sys, 1, tiny_png(4096, 32, 2));
        let errs = validate_system(&sys);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::BiomeMapTooLarge { .. })),
            "{errs:?}"
        );
    }

    #[test]
    fn duplicate_legend_index_is_rejected() {
        let mut sys = fixture();
        with_biomes(&mut sys, 0, tiny_png(4, 4, 2)); // two entries at index 0
        let errs = validate_system(&sys);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::BiomeLegendDuplicate { .. })),
            "{errs:?}"
        );
    }

    #[test]
    fn unresolved_loc_key_is_rejected() {
        let mut loc = std::collections::BTreeMap::new();
        loc.insert("system.test.epoch_note".to_string(), "x".to_string());
        loc.insert("body.star.name".to_string(), "x".to_string());
        // Deliberately omit body.planet.name.
        let loc = LocFile {
            schema_version: SCHEMA_VERSION,
            strings: loc,
        };
        let errs = validate_loc_keys(&fixture(), &loc);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::UnresolvedLocKey { key, .. } if key == "body.planet.name")),
            "{errs:?}"
        );
    }

    // ---- part/resource validator tests (§3.1, §3.4) ----

    use std::path::PathBuf;

    use crate::part::{
        AeroDef, AeroKnot, AttachNode, ColliderDef, ModuleDef, PART_SCHEMA_VERSION, PartCategory,
        PartDef, PartVariant, QualCurve, QualKnot, ReliabilityDef, ScaleRule, StressKind,
        StressRule, StructuralDef, ThermalDef, ThrottleRange,
    };
    use crate::resource::{RESOURCE_SCHEMA_VERSION, ResourceDef};

    /// A minimal *valid* resource table — enough for the part fixture's
    /// references (propellants + build materials + a battery).
    fn res_fixture() -> ResourceFile {
        ResourceFile {
            schema_version: RESOURCE_SCHEMA_VERSION,
            resources: vec![
                ResourceDef {
                    id: "liquid_fuel".into(),
                    display_name: "resource.liquid_fuel".into(),
                    mass_per_unit_kg: 5.0,
                },
                ResourceDef {
                    id: "oxidizer".into(),
                    display_name: "resource.oxidizer".into(),
                    mass_per_unit_kg: 5.0,
                },
                ResourceDef {
                    id: "metal".into(),
                    display_name: "resource.metal".into(),
                    mass_per_unit_kg: 10.0,
                },
                ResourceDef {
                    id: "electric_charge".into(),
                    display_name: "resource.electric_charge".into(),
                    mass_per_unit_kg: 0.0,
                },
            ],
        }
    }

    /// A minimal *valid* engine def — the negative tests each mutate
    /// one rule so attribution is unambiguous.
    fn part_fixture() -> PartDef {
        PartDef {
            schema_version: PART_SCHEMA_VERSION,
            id: "test.engine".into(),
            family: "test.family".into(),
            display_name: "part.test.engine.name".into(),
            category: PartCategory::Engine,
            mass_dry_kg: 900.0,
            cost_funds: 1500.0,
            size_m: csp_core::DVec3::new(1.25, 1.25, 1.8),
            build_resources: vec![("metal".into(), 70.0)],
            attach_nodes: vec![AttachNode {
                id: "top".into(),
                position_m: csp_core::DVec3::new(0.0, 0.0, 0.9),
                direction: csp_core::DVec3::Z,
                diameter_m: 1.25,
            }],
            scale: ScaleRule::None,
            collider: ColliderDef::Cylinder {
                radius_m: 0.625,
                half_height_m: 0.9,
            },
            aero: AeroDef {
                cd_curve: vec![
                    AeroKnot {
                        mach: 0.0,
                        coeff: 0.5,
                    },
                    AeroKnot {
                        mach: 8.0,
                        coeff: 0.3,
                    },
                ],
                cl_curve: vec![AeroKnot {
                    mach: 0.0,
                    coeff: 0.0,
                }],
                ref_len_m: 1.8,
            },
            thermal: ThermalDef {
                heat_capacity_j_per_k: 450_000.0,
                max_temp_k: 1600.0,
                emissivity: 0.8,
                conduction_w_per_k: 60.0,
            },
            structural: StructuralDef {
                max_axial_n: 600_000.0,
                max_shear_n: 150_000.0,
                max_torque_nm: 25_000.0,
            },
            variants: vec![PartVariant {
                id: "stock".into(),
                display_name: "part.variant.stock".into(),
                cost_multiplier: 1.0,
                stat_multipliers: Vec::new(),
            }],
            modules: vec![ModuleDef::Engine {
                thrust_vac_n: 200_000.0,
                isp_vac_s: 320.0,
                isp_sl_s: 280.0,
                throttle_range: ThrottleRange { min: 0.3, max: 1.0 },
                gimbal_deg: 3.0,
                ignitions: Some(6),
                rated_burn_s: 240.0,
                propellants: vec![("liquid_fuel".into(), 0.45), ("oxidizer".into(), 0.55)],
                ullage_required: false,
            }],
            reliability: ReliabilityDef {
                base_mtbf_s: 400_000.0,
                infant_mortality: 0.2,
                qualification_curve: QualCurve {
                    knots: vec![
                        QualKnot {
                            qualification_units: 0.0,
                            hazard_multiplier: 1.0,
                        },
                        QualKnot {
                            qualification_units: 100.0,
                            hazard_multiplier: 0.5,
                        },
                    ],
                },
                regime_stress: vec![StressRule {
                    kind: StressKind::ExcessG,
                    threshold: 12.0,
                    multiplier: 2.0,
                }],
                dormant_stress: 0.02,
                failure_modes: vec![(crate::part::FailureMode::DegradedThrust, 1.0)],
            },
            tech_requirement: "tech.start".into(),
        }
    }

    fn loaded_part(def: PartDef) -> LoadedParts {
        LoadedParts {
            parts: vec![LoadedPart {
                path: PathBuf::from(format!("{}.ron", def.id)),
                def,
            }],
        }
    }

    fn part_errors(def: PartDef) -> Vec<ValidationError> {
        validate_parts(&loaded_part(def), &res_fixture())
    }

    /// The fixture itself must be clean — otherwise the negative tests
    /// below attribute nothing.
    #[test]
    fn part_fixture_is_valid() {
        assert!(validate_resources(&res_fixture()).is_empty());
        assert!(part_errors(part_fixture()).is_empty());
    }

    #[test]
    fn part_bad_schema_version_is_rejected() {
        let mut d = part_fixture();
        d.schema_version = 42;
        let errs = part_errors(d);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::SchemaVersion { found: 42, .. })),
            "{errs:?}"
        );
    }

    #[test]
    fn duplicate_part_id_is_rejected() {
        let d = part_fixture();
        let loaded = LoadedParts {
            parts: vec![
                LoadedPart {
                    path: PathBuf::from("test.engine.ron"),
                    def: d.clone(),
                },
                LoadedPart {
                    path: PathBuf::from("copy/test.engine.ron"),
                    def: d,
                },
            ],
        };
        let errs = validate_parts(&loaded, &res_fixture());
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::DuplicatePartId { .. })),
            "{errs:?}"
        );
    }

    #[test]
    fn part_id_file_mismatch_is_rejected() {
        let loaded = LoadedParts {
            parts: vec![LoadedPart {
                path: PathBuf::from("wrong.name.ron"),
                def: part_fixture(),
            }],
        };
        let errs = validate_parts(&loaded, &res_fixture());
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::PartIdFileMismatch { .. })),
            "{errs:?}"
        );
    }

    #[test]
    fn non_positive_part_mass_is_rejected() {
        let mut d = part_fixture();
        d.mass_dry_kg = -1.0;
        let errs = part_errors(d);
        assert!(
            errs.iter().any(|e| matches!(
                e,
                ValidationError::PartNonPositive {
                    field: "mass_dry_kg",
                    ..
                }
            )),
            "{errs:?}"
        );
    }

    #[test]
    fn non_finite_part_field_is_rejected() {
        let mut d = part_fixture();
        d.size_m.x = f64::NAN;
        let errs = part_errors(d);
        assert!(
            errs.iter().any(|e| matches!(
                e,
                ValidationError::PartNonFinite {
                    field: "size_m",
                    ..
                }
            )),
            "{errs:?}"
        );
    }

    #[test]
    fn unknown_and_duplicate_resources_are_rejected() {
        let mut d = part_fixture();
        d.build_resources = vec![("metal".into(), 1.0), ("metal".into(), 2.0)];
        let errs = part_errors(d);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::DuplicateResource { .. })),
            "{errs:?}"
        );

        let mut d = part_fixture();
        d.build_resources = vec![("unobtainium".into(), 1.0)];
        let errs = part_errors(d);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::UnknownResource { resource, .. } if resource == "unobtainium")),
            "{errs:?}"
        );
    }

    #[test]
    fn empty_and_duplicate_variants_are_rejected() {
        let mut d = part_fixture();
        d.variants.clear();
        let errs = part_errors(d);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::EmptyVariants { .. })),
            "{errs:?}"
        );

        let mut d = part_fixture();
        d.variants.push(d.variants[0].clone());
        let errs = part_errors(d);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::DuplicateVariantId { .. })),
            "{errs:?}"
        );
    }

    #[test]
    fn variant_stat_errors_are_rejected() {
        use crate::part::StatPath;
        // Same stat twice.
        let mut d = part_fixture();
        d.variants[0].stat_multipliers = vec![(StatPath::IspVacS, 1.1), (StatPath::IspVacS, 1.2)];
        let errs = part_errors(d);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::DuplicateVariantStat { .. })),
            "{errs:?}"
        );
        // Non-positive multiplier.
        let mut d = part_fixture();
        d.variants[0].stat_multipliers = vec![(StatPath::ThrustVacN, 0.0)];
        let errs = part_errors(d);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::BadVariantMultiplier { .. })),
            "{errs:?}"
        );
    }

    #[test]
    fn bad_scale_rules_are_rejected() {
        // Discrete without the 1.0 baseline.
        let mut d = part_fixture();
        d.scale = ScaleRule::Discrete(vec![0.5, 2.0]);
        let errs = part_errors(d);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::BadScaleRule { .. })),
            "{errs:?}"
        );
        // Discrete unsorted.
        let mut d = part_fixture();
        d.scale = ScaleRule::Discrete(vec![2.0, 1.0]);
        let errs = part_errors(d);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::BadScaleRule { .. })),
            "{errs:?}"
        );
        // Continuous range that does not contain 1.0.
        let mut d = part_fixture();
        d.scale = ScaleRule::Continuous {
            min: 1.5,
            max: 3.0,
            exponents: crate::part::ScaleExponents {
                mass: 3.0,
                cost: 3.0,
                thrust: 2.0,
                capacity: 3.0,
                structural: 2.0,
            },
        };
        let errs = part_errors(d);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::BadScaleRule { .. })),
            "{errs:?}"
        );
        // Continuous exponent out of the sane bound.
        let mut d = part_fixture();
        d.scale = ScaleRule::Continuous {
            min: 0.5,
            max: 2.0,
            exponents: crate::part::ScaleExponents {
                mass: 9.0,
                cost: 3.0,
                thrust: 2.0,
                capacity: 3.0,
                structural: 2.0,
            },
        };
        let errs = part_errors(d);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::BadScaleRule { .. })),
            "{errs:?}"
        );
    }

    #[test]
    fn bad_attach_nodes_are_rejected() {
        // No nodes at all.
        let mut d = part_fixture();
        d.attach_nodes.clear();
        let errs = part_errors(d);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::BadAttachNode { .. })),
            "{errs:?}"
        );
        // Non-unit direction.
        let mut d = part_fixture();
        d.attach_nodes[0].direction = csp_core::DVec3::new(0.0, 0.0, 2.0);
        let errs = part_errors(d);
        assert!(
            errs.iter().any(|e| matches!(
                e,
                ValidationError::BadAttachNode {
                    reason: "direction is not unit length",
                    ..
                }
            )),
            "{errs:?}"
        );
        // Duplicate node id.
        let mut d = part_fixture();
        d.attach_nodes.push(d.attach_nodes[0].clone());
        let errs = part_errors(d);
        assert!(
            errs.iter().any(|e| matches!(
                e,
                ValidationError::BadAttachNode {
                    reason: "duplicate node id",
                    ..
                }
            )),
            "{errs:?}"
        );
    }

    #[test]
    fn category_module_mismatch_is_rejected() {
        let mut d = part_fixture();
        d.modules = Vec::new(); // category Engine without an Engine module
        let errs = part_errors(d);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::CategoryModuleMismatch { .. })),
            "{errs:?}"
        );
    }

    #[test]
    fn discrete_scaled_engine_or_tank_is_rejected() {
        // Engine module on a Discrete rule: the size menu cannot scale
        // its stats — forbidden outright.
        let mut d = part_fixture();
        d.scale = ScaleRule::Discrete(vec![0.5, 1.0, 2.0]);
        let errs = part_errors(d);
        assert!(
            errs.iter().any(|e| matches!(
                e,
                ValidationError::BadModule {
                    reason: "discrete scaling carries no exponents — tank/engine stats would not scale",
                    ..
                }
            )),
            "{errs:?}"
        );
        // A tank-only part fares no better.
        let mut d = part_fixture();
        d.scale = ScaleRule::Discrete(vec![0.5, 1.0, 2.0]);
        d.modules = vec![ModuleDef::Tank {
            capacity: vec![("liquid_fuel".into(), 100.0)],
        }];
        d.category = PartCategory::FuelTank;
        let errs = part_errors(d);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::BadModule { .. })),
            "{errs:?}"
        );
        // Discrete on a passive part is fine.
        let mut d = part_fixture();
        d.scale = ScaleRule::Discrete(vec![0.5, 1.0, 2.0]);
        d.modules = vec![ModuleDef::Decoupler];
        d.category = PartCategory::Utility;
        assert!(part_errors(d).is_empty());
        // And a Continuous rule keeps its engine legal.
        let mut d = part_fixture();
        d.scale = ScaleRule::Continuous {
            min: 0.5,
            max: 2.0,
            exponents: crate::part::ScaleExponents {
                mass: 3.0,
                cost: 3.0,
                thrust: 2.0,
                capacity: 3.0,
                structural: 2.0,
            },
        };
        assert!(part_errors(d).is_empty());
    }

    #[test]
    fn bad_engine_fields_are_rejected() {
        // isp_sl above isp_vac.
        let mut d = part_fixture();
        if let ModuleDef::Engine { isp_sl_s, .. } = &mut d.modules[0] {
            *isp_sl_s = 400.0;
        }
        let errs = part_errors(d);
        assert!(
            errs.iter().any(|e| matches!(
                e,
                ValidationError::BadModule {
                    reason: "isp_sl_s exceeds isp_vac_s",
                    ..
                }
            )),
            "{errs:?}"
        );
        // Propellant fractions not summing to 1.
        let mut d = part_fixture();
        if let ModuleDef::Engine { propellants, .. } = &mut d.modules[0] {
            propellants[1].1 = 0.9;
        }
        let errs = part_errors(d);
        assert!(
            errs.iter().any(|e| matches!(
                e,
                ValidationError::BadModule {
                    reason: "propellant mass fractions do not sum to 1",
                    ..
                }
            )),
            "{errs:?}"
        );
        // Broken throttle band.
        let mut d = part_fixture();
        if let ModuleDef::Engine { throttle_range, .. } = &mut d.modules[0] {
            *throttle_range = ThrottleRange { min: 1.2, max: 1.4 };
        }
        let errs = part_errors(d);
        assert!(
            errs.iter().any(|e| matches!(
                e,
                ValidationError::BadModule {
                    reason: "throttle_range outside 0 <= min < max <= 1",
                    ..
                }
            )),
            "{errs:?}"
        );
    }

    #[test]
    fn bad_reliability_fields_are_rejected() {
        // dormant_stress above 1.
        let mut d = part_fixture();
        d.reliability.dormant_stress = 1.5;
        let errs = part_errors(d);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::BadReliability { .. })),
            "{errs:?}"
        );
        // Empty qualification curve.
        let mut d = part_fixture();
        d.reliability.qualification_curve.knots.clear();
        let errs = part_errors(d);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::BadReliability { .. })),
            "{errs:?}"
        );
        // Stress multiplier below 1 (stress raises hazard, §3.4).
        let mut d = part_fixture();
        d.reliability.regime_stress[0].multiplier = 0.5;
        let errs = part_errors(d);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::BadReliability { .. })),
            "{errs:?}"
        );
        // Empty failure modes.
        let mut d = part_fixture();
        d.reliability.failure_modes.clear();
        let errs = part_errors(d);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::BadReliability { .. })),
            "{errs:?}"
        );
    }

    #[test]
    fn bad_collider_is_rejected() {
        let mut d = part_fixture();
        d.collider = ColliderDef::Convex {
            points: vec![csp_core::DVec3::ZERO; 3],
        };
        let errs = part_errors(d);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::BadCollider { .. })),
            "{errs:?}"
        );
    }

    #[test]
    fn bad_aero_curve_is_rejected() {
        let mut d = part_fixture();
        d.aero.cd_curve = vec![
            AeroKnot {
                mach: 8.0,
                coeff: 0.3,
            },
            AeroKnot {
                mach: 0.0,
                coeff: 0.5,
            },
        ];
        let errs = part_errors(d);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::BadAeroCurve { .. })),
            "{errs:?}"
        );
    }

    /// §3.4's Euler-stability bound: `conduction_w_per_k` above
    /// `heat_capacity_j_per_k` is rejected; equality is the accepted
    /// edge.
    #[test]
    fn conduction_exceeding_capacity_is_rejected() {
        let mut d = part_fixture();
        d.thermal.conduction_w_per_k = d.thermal.heat_capacity_j_per_k + 1.0;
        let errs = part_errors(d);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::ConductionExceedsCapacity { .. })),
            "{errs:?}"
        );
        // The boundary itself is valid.
        let mut d = part_fixture();
        d.thermal.conduction_w_per_k = d.thermal.heat_capacity_j_per_k;
        assert!(part_errors(d).is_empty());
    }

    #[test]
    fn bad_resource_table_is_rejected() {
        // Duplicate resource id.
        let mut r = res_fixture();
        r.resources.push(r.resources[0].clone());
        let errs = validate_resources(&r);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::DuplicateResource { .. })),
            "{errs:?}"
        );
        // Negative mass per unit.
        let mut r = res_fixture();
        r.resources[0].mass_per_unit_kg = -1.0;
        let errs = validate_resources(&r);
        assert!(
            errs.iter().any(|e| matches!(
                e,
                ValidationError::PartNonPositive {
                    field: "mass_per_unit_kg",
                    ..
                }
            )),
            "{errs:?}"
        );
    }

    #[test]
    fn part_loc_key_resolution_is_rejected_when_missing() {
        let loaded = loaded_part(part_fixture());
        let loc = LocFile {
            schema_version: SCHEMA_VERSION,
            strings: std::collections::BTreeMap::new(),
        };
        let errs = validate_part_loc_keys(&loaded, &res_fixture(), &loc);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::UnresolvedLocKey { key, .. } if key == "part.test.engine.name")),
            "{errs:?}"
        );
    }
}
