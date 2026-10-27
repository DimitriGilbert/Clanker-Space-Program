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
//! - biome legends have unique well-formed indices/IDs that fit inside
//!   the PNG's palette, the map decodes as a real 8-bit palette PNG
//!   within §3.0.1's ≤2048×1024 bound, and every pixel index the map
//!   paints is legended;
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

use crate::biome::{BIOME_MAP_MAX_H, BIOME_MAP_MAX_W, decode_indexed_png, inspect_indexed_png};
use crate::ids::LocKey;
use crate::load::{LoadedPart, LoadedParts, LoadedSystem};
use crate::loc::LocFile;
use crate::part::{ColliderDef, ModuleDef, PART_SCHEMA_VERSION, PartCategory, PartDef, ScaleRule};
use crate::resource::{RESOURCE_SCHEMA_VERSION, ResourceFile};
use crate::site::{SiteDef, SiteFile};
use crate::system::{KeplerElements, SCHEMA_VERSION};
use crate::techtree::{TechNode, UpgradeTrack};

/// Relative tolerance for the §3.0 period-consistency rule:
/// `|T_authored − 2π√(a³/μ_parent)| / T_computed ≤ PERIOD_REL_TOL`.
pub const PERIOD_REL_TOL: f64 = 1e-9;

/// Minimum `|cos H · cos φ|` a site on a fixed-sun (zero-synodic)
/// body must keep from the permanent terminator — ~2.9° of real
/// angle at the equator. On such a body the Day/Night verdict is
/// decided ONCE by that product; a site within the floor lets a
/// floating-point residue flip its entire power budget (rask-mines
/// shipped at π/2 on a +6.1e-17 cosine — C09-F2). Non-synodic-zero
/// bodies cycle day/night anyway, so the rule only applies where the
/// verdict is permanent.
pub const FIXED_SUN_TERMINATOR_MIN_COS: f64 = 0.05;

/// The fixed-sun floor under `|ω − n_sun|` — mirrors `colonies::EPS`,
/// the `next_terminator_tick` "no crossings" bound (kept in step
/// manually: the sim depends on this crate, never the reverse).
const FIXED_SUN_SYNODIC_EPS: f64 = 1e-9;

/// One validation failure. `body`/`parent` fields carry the string form
/// of IDs — diagnostics are never hashed, so plain `String`s are fine
/// (same convention as `csp_bench::ReplayError`).
#[derive(Clone, Debug, PartialEq)]
pub enum ValidationError {
    /// A file's `schema_version` is outside the band its loader/
    /// validator pair supports (the exact-match namespaces pin
    /// `expected`; `sites.ron`'s current-or-previous band reports the
    /// current version).
    SchemaVersion {
        /// Which file kind ("system.ron", `body.ron`/`biomes.ron` under a
        /// body dir — the message carries the context).
        file: String,
        /// What the file declared.
        found: u32,
        /// The schema version this validator expects.
        expected: u32,
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
    /// A parented body declares no `soi_m` — `None` reads as infinite
    /// (the star's case), which on a satellite swallows its own parent.
    SoiMissing {
        /// The body.
        body: String,
    },
    /// §3.0 rule: a child's periapsis must clear the parent's surface.
    PeriapsisInsideParent {
        /// The parent whose surface the orbit dips under.
        parent: String,
        /// The child orbiting through it.
        child: String,
        /// The child's periapsis `a·(1−e)` (m, center-to-center).
        periapsis_m: f64,
        /// `parent.radius_m + child.radius_m` — the clearance the
        /// periapsis must exceed.
        clearance_m: f64,
    },
    /// §3.0 rule: a body's own SOI must nest inside its own orbit —
    /// `soi ≥ periapsis` puts the parent inside the child's SOI at
    /// closest approach.
    SoiExceedsOrbit {
        /// The body.
        body: String,
        /// Its authored SOI (m).
        soi_m: f64,
        /// Its own periapsis `a·(1−e)` (m).
        periapsis_m: f64,
    },
    /// §3.0 rule: two siblings' SOI spheres must not overlap. Flagged
    /// only when their orbital radial shells are disjoint (the provable
    /// static bound): `soi_a + soi_b` exceeds the shell gap.
    SiblingSoiOverlap {
        /// The shared parent.
        parent: String,
        /// The lower-id sibling.
        a: String,
        /// The other sibling.
        b: String,
        /// The gap between the two radial shells (m).
        gap_m: f64,
        /// `soi_a + soi_b` (m).
        soi_sum_m: f64,
    },
    /// A `surface_resources` key is not a `resources.ron` id — the one
    /// `ResourceId` cross-ref `validate_system` cannot check (the
    /// resource file lives outside the system directory); gated in
    /// [`validate_dataset`] instead.
    UnknownSurfaceResource {
        /// The body authoring the unknown key.
        body: String,
        /// The unknown resource ID.
        resource: String,
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
    /// A pixel in the decoded map carries a palette index no legend
    /// entry names — an unnamed biome region (the science key would
    /// resolve a biome id that was never authored). The header/pixel
    /// bound is the PNG's palette; this is the legend side.
    BiomeIndexNotInLegend {
        /// The body.
        body: String,
        /// The palette index the legend does not cover.
        index: u8,
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
    /// A variant multiplies a [`crate::part::StatPath`] the part def
    /// does not carry — `resolve_built_part` fails this with
    /// `UnresolvableStatPath` at manufacture, so it must not validate
    /// green (the same `base_value(def)` predicate the tech-tree
    /// upgrade-track check runs).
    UnresolvableVariantStat {
        /// The part.
        part: String,
        /// The variant.
        variant: String,
        /// `StatPath::name` of the path that does not resolve.
        stat: &'static str,
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
    /// A `data/recipes/*.ron` def failed a §3.5.1 rule — the
    /// [`ValidationError::InvalidSite`]/[`ValidationError::InvalidColony`]
    /// shape applied to the recipe registry (previously recipe errors
    /// were stamped `InvalidColony`, which attributed recipe-registry
    /// failures to a colony file that never carried them).
    InvalidRecipe {
        /// The recipe def's id.
        id: String,
        /// What is wrong with it.
        reason: String,
    },
    /// An `data/experiments/*.ron` def failed a §3.2 rule.
    BadExperiment {
        /// The experiment def at fault.
        experiment: String,
        /// Why it failed (stable reason token).
        reason: &'static str,
    },
    // ---- contracts (§3.6) ----
    /// A `data/contracts/*.ron` template failed a §3.6 rule.
    BadContract {
        /// The template at fault.
        contract: String,
        /// Why it failed (stable reason token).
        reason: &'static str,
    },
    /// A tech node's `unlocks.contract_types` names a template that
    /// does not exist among the authored contracts.
    UnknownContractType {
        /// The tech node carrying the dangling unlock.
        node: String,
        /// The missing contract-type ID.
        contract_type: String,
    },
    // ---- tech tree (§3.3) ----
    /// Two `data/techtree/**` files declare the same node `id`.
    DuplicateTechNodeId {
        /// The duplicated ID.
        id: String,
    },
    /// Two `data/techtree/**` files declare the same track `id`.
    DuplicateUpgradeTrackId {
        /// The duplicated ID.
        id: String,
    },
    /// A node's `requires` names a node absent from the merged tree.
    UnknownTechPrerequisite {
        /// The node with the dangling prerequisite.
        node: String,
        /// The missing prerequisite ID.
        prerequisite: String,
    },
    /// Following `requires` edges returns to a visited node — §3.3
    /// "no cycles".
    TechCycle {
        /// A node on the cycle.
        node: String,
    },
    /// A node's `science_cost` is lower than one of its
    /// prerequisites' — §3.3 "every node's cost monotonic in depth"
    /// (non-decreasing along every `requires` edge).
    TechCostNotMonotonic {
        /// The cheaper node.
        node: String,
        /// The prerequisite it undercuts.
        prerequisite: String,
    },
    /// A node is malformed beyond the dedicated variants: non-finite
    /// or negative `science_cost`, a duplicated prerequisite, a
    /// duplicated grant, a grant level of 0 or above the track's
    /// authored length, an empty/unresolvable id field.
    BadTechNode {
        /// The node at fault.
        node: String,
        /// Why it failed (stable reason token).
        reason: &'static str,
    },
    /// A node's `track_levels` grant names a track absent from the
    /// merged tree.
    UnknownUpgradeTrack {
        /// The node carrying the bad grant.
        node: String,
        /// The missing track ID.
        track: String,
    },
    /// An `UpgradeTrack` is malformed: empty `levels`, a non-finite or
    /// negative `science_cost`, an out-of-band effect, a duplicated
    /// `(family, stat_path)`, a family no part belongs to, a
    /// `stat_path` some family part cannot resolve, or more than
    /// `u8::MAX` levels.
    BadUpgradeTrack {
        /// The track at fault.
        track: String,
        /// Why it failed (stable reason token).
        reason: &'static str,
    },
    /// A part's `tech_requirement` names a node absent from the merged
    /// tree — §3.3 "every part reachable": a def gated on a
    /// nonexistent node can never be manufactured.
    UnknownTechRequirement {
        /// The unreachable part.
        part: String,
        /// The node it names.
        tech: String,
    },
    /// A `data/balance.ron` tunable violates its authored domain — a
    /// bounded value outside its range (the capture-cone angle, the
    /// recovery shares, the soft-capture Euler-stability bound, a zero
    /// cadence) beyond what the plain sign checks express. Finite/sign
    /// failures report as [`ValidationError::PartNonFinite`]/
    /// [`ValidationError::PartNonPositive`] under `"balance.ron"`; the
    /// resource ref reports as [`ValidationError::UnknownResource`].
    BadBalance {
        /// Field name.
        field: &'static str,
        /// Why it failed (stable reason token).
        reason: &'static str,
    },
    /// A `data/parts/` `Converter` module names a `data/recipes/` id
    /// that does not exist — the colony flow network would register no
    /// recipe rows for it (a dead converter).
    UnknownRecipe {
        /// The referencing part.
        part: String,
        /// The unknown recipe ID.
        recipe: String,
    },
    /// A `data/parts/` `Experiment` module names a
    /// `data/experiments/` kind that does not exist — `RunExperiment`
    /// has no def to resolve and `ModuleReq::Experiment` prereqs can
    /// never be met.
    UnknownExperimentKind {
        /// The referencing part.
        part: String,
        /// The unknown experiment kind ID.
        experiment: String,
    },
    /// An authored file reference (`terrain.heightmap`, a
    /// `surface_resources` map, a `BiomeMapRef` legend/png) is not a
    /// tame single-segment relative name — the loader rejects these
    /// before the `join`; the validator repeats the check so a
    /// hand-rolled `LoadedSystem` cannot smuggle an unsafe ref to
    /// later consumers.
    BadFileRef {
        /// Owning body.
        body: String,
        /// Field name.
        field: &'static str,
    },
    /// A `sites.ron` site entry is malformed: duplicate id, a body
    /// the system does not declare, a non-finite or out-of-range
    /// geocentric pose, or a second launch site on one body.
    InvalidSite {
        /// The site id the rule fired on (or the empty string for
        /// file-level rules).
        id: String,
        /// What is wrong with it.
        reason: String,
    },
    /// A `colonies.ron` colony entry is malformed (§3.5.1): the site
    /// is not in `sites.ron`, a rate/capacity/level field is
    /// non-finite or negative, or a resource ref is unknown or
    /// duplicated inside one list.
    InvalidColony {
        /// The colony's site id (its name in the file).
        site: String,
        /// What is wrong with it.
        reason: String,
    },
}

impl fmt::Display for ValidationError {
    #[allow(clippy::too_many_lines)] // one arm per variant + the explicit delegation alternation
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SchemaVersion {
                file,
                found,
                expected,
            } => {
                write!(f, "{file}: schema_version {found}, expected {expected}")
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
            Self::SoiMissing { body } => write!(
                f,
                "{body:?}: parented body declares no soi_m — a non-star SOI is never unbounded"
            ),
            Self::PeriapsisInsideParent {
                parent,
                child,
                periapsis_m,
                clearance_m,
            } => write!(
                f,
                "{child:?} periapsis {periapsis_m} m does not clear parent {parent:?} + own radius {clearance_m} m"
            ),
            Self::SoiExceedsOrbit {
                body,
                soi_m,
                periapsis_m,
            } => write!(
                f,
                "{body:?}: soi_m {soi_m} m reaches its own periapsis {periapsis_m} m — SOI must nest inside the orbit"
            ),
            Self::SiblingSoiOverlap {
                parent,
                a,
                b,
                gap_m,
                soi_sum_m,
            } => write!(
                f,
                "siblings {a:?} and {b:?} of {parent:?}: SOI sum {soi_sum_m} m exceeds the {gap_m} m shell gap — spheres can overlap"
            ),
            Self::UnknownSurfaceResource { body, resource } => {
                write!(
                    f,
                    "{body:?}.surface_resources: resource {resource:?} is not in resources.ron"
                )
            }
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
            Self::BiomeIndexNotInLegend { body, index } => write!(
                f,
                "{body:?}: biome map paints palette index {index} the legend does not name"
            ),
            Self::UnresolvedLocKey { context, key } => {
                write!(f, "{context}: loc key {key:?} has no entry in en.ron")
            }
            // Part/resource/recipe/contract/tech/site/colony
            // diagnostics are grouped into `fmt_part`/`fmt_tech` — one
            // arm per variant would triple this function's length. The
            // delegation is an explicit alternation, NOT `_`: a new
            // variant makes this match non-exhaustive, so it can never
            // reach the bottom of the chain and format empty.
            Self::DuplicatePartId { .. }
            | Self::PartIdFileMismatch { .. }
            | Self::PartNonFinite { .. }
            | Self::PartNonPositive { .. }
            | Self::UnknownResource { .. }
            | Self::DuplicateResource { .. }
            | Self::EmptyVariants { .. }
            | Self::DuplicateVariantId { .. }
            | Self::DuplicateVariantStat { .. }
            | Self::BadVariantMultiplier { .. }
            | Self::UnresolvableVariantStat { .. }
            | Self::BadScaleRule { .. }
            | Self::BadAttachNode { .. }
            | Self::CategoryModuleMismatch { .. }
            | Self::BadModule { .. }
            | Self::BadReliability { .. }
            | Self::BadCollider { .. }
            | Self::BadAeroCurve { .. }
            | Self::ConductionExceedsCapacity { .. }
            | Self::InvalidRecipe { .. }
            | Self::BadExperiment { .. }
            | Self::BadContract { .. }
            | Self::UnknownContractType { .. }
            | Self::DuplicateTechNodeId { .. }
            | Self::DuplicateUpgradeTrackId { .. }
            | Self::UnknownTechPrerequisite { .. }
            | Self::TechCycle { .. }
            | Self::TechCostNotMonotonic { .. }
            | Self::BadTechNode { .. }
            | Self::UnknownUpgradeTrack { .. }
            | Self::BadUpgradeTrack { .. }
            | Self::UnknownTechRequirement { .. }
            | Self::BadBalance { .. }
            | Self::UnknownRecipe { .. }
            | Self::UnknownExperimentKind { .. }
            | Self::BadFileRef { .. }
            | Self::InvalidSite { .. }
            | Self::InvalidColony { .. } => self.fmt_part(f),
        }
    }
}

impl ValidationError {
    /// Display for the part/resource rule variants — split out of
    /// [`fmt::Display`] to keep both functions readable (one arm per
    /// variant is the honest shape here). The `_` arm is unreachable:
    /// `fmt`'s catch-all routes only part/resource variants here.
    #[allow(clippy::too_many_lines)] // one arm per variant is the table
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
            Self::UnresolvableVariantStat {
                part,
                variant,
                stat,
            } => {
                write!(
                    f,
                    "{part:?}: variant {variant:?} multiplies {stat}, which this part def does not carry"
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
            Self::InvalidRecipe { id, reason } => {
                write!(f, "recipe {id:?}: {reason}")
            }
            Self::BadExperiment { experiment, reason } => {
                write!(f, "{experiment:?}: experiment: {reason}")
            }
            Self::BadContract { contract, reason } => {
                write!(f, "{contract:?}: contract: {reason}")
            }
            Self::UnknownContractType {
                node,
                contract_type,
            } => write!(
                f,
                "{node:?}: unlocks contract type {contract_type:?} but no template declares it"
            ),
            Self::BadBalance { field, reason } => {
                write!(f, "balance.ron: {field}: {reason}")
            }
            Self::UnknownRecipe { part, recipe } => {
                write!(
                    f,
                    "{part:?}: converter recipe {recipe:?} is not in data/recipes"
                )
            }
            Self::UnknownExperimentKind { part, experiment } => {
                write!(
                    f,
                    "{part:?}: experiment kind {experiment:?} is not in data/experiments"
                )
            }
            Self::BadFileRef { body, field } => {
                write!(
                    f,
                    "{body:?}: {field} is not a tame single-segment file name"
                )
            }
            // The tech/site/colony tail delegates to `fmt_tech` —
            // explicit alternation like `fmt`'s, so the routing is
            // compile-checked.
            Self::DuplicateTechNodeId { .. }
            | Self::DuplicateUpgradeTrackId { .. }
            | Self::UnknownTechPrerequisite { .. }
            | Self::TechCycle { .. }
            | Self::TechCostNotMonotonic { .. }
            | Self::BadTechNode { .. }
            | Self::UnknownUpgradeTrack { .. }
            | Self::BadUpgradeTrack { .. }
            | Self::UnknownTechRequirement { .. }
            | Self::InvalidSite { .. }
            | Self::InvalidColony { .. } => self.fmt_tech(f),
            // The system-side variants `fmt` handles locally never
            // reach here — same never-empty rule as `fmt_tech`'s tail.
            other => {
                debug_assert!(false, "ValidationError has no Display arm: {other:?}");
                write!(f, "{other:?}")
            }
        }
    }

    /// Display for the §3.3 tech-tree variants plus the site/colony
    /// file variants — split from [`fmt_part`] for the same readability
    /// reason. The final arm is unreachable by construction (the
    /// alternations above route only this set here) but still never
    /// formats empty: debug builds assert, release writes the Debug
    /// form.
    fn fmt_tech(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateTechNodeId { id } => {
                write!(f, "duplicate tech node id {id:?}")
            }
            Self::DuplicateUpgradeTrackId { id } => {
                write!(f, "duplicate upgrade track id {id:?}")
            }
            Self::UnknownTechPrerequisite { node, prerequisite } => {
                write!(
                    f,
                    "{node:?}: prerequisite {prerequisite:?} is not in the tree"
                )
            }
            Self::TechCycle { node } => {
                write!(f, "{node:?}: requires chain cycles back")
            }
            Self::TechCostNotMonotonic { node, prerequisite } => {
                write!(
                    f,
                    "{node:?}: science_cost below prerequisite {prerequisite:?}'s (cost must be monotonic in depth)"
                )
            }
            Self::BadTechNode { node, reason } => {
                write!(f, "{node:?}: tech node: {reason}")
            }
            Self::UnknownUpgradeTrack { node, track } => {
                write!(
                    f,
                    "{node:?}: track_levels grant {track:?} is not in the tree"
                )
            }
            Self::BadUpgradeTrack { track, reason } => {
                write!(f, "{track:?}: upgrade track: {reason}")
            }
            Self::InvalidSite { id, reason } => {
                write!(f, "site {id:?}: {reason}")
            }
            Self::InvalidColony { site, reason } => {
                write!(f, "colony at site {site:?}: {reason}")
            }
            Self::UnknownTechRequirement { part, tech } => {
                write!(
                    f,
                    "{part:?}: tech_requirement {tech:?} is not in the tree — part unreachable"
                )
            }
            // Unreachable by construction — but never an empty render:
            // a variant routed here without an arm is a bug the debug
            // assert names loudly, and release still prints something.
            other => {
                debug_assert!(false, "ValidationError has no Display arm: {other:?}");
                write!(f, "{other:?}")
            }
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

/// §3.2 situation-mask hygiene shared by `ModuleDef::Experiment` and
/// `ExperimentCap`: the mask must be non-empty (`empty_reason` keeps
/// each site's stable message) AND carry no bits outside
/// [`crate::part::SituationMask::ALL`] — a spare bit is dead authored
/// data (`situations()` silently drops it; `Situation` is append-only,
/// so no shipping bit pattern may set them).
fn check_situation_mask(
    mask: crate::part::SituationMask,
    empty_reason: &'static str,
    mut bad: impl FnMut(&'static str),
) {
    if mask.is_empty() {
        bad(empty_reason);
    }
    if mask.0 & !crate::part::SituationMask::ALL.0 != 0 {
        bad("situation mask sets spare bits outside the six situations");
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
            expected: SCHEMA_VERSION,
        });
    }
    for b in &sys.bodies {
        if b.def.schema_version != SCHEMA_VERSION {
            out.push(ValidationError::SchemaVersion {
                file: format!("{}/body.ron", b.def.id),
                found: b.def.schema_version,
                expected: SCHEMA_VERSION,
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
                expected: SCHEMA_VERSION,
            });
        }
    }
}

/// Index loaded defs by ID, reporting duplicates. First def wins —
/// the convention the loaders' `by_id` views document (`or_insert`,
/// not `insert`: `insert` would make the LAST duplicate the resolved
/// def while the sim sees the first).
fn build_def_map<'a>(sys: &'a LoadedSystem, out: &mut Vec<ValidationError>) -> DefMap<'a> {
    let mut defs = DefMap::new();
    for b in &sys.bodies {
        let id = b.def.id.as_str();
        if let std::collections::btree_map::Entry::Vacant(v) = defs.entry(id) {
            v.insert(b);
        } else {
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
    match b.def.soi_m {
        Some(soi) => check_positive(body, "soi_m", soi, out),
        // `None` reads as infinite — the star's case (`BodyDef::soi_m`'s
        // doc). On a parented body an unbounded SOI swallows the whole
        // system: nothing inside can ever transition out, and the
        // child-side geometry rules in [`check_orbit_rules`] would have
        // no finite boundary to check against (silently skipped there
        // before this gate existed).
        None if b.def.parent.is_some() => out.push(ValidationError::SoiMissing {
            body: body.to_string(),
        }),
        None => {}
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
    check_file_refs(b, out);
}

/// Authored file references must be tame single-segment names — the
/// same gate [`crate::load`] runs before `join`ing them under the body
/// dir, repeated here so a hand-rolled `LoadedSystem` cannot smuggle a
/// `..`/absolute/separator-bearing ref to the renderer or the
/// concentration-map consumer.
fn check_file_refs(b: &crate::load::LoadedBody, out: &mut Vec<ValidationError>) {
    let body = b.def.id.as_str();
    let mut check = |field: &'static str, name: &str| {
        if !crate::load::authored_name_ok(name) {
            out.push(ValidationError::BadFileRef {
                body: body.to_string(),
                field,
            });
        }
    };
    if let Some(h) = &b.def.terrain.heightmap {
        check("terrain.heightmap", h);
    }
    if let Some(r) = &b.def.biome_map {
        check("biome_map.png", &r.png);
        check("biome_map.legend", &r.legend);
    }
    for (_, map) in &b.def.surface_resources {
        check("surface_resources.png", &map.png);
    }
}

/// Orbit-element sanity plus the §3.0 orbital rules: child apoapsis
/// inside parent SOI, `period_s` consistent with `2π√(a³/μ_parent)`
/// to [`PERIOD_REL_TOL`] relative, and the patched-conics geometry the
/// apoapsis rule alone does not cover — periapsis above the parent's
/// surface, the child's own SOI nested inside its own orbit, and
/// sibling SOIs that cannot overlap while their radial shells stay
/// disjoint.
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
    let periapsis = o.semi_major_axis_m * (1.0 - o.eccentricity);
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
    // A moon with a periapsis under the parent's surface is an authored
    // orbit through solid ground — `periapsis` is center-to-center, so
    // the clearance is both radii. (Non-finite `a`/`e` was already
    // reported above; NaN comparisons are false, so this never
    // double-reports.)
    let clearance = p.def.radius_m + b.def.radius_m;
    if periapsis <= clearance {
        out.push(ValidationError::PeriapsisInsideParent {
            parent: p.def.id.as_str().to_string(),
            child: body.to_string(),
            periapsis_m: periapsis,
            clearance_m: clearance,
        });
    }
    // The body's own SOI must nest strictly inside its own orbit:
    // `soi >= periapsis` means the SOI sphere reaches its parent at
    // closest approach — every vessel near the parent is then also
    // inside the child's SOI (and the §2.6.6 transition scan sees a
    // parent-inside-child contradiction).
    if let Some(soi) = b.def.soi_m
        && soi >= periapsis
    {
        out.push(ValidationError::SoiExceedsOrbit {
            body: body.to_string(),
            soi_m: soi,
            periapsis_m: periapsis,
        });
    }
    check_sibling_soi_overlap(b, defs, out);
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

/// §3.0 sibling rule: two children of one parent whose SOI spheres can
/// overlap. The statically provable shape is *disjoint radial shells*:
/// while `[rp_x, apo_x]` and `[rp_y, apo_y]` do not overlap, the bodies'
/// center distance is bounded below by the shell gap regardless of
/// inclination, so `soi_x + soi_y > gap` means the spheres can meet at
/// closest approach (the §2.6.6 scan then sees a vessel inside two
/// sibling SOIs at once — an ambiguous transition). Overlapping shells
/// prove nothing statically — inclined orbits can share a radial range
/// without ever nearing — so those pairs are not flagged here.
///
/// Runs once per unordered pair: `b` reports only siblings whose id
/// sorts after its own. Siblings with `soi_m: None` (infinite) are
/// already flagged by the `SoiMissing` gate and skipped here.
fn check_sibling_soi_overlap(
    b: &crate::load::LoadedBody,
    defs: &DefMap,
    out: &mut Vec<ValidationError>,
) {
    let (Some(o), Some(soi)) = (&b.def.orbit, b.def.soi_m) else {
        return;
    };
    let rp = o.semi_major_axis_m * (1.0 - o.eccentricity);
    let apo = o.semi_major_axis_m * (1.0 + o.eccentricity);
    for s in defs.values() {
        if s.def.id == b.def.id || s.def.parent != b.def.parent {
            continue;
        }
        // Each unordered pair reports exactly once, under the sibling
        // whose id sorts first.
        if b.def.id.as_str() > s.def.id.as_str() {
            continue;
        }
        let (Some(so), Some(s_soi)) = (&s.def.orbit, s.def.soi_m) else {
            continue;
        };
        let s_rp = so.semi_major_axis_m * (1.0 - so.eccentricity);
        let s_apo = so.semi_major_axis_m * (1.0 + so.eccentricity);
        // The gap between the shells, positive only when they are
        // disjoint (the lower orbit's apoapsis below the higher's
        // periapsis); overlapping shells → not statically provable.
        let gap = (rp - s_apo).max(s_rp - apo);
        if gap > 0.0 && soi + s_soi > gap {
            out.push(ValidationError::SiblingSoiOverlap {
                parent: b
                    .def
                    .parent
                    .as_ref()
                    .map_or_else(String::new, |p| p.as_str().to_string()),
                a: b.def.id.as_str().to_string(),
                b: s.def.id.as_str().to_string(),
                gap_m: gap,
                soi_sum_m: soi + s_soi,
            });
        }
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
        check_id(e.id.as_str(), out);
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
    // Header pass for §3.0.1's dimension cap — `BiomeMapTooLarge`
    // carries the declared dims, which the decode's `&'static str`
    // cannot. [`decode_indexed_png`] enforces the same bound at header
    // parse regardless (pre-inflate), so an oversized map is rejected
    // either way and gets no pixel pass.
    if let Ok(info) = inspect_indexed_png(&bm.png)
        && (info.width > BIOME_MAP_MAX_W || info.height > BIOME_MAP_MAX_H)
    {
        out.push(ValidationError::BiomeMapTooLarge {
            body: body.to_string(),
            width: info.width,
            height: info.height,
        });
        return;
    }
    // Pixels, not just headers: a map that paints a palette index the
    // legend never names is an unnamed biome region, and a corrupt
    // IDAT is data rot — both surface only through the decode
    // ([`crate::biome::decode_indexed_png`] runs the same header rules
    // `inspect_indexed_png` did, plus inflate + palette bounds).
    match decode_indexed_png(&bm.png) {
        Err(reason) => out.push(ValidationError::BiomeMapPngInvalid {
            body: body.to_string(),
            reason,
        }),
        Ok(img) => {
            let info = img.info;
            for e in &bm.legend.entries {
                if usize::from(e.index) >= info.palette_len {
                    out.push(ValidationError::BiomeIndexExceedsPalette {
                        body: body.to_string(),
                        index: e.index,
                        palette_len: info.palette_len,
                    });
                }
            }
            // `indices` is exactly the legend's coverage set — every
            // pixel index must land in it. One error per uncovered
            // index, not per pixel.
            let mut reported = BTreeSet::new();
            for &i in &img.indices {
                if !indices.contains(&i) && reported.insert(i) {
                    out.push(ValidationError::BiomeIndexNotInLegend {
                        body: body.to_string(),
                        index: i,
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
            expected: RESOURCE_SCHEMA_VERSION,
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

/// §8.3's `balance.ron` checks: `schema_version` current; every numeric
/// tunable finite and inside the domain its `csp-sim` `*Config::is_valid`
/// mirror gates at restore (`DockingConfig`, `TransferConfig`,
/// `ScienceConfig`, `EconomyConfig`, `LifeSupportConfig`,
/// `ReliabilityConfig` — the authored file must never produce a config
/// the sim itself would reject); and `reliability_bench_resource`
/// resolvable against `resources.ron` — the bench run reads the site
/// inventory through it, so an unresolved id silently charges nothing.
///
/// Sign conventions: strictly-positive where a zero is not a tunable
/// (radii, rates, cadences — `part_positive`'s rule), non-negative for
/// magnitudes that may be authored off (`dormant_stress_scale: 0`
/// disables the mechanic). Ranges beyond the sign report as
/// [`ValidationError::BadBalance`]. Ungated counters — `max_debris`,
/// `max_live_joints_per_bubble`, `capture_window_ticks`,
/// `crew_transfer_ticks`, `starting_funds_micros`,
/// `script_op_budget`, `reliability_notification_cap`, and
/// `max_tracked_vessels` — all admit `0` as a legal degenerate (no
/// debris, no joints, an empty arena) and carry no finite domain to
/// check.
///
/// The `dock_soft_pull_per_s < 128` bound is the explicit-Euler
/// stability limit of the soft-capture map `c_{n+1} ≈ (1 − pull·dt)·c_n`
/// at the pinned `1/64` s tick (`pull·dt < 2`); `csp-sim`
/// `DockingConfig::is_valid` is its canonical home (constant
/// `joints::SOFT_PULL_LIMIT_PER_S` = `2/PHYSICS_DT`) — `csp-data`
/// cannot call it (the dependency runs the other way), so the bound
/// is mirrored here with the constant named.
#[allow(clippy::too_many_lines)] // one line per field reads as a table
pub fn validate_balance(
    b: &crate::balance::BalanceDef,
    res: &ResourceFile,
) -> Vec<ValidationError> {
    /// The `part` slot balance errors stamp — the `resources.ron`
    /// convention above.
    const WHO: &str = "balance.ron";
    /// Finite and `> 0`.
    fn pos(field: &'static str, v: f64, out: &mut Vec<ValidationError>) {
        part_positive(WHO, field, v, out);
    }
    /// Finite and `≥ 0`.
    fn nonneg(field: &'static str, v: f64, out: &mut Vec<ValidationError>) {
        if !v.is_finite() {
            out.push(ValidationError::PartNonFinite {
                part: WHO.to_string(),
                field,
            });
        } else if v < 0.0 {
            out.push(ValidationError::PartNonPositive {
                part: WHO.to_string(),
                field,
                value: v,
            });
        }
    }
    /// Finite, then the authored band — a band failure is a
    /// [`ValidationError::BadBalance`], not a sign failure.
    fn band(
        field: &'static str,
        v: f64,
        in_band: impl FnOnce(f64) -> bool,
        reason: &'static str,
        out: &mut Vec<ValidationError>,
    ) {
        if !v.is_finite() {
            out.push(ValidationError::PartNonFinite {
                part: WHO.to_string(),
                field,
            });
        } else if !in_band(v) {
            out.push(ValidationError::BadBalance { field, reason });
        }
    }

    let mut out = Vec::new();
    if b.schema_version != crate::balance::BALANCE_SCHEMA_VERSION {
        out.push(ValidationError::SchemaVersion {
            file: WHO.to_string(),
            found: b.schema_version,
            expected: crate::balance::BALANCE_SCHEMA_VERSION,
        });
    }
    // §2.3/§2.6.5/§2.9 world-shape tunables.
    pos("bubble_radius_m", b.bubble_radius_m, &mut out);
    if b.advance_to_max_events_per_frame == 0 {
        out.push(ValidationError::BadBalance {
            field: "advance_to_max_events_per_frame",
            reason: "zero — every advance_to call would overflow on its first event",
        });
    }
    // §2.4 docking block — `DockingConfig::is_valid`'s finite-≥0 domain,
    // plus the authored refinements `from_balance` documents.
    nonneg("dock_capture_range_m", b.dock_capture_range_m, &mut out);
    band(
        "dock_capture_angle_deg",
        b.dock_capture_angle_deg,
        |v| v > 0.0 && v <= 90.0,
        "outside the authored (0, 90] capture cone — from_balance clamps into it",
        &mut out,
    );
    nonneg(
        "dock_capture_rel_speed_mps",
        b.dock_capture_rel_speed_mps,
        &mut out,
    );
    nonneg("dock_soft_pull_per_s", b.dock_soft_pull_per_s, &mut out);
    if b.dock_soft_pull_per_s >= 128.0 {
        out.push(ValidationError::BadBalance {
            field: "dock_soft_pull_per_s",
            reason: ">= 128 — the soft-capture Euler step diverges (pull*dt >= 2 at dt = 1/64 s)",
        });
    }
    nonneg(
        "dock_soft_max_correction_mps",
        b.dock_soft_max_correction_mps,
        &mut out,
    );
    nonneg(
        "dock_latch_separation_m",
        b.dock_latch_separation_m,
        &mut out,
    );
    nonneg(
        "dock_latch_rel_speed_mps",
        b.dock_latch_rel_speed_mps,
        &mut out,
    );
    // §3.5 transfers — `TransferConfig::is_valid`: radius ≥ 0, rate > 0.
    nonneg("transfer_radius_m", b.transfer_radius_m, &mut out);
    pos(
        "transfer_rate_units_per_s",
        b.transfer_rate_units_per_s,
        &mut out,
    );
    // §3.2 science — `ScienceConfig::is_valid`: k, epsilon, η > 0; the
    // transmit cost ≥ 0.
    pos("science_softcap_k", b.science_softcap_k, &mut out);
    pos("research_data_epsilon", b.research_data_epsilon, &mut out);
    pos("lab_science_per_data", b.lab_science_per_data, &mut out);
    nonneg(
        "transmit_ec_per_data_unit",
        b.transmit_ec_per_data_unit,
        &mut out,
    );
    // §3.5 economy — `EconomyConfig::is_valid`: salary cadence nonzero,
    // payout ≥ 0, shares in [0,1], salary/upkeep rates ≥ 0, manufacture
    // rate > 0.
    if b.salary_period_days == 0 {
        out.push(ValidationError::BadBalance {
            field: "salary_period_days",
            reason: "zero — the salary cadence must be nonzero",
        });
    }
    nonneg("crew_hire_cost_funds", b.crew_hire_cost_funds, &mut out);
    // C21-F8: founding may be authored free (colony bootstrap worlds)
    // but never negative/non-finite — `to_micros` is the mirror.
    nonneg(
        "colony_founding_cost_funds",
        b.colony_founding_cost_funds,
        &mut out,
    );
    // `EconomyConfig::is_valid`'s finite-≥0 mirror — a NaN or negative
    // salary/upkeep passes the loader but `set_economy_config`/
    // `World::restore` rejects it.
    nonneg("crew_salary_per_s", b.crew_salary_per_s, &mut out);
    nonneg(
        "site_upkeep_funds_per_s",
        b.site_upkeep_funds_per_s,
        &mut out,
    );
    nonneg(
        "science_funds_per_point",
        b.science_funds_per_point,
        &mut out,
    );
    band(
        "recovery_efficiency",
        b.recovery_efficiency,
        |v| (0.0..=1.0).contains(&v),
        "outside [0, 1] — it is a share of recovered value",
        &mut out,
    );
    band(
        "abandon_recovery",
        b.abandon_recovery,
        |v| (0.0..=1.0).contains(&v),
        "outside [0, 1] — it is a share of recovered value",
        &mut out,
    );
    // §3.6 board — `ContractConfig::is_valid`: board size nonzero.
    if b.contract_offer_slots == 0 {
        out.push(ValidationError::BadBalance {
            field: "contract_offer_slots",
            reason: "zero — the offer board would never stock",
        });
    }
    // §3.5 manufacture + life support — rates ≥ 0, cadence > 0.
    pos(
        "manufacture_rate_kg_per_s",
        b.manufacture_rate_kg_per_s,
        &mut out,
    );
    pos(
        "life_support_check_period_s",
        b.life_support_check_period_s,
        &mut out,
    );
    nonneg(
        "life_support_oxygen_per_crew_s",
        b.life_support_oxygen_per_crew_s,
        &mut out,
    );
    nonneg(
        "life_support_food_per_crew_s",
        b.life_support_food_per_crew_s,
        &mut out,
    );
    nonneg(
        "life_support_ec_per_crew_s",
        b.life_support_ec_per_crew_s,
        &mut out,
    );
    // §3.4 reliability — `ReliabilityConfig::is_valid` domains.
    nonneg(
        "reliability_dormant_stress_scale",
        b.reliability_dormant_stress_scale,
        &mut out,
    );
    nonneg(
        "reliability_ignition_hazard_s",
        b.reliability_ignition_hazard_s,
        &mut out,
    );
    band(
        "reliability_degraded_thrust_factor",
        b.reliability_degraded_thrust_factor,
        |v| (0.0..=1.0).contains(&v),
        "outside [0, 1] — a degraded-thrust multiplier may not exceed full thrust",
        &mut out,
    );
    nonneg(
        "reliability_leak_units_per_s",
        b.reliability_leak_units_per_s,
        &mut out,
    );
    pos(
        "reliability_infant_tau_s",
        b.reliability_infant_tau_s,
        &mut out,
    );
    nonneg(
        "reliability_flight_units_per_s_vacuum",
        b.reliability_flight_units_per_s_vacuum,
        &mut out,
    );
    nonneg(
        "reliability_flight_units_per_s_atmosphere",
        b.reliability_flight_units_per_s_atmosphere,
        &mut out,
    );
    nonneg(
        "reliability_bench_funds_per_unit",
        b.reliability_bench_funds_per_unit,
        &mut out,
    );
    check_id(b.reliability_bench_resource.as_str(), &mut out);
    if !res.contains(&b.reliability_bench_resource) {
        out.push(ValidationError::UnknownResource {
            part: WHO.to_string(),
            resource: b.reliability_bench_resource.as_str().to_string(),
        });
    }
    nonneg(
        "reliability_bench_resource_per_unit",
        b.reliability_bench_resource_per_unit,
        &mut out,
    );
    nonneg(
        "reliability_bench_seconds_per_unit",
        b.reliability_bench_seconds_per_unit,
        &mut out,
    );
    out
}

/// §1.2's `loc/en.ron` file-shape check repeated validator-side — the
/// same `schema_version` gate [`crate::load::load_loc`] runs, here for
/// hand-rolled tables (the "validator re-checks what serde enforced"
/// convention). The key-resolution half lives in the
/// `validate_*_loc_keys` family.
pub fn validate_loc(loc: &LocFile) -> Vec<ValidationError> {
    if loc.schema_version != SCHEMA_VERSION {
        vec![ValidationError::SchemaVersion {
            file: "loc/en.ron".to_string(),
            found: loc.schema_version,
            expected: SCHEMA_VERSION,
        }]
    } else {
        Vec::new()
    }
}

/// §3.5's `sites.ron` checks: `schema_version` (the loader's
/// `current|previous` band), unique ids, every site on a declared
/// system body, a finite in-range geocentric pose, at most one launch
/// site per body (§3.5's "1.0 at the launch site" needs a definite
/// site to mean it), and M8-B's `resource_yields` rows — unique,
/// known resources, finite fractions in `[0,1]`.
pub fn validate_sites(
    file: &SiteFile,
    sys: &LoadedSystem,
    res: &ResourceFile,
) -> Vec<ValidationError> {
    let mut out = Vec::new();
    if !crate::site::SITE_SCHEMA_SUPPORTED.contains(&file.schema_version) {
        out.push(ValidationError::SchemaVersion {
            file: "sites.ron".to_string(),
            found: file.schema_version,
            expected: crate::site::SITE_SCHEMA_VERSION,
        });
    }
    let bodies: BTreeSet<&str> = sys.bodies.iter().map(|b| b.def.id.as_str()).collect();
    let mut seen = BTreeSet::new();
    let mut launch_bodies = BTreeSet::new();
    let half_pi = core::f64::consts::FRAC_PI_2;
    for s in &file.sites {
        let id = s.id.as_str();
        check_id(id, &mut out);
        // Each entry re-declares its own version (§3.1 first-field
        // convention): the same current-or-previous band as the file.
        if !crate::site::SITE_SCHEMA_SUPPORTED.contains(&s.schema_version) {
            out.push(ValidationError::SchemaVersion {
                file: format!("sites.ron entry {id:?}"),
                found: s.schema_version,
                expected: crate::site::SITE_SCHEMA_VERSION,
            });
        }
        if !seen.insert(id) {
            out.push(ValidationError::InvalidSite {
                id: id.to_string(),
                reason: "duplicate site id".to_string(),
            });
        }
        if !bodies.contains(s.body.as_str()) {
            out.push(ValidationError::InvalidSite {
                id: id.to_string(),
                reason: format!("body {:?} not in this system", s.body.as_str()),
            });
        }
        if !(s.latitude_rad.is_finite() && s.latitude_rad.abs() <= half_pi) {
            out.push(ValidationError::InvalidSite {
                id: id.to_string(),
                reason: "latitude out of [-pi/2, pi/2] or non-finite".to_string(),
            });
        }
        if !s.longitude_rad.is_finite() {
            out.push(ValidationError::InvalidSite {
                id: id.to_string(),
                reason: "longitude non-finite".to_string(),
            });
        }
        if !(s.altitude_m.is_finite() && s.altitude_m >= 0.0) {
            out.push(ValidationError::InvalidSite {
                id: id.to_string(),
                reason: "altitude negative or non-finite".to_string(),
            });
        }
        // C09-F2/C34-F3's terminator-distance guard: on a zero-synodic
        // body the site's daylight is a PERMANENT verdict hung on
        // `cos H · cos φ` — a site authored near the line lets one
        // longitude ulp flip its whole power budget forever.
        // `None` covers an unresolved body (already flagged above) or
        // a non-fixed sun (the verdict cycles — grazing is legal).
        if let Some(margin) = fixed_sun_cos_margin(sys, s)
            && margin < FIXED_SUN_TERMINATOR_MIN_COS
        {
            out.push(ValidationError::InvalidSite {
                id: id.to_string(),
                reason: format!(
                    "site sits {margin:.3e} of |cos H·cos φ| off a FIXED terminator \
                     (zero-synodic body {:?}) — the permanent Day/Night verdict must not \
                     hang on a rounding residue; keep ≥ {FIXED_SUN_TERMINATOR_MIN_COS}",
                    s.body.as_str()
                ),
            });
        }
        if s.launch && !launch_bodies.insert(s.body.as_str()) {
            out.push(ValidationError::InvalidSite {
                id: id.to_string(),
                reason: format!(
                    "second launch site on {:?} — one launch site per body",
                    s.body.as_str()
                ),
            });
        }
        let mut yields = BTreeSet::new();
        for (r, y) in &s.resource_yields {
            if !yields.insert(r.as_str()) {
                out.push(ValidationError::InvalidSite {
                    id: id.to_string(),
                    reason: format!("duplicate yield resource {r:?}"),
                });
            }
            if !res.contains(r) {
                out.push(ValidationError::InvalidSite {
                    id: id.to_string(),
                    reason: format!("unknown yield resource {r:?}"),
                });
            }
            if !(y.is_finite() && (0.0..=1.0).contains(y)) {
                out.push(ValidationError::InvalidSite {
                    id: id.to_string(),
                    reason: format!("yield fraction for {r:?} outside [0,1] or non-finite"),
                });
            }
        }
        validate_site_stock(s, id, res, &mut out);
    }
    out
}

/// W2.6: one site's `starting_stock` rows — the seeded stockpile
/// build orders and bench stands spend. Unique, known resources,
/// finite non-negative units (zero is pointless but legal — an
/// authored "this row exists" marker).
fn validate_site_stock(s: &SiteDef, id: &str, res: &ResourceFile, out: &mut Vec<ValidationError>) {
    let mut stock = BTreeSet::new();
    for (r, u) in &s.starting_stock {
        if !stock.insert(r.as_str()) {
            out.push(ValidationError::InvalidSite {
                id: id.to_string(),
                reason: format!("duplicate stock resource {r:?}"),
            });
        }
        if !res.contains(r) {
            out.push(ValidationError::InvalidSite {
                id: id.to_string(),
                reason: format!("unknown stock resource {r:?}"),
            });
        }
        if !(u.is_finite() && *u >= 0.0) {
            out.push(ValidationError::InvalidSite {
                id: id.to_string(),
                reason: format!("stock units for {r:?} negative or non-finite"),
            });
        }
    }
}

/// The site's `|cos H0 · cos φ|` terminator margin — `Some` only when
/// the body carries a FIXED sun (`|ω − n_sun|` under
/// [`FIXED_SUN_SYNODIC_EPS`], `csp-sim::colonies::next_terminator_tick`'s
/// no-crossings bound), where the product is a permanent Day/Night
/// verdict rather than a momentary phase. Replicates
/// `body_light`'s closed form — the sim depends on this crate, never
/// the reverse, so the predicate is re-derived rather than imported:
/// `n = 2π/T`, `L0 = M0 + ω_arg + Ω_node`, `H0 = λ − L0 − π`, and for
/// a tidally-locked body (`rotation_period_s` absent or ≤ 0 — spin IS
/// the orbital mean motion) the sun-direction sweep is the
/// star-orbiting ancestor's `(n, L0)`, walked up `parent` links while
/// parents still carry elements (a planet locked to the star itself
/// resolves to its own orbit — the genuinely fixed sun). A rotating
/// body keeps its own orbit's `n` — `body_light`'s non-locked path,
/// verbatim.
fn fixed_sun_cos_margin(sys: &LoadedSystem, s: &SiteDef) -> Option<f64> {
    const TWO_PI: f64 = 2.0 * core::f64::consts::PI;
    let bodies: BTreeMap<&str, &crate::system::BodyDef> = sys
        .bodies
        .iter()
        .map(|b| (b.def.id.as_str(), &b.def))
        .collect();
    let def = bodies.get(s.body.as_str())?;
    let motion = |d: &crate::system::BodyDef| -> (f64, f64) {
        d.orbit.as_ref().map_or((0.0, 0.0), |e: &KeplerElements| {
            let n = if e.period_s.is_finite() && e.period_s > 0.0 {
                TWO_PI / e.period_s
            } else {
                0.0
            };
            (
                n,
                e.mean_anomaly_epoch_rad + e.arg_periapsis_rad + e.ascending_node_rad,
            )
        })
    };
    let (n_body, _) = motion(def);
    let locked = !def.rotation_period_s.is_some_and(|p| p > 0.0);
    let omega = def
        .rotation_period_s
        .map_or(n_body, |p| if p > 0.0 { TWO_PI / p } else { n_body });
    let (n_sun, l0_sun) = if locked {
        // Walk the parent chain while parents still orbit — the walk
        // ends at the body that orbits the star directly (its parent
        // carries no elements). Bounded by the body count: parent
        // chains are `validate_system`-proven acyclic, and an
        // unvalidated cycle resolves to the last ancestor reached.
        let mut current = def;
        for _ in 0..bodies.len() {
            match current.parent.as_ref().and_then(|p| bodies.get(p.as_str())) {
                Some(parent) if parent.orbit.is_some() => current = parent,
                _ => break,
            }
        }
        motion(current)
    } else {
        let (_, l0) = motion(def);
        (n_body, l0)
    };
    if (omega - n_sun).abs() >= FIXED_SUN_SYNODIC_EPS {
        return None;
    }
    let h0 = s.longitude_rad - l0_sun - core::f64::consts::PI;
    Some((libm::cos(h0) * libm::cos(s.latitude_rad)).abs())
}

/// Loc resolution for the site set (the [`validate_loc_keys`]
/// pattern): every site's `display_name` must resolve in `en.ron`.
/// Separate from [`validate_sites`] because the loc file lives
/// outside the system directory.
pub fn validate_site_loc_keys(file: &SiteFile, loc: &LocFile) -> Vec<ValidationError> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for s in &file.sites {
        let key = &s.display_name;
        if seen.insert(key.as_str().to_string()) && !loc.contains(key) {
            out.push(ValidationError::UnresolvedLocKey {
                context: format!("site {}", s.id),
                key: key.as_str().to_string(),
            });
        }
    }
    out
}

/// `data/recipes/*.ron` checks: `schema_version`, `id` ↔ file stem,
/// unique ids, every `inputs`/`outputs` row a known resource with a
/// finite non-negative rate, at least one positive-rate output (a
/// recipe with no product is dead data), no resource on both sides
/// of one recipe (a self-loop — the recipe graph's degenerate
/// cycle), no net-positive yield (Σ outputs > Σ inputs mints units
/// against no real supply), and an ACYCLIC cross-recipe digraph —
/// §8.2's "cycles" gate: a cyclic converter web can hold itself
/// grounded in the colony solver and, at a non-negative net, mint
/// forever (C32-F2; the runtime pin that catches the same class is
/// `unanchored_mint_pins` in csp-sim).
pub fn validate_recipes(
    loaded: &crate::load::LoadedRecipes,
    res: &ResourceFile,
) -> Vec<ValidationError> {
    let mut out = Vec::new();
    let bad = |id: &str, reason: &str, out: &mut Vec<ValidationError>| {
        out.push(ValidationError::InvalidRecipe {
            id: id.to_string(),
            reason: reason.to_string(),
        });
    };
    let mut seen_ids = BTreeSet::new();
    for lr in &loaded.recipes {
        let d = &lr.def;
        check_id(d.id.as_str(), &mut out);
        let stem = lr
            .path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        if stem != d.id.as_str() {
            bad(
                d.id.as_str(),
                "recipe id does not match file stem",
                &mut out,
            );
        }
        if !seen_ids.insert(d.id.clone()) {
            bad(d.id.as_str(), "duplicate recipe id", &mut out);
        }
        if d.schema_version != crate::recipe::RECIPE_SCHEMA_VERSION {
            bad(d.id.as_str(), "unsupported schema_version", &mut out);
        }
        for (what, list) in [("inputs", &d.inputs), ("outputs", &d.outputs)] {
            let mut seen = BTreeSet::new();
            for (r, q) in list {
                if !seen.insert(r.as_str()) {
                    bad(
                        d.id.as_str(),
                        &format!("duplicate resource {r:?} in {what}"),
                        &mut out,
                    );
                }
                if !res.contains(r) {
                    bad(
                        d.id.as_str(),
                        &format!("unknown resource {r:?} in {what}"),
                        &mut out,
                    );
                }
                if !(q.is_finite() && *q >= 0.0) {
                    bad(
                        d.id.as_str(),
                        &format!("non-finite or negative {what} rate for {r:?}"),
                        &mut out,
                    );
                }
            }
        }
        if !d.outputs.iter().any(|(_, q)| *q > 0.0) {
            bad(d.id.as_str(), "recipe produces nothing", &mut out);
        }
        // A positive-rate resource on BOTH sides of one recipe is a
        // self-loop — the recipe graph's degenerate cycle (C32-F2):
        // net-positive it mints, net-negative it quietly eats itself.
        for (r, q) in &d.inputs {
            if *q > 0.0 && d.outputs.iter().any(|(or, oq)| or == r && *oq > 0.0) {
                bad(
                    d.id.as_str(),
                    &format!("resource {r:?} is both an input and an output"),
                    &mut out,
                );
            }
        }
        // Net yield: Σ inputs ≥ Σ outputs — every authored recipe
        // refines at a loss; a net-positive recipe mints units
        // against no real supply even before graph cycles (C32-F2).
        let in_total: f64 = d.inputs.iter().map(|(_, q)| *q).sum();
        let out_total: f64 = d.outputs.iter().map(|(_, q)| *q).sum();
        if out_total - in_total > 1e-9 {
            bad(
                d.id.as_str(),
                "net-positive yield: outputs exceed inputs",
                &mut out,
            );
        }
    }
    check_recipe_graph(loaded, &mut out);
    out
}

/// The cross-recipe digraph must be ACYCLIC (PLAN §8.2's "cycles"
/// gate, C32-F2): edge r→s when a positive-rate output of r is a
/// positive-rate input of s. Any directed cycle lets a converter
/// web hold itself grounded in the colony solver and — at a
/// non-negative aggregate net — mint forever (the C09-F1 runtime
/// hole, now pinned sim-side; this is the authored-data gate that
/// keeps it unshippable). Duplicate ids are already flagged by
/// [`validate_recipes`]; `by_id`'s first-wins table keeps this
/// build deterministic regardless. Every cyclic recipe is flagged
/// once, naming the whole component, in ascending id order.
fn check_recipe_graph(loaded: &crate::load::LoadedRecipes, out: &mut Vec<ValidationError>) {
    let by_id = loaded.by_id();
    let ids: Vec<_> = by_id.keys().cloned().collect();
    // `feeds[i]` — the recipes whose outputs i consumes.
    let feeds: Vec<BTreeSet<usize>> = ids
        .iter()
        .map(|id| {
            let def = by_id[id];
            let ins: BTreeSet<_> = def
                .inputs
                .iter()
                .filter(|(_, q)| *q > 0.0)
                .map(|(r, _)| r)
                .collect();
            ids.iter()
                .enumerate()
                .filter(|(_, other)| {
                    **other != *id
                        && by_id[*other]
                            .outputs
                            .iter()
                            .any(|(r, q)| *q > 0.0 && ins.contains(r))
                })
                .map(|(j, _)| j)
                .collect()
        })
        .collect();
    // Mutual reachability partitions the graph into SCCs — the
    // recipe count is small, so the per-source walk stays trivial,
    // and ascending `ids` order (BTreeMap) makes the report
    // deterministic. An SCC of ≥2 IS a directed cycle.
    let reach: Vec<BTreeSet<usize>> = (0..ids.len())
        .map(|i| {
            let mut seen = BTreeSet::new();
            let mut stack: Vec<usize> = feeds[i].iter().copied().collect();
            while let Some(j) = stack.pop() {
                if seen.insert(j) {
                    stack.extend(feeds[j].iter().copied());
                }
            }
            seen
        })
        .collect();
    let mut assigned = vec![false; ids.len()];
    for i in 0..ids.len() {
        if assigned[i] {
            continue;
        }
        let members: Vec<usize> = (0..ids.len())
            .filter(|&j| j == i || (reach[i].contains(&j) && reach[j].contains(&i)))
            .collect();
        for &j in &members {
            assigned[j] = true;
        }
        if members.len() > 1 {
            let cycle = members
                .iter()
                .map(|&j| ids[j].as_str())
                .collect::<Vec<_>>()
                .join(" ↔ ");
            for &j in &members {
                out.push(ValidationError::InvalidRecipe {
                    id: ids[j].as_str().to_string(),
                    reason: format!("recipe graph cycle: {cycle}"),
                });
            }
        }
    }
}

/// Loc resolution for the recipe set (the [`validate_site_loc_keys`]
/// pattern): every recipe's `display_name` must resolve in `en.ron`.
/// Separate from [`validate_recipes`] because the loc file lives
/// outside the recipes directory.
pub fn validate_recipe_loc_keys(
    loaded: &crate::load::LoadedRecipes,
    loc: &LocFile,
) -> Vec<ValidationError> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for lr in &loaded.recipes {
        let key = &lr.def.display_name;
        if seen.insert(key.as_str().to_string()) && !loc.contains(key) {
            out.push(ValidationError::UnresolvedLocKey {
                context: format!("recipe {}", lr.def.id),
                key: key.as_str().to_string(),
            });
        }
    }
    out
}

fn bad_colony(site: &str, reason: String, out: &mut Vec<ValidationError>) {
    out.push(ValidationError::InvalidColony {
        site: site.to_string(),
        reason,
    });
}

/// `(resource, rate)` list checks for colony modules — unique, known,
/// finite, non-negative.
fn check_colony_pairs(
    site: &str,
    what: &str,
    list: &[(crate::ids::ResourceId, f64)],
    res: &ResourceFile,
    out: &mut Vec<ValidationError>,
) {
    let mut seen = BTreeSet::new();
    for (id, v) in list {
        if !seen.insert(id.as_str()) {
            bad_colony(site, format!("duplicate resource {id:?} in {what}"), out);
        }
        if !res.contains(id) {
            bad_colony(site, format!("unknown resource {id:?} in {what}"), out);
        }
        if !v.is_finite() {
            bad_colony(site, format!("non-finite {what} rate for {id:?}"), out);
        } else if *v < 0.0 {
            bad_colony(site, format!("negative {what} rate for {id:?}"), out);
        }
    }
}

/// The cross-field check `check_colony_pairs` cannot express: an
/// `electric_charge` entry in a `consumes`/`consumes_per_crew` list
/// plus a nonzero `power_ec_per_s` is ONE EC draw authored twice —
/// the sim's `(module, resource)` request rows would collide and the
/// grant would silently drop half the demand.
fn check_power_collision(
    site: &str,
    module: usize,
    what: &str,
    list: &[(crate::ids::ResourceId, f64)],
    power_ec_per_s: f64,
    out: &mut Vec<ValidationError>,
) {
    if power_ec_per_s > 0.0
        && list
            .iter()
            .any(|(id, q)| *q > 0.0 && id.as_str() == crate::resource::ELECTRIC_CHARGE)
    {
        bad_colony(
            site,
            format!("module {module}: electric_charge in {what} collides with power_ec_per_s"),
            out,
        );
    }
}

/// §3.5.1's `colonies.ron` checks: `schema_version`, every colony
/// founded on a declared `sites.ron` site, every resource ref known
/// to `resources.ron` and unique within its list, and every
/// rate/capacity/level field finite and non-negative (a zero rate is
/// legal — an inert module; negative is not a flow).
#[allow(clippy::too_many_lines)] // the per-module-field check list reads as a list
#[allow(clippy::cognitive_complexity)] // ditto: it is one flat per-module switch
pub fn validate_colonies(
    file: &crate::colony::ColonyFile,
    sites: &SiteFile,
    res: &ResourceFile,
    recipes: &crate::load::LoadedRecipes,
) -> Vec<ValidationError> {
    let mut out = Vec::new();
    if file.schema_version != crate::colony::COLONY_SCHEMA_VERSION {
        out.push(ValidationError::SchemaVersion {
            file: "colonies.ron".to_string(),
            found: file.schema_version,
            expected: crate::colony::COLONY_SCHEMA_VERSION,
        });
    }
    let site_ids: BTreeSet<&str> = sites.sites.iter().map(|s| s.id.as_str()).collect();
    let recipe_ids: BTreeSet<&str> = recipes.recipes.iter().map(|r| r.def.id.as_str()).collect();
    // First def wins on a duplicate id — the same convention
    // `LoadedRecipes::by_id` gives the sim (`BTreeMap::collect` would
    // keep the LAST def; `validate_recipes` flags the duplicate
    // loudly either way, but the collision check below must inspect
    // the def the sim would actually install).
    let mut recipe_defs: BTreeMap<&str, &crate::recipe::RecipeDef> = BTreeMap::new();
    for r in &recipes.recipes {
        recipe_defs.entry(r.def.id.as_str()).or_insert(&r.def);
    }
    for def in &file.colonies {
        let site = def.site.as_str();
        if !site_ids.contains(site) {
            bad_colony(site, format!("site {site:?} not in sites.ron"), &mut out);
        }
        for (i, m) in def.modules.iter().enumerate() {
            use crate::colony::ColonyModuleDef as M;
            match m {
                M::Solar {
                    area_m2,
                    efficiency,
                } => {
                    if !(area_m2.is_finite() && *area_m2 >= 0.0) {
                        bad_colony(site, format!("module {i}: bad solar area"), &mut out);
                    }
                    if !(efficiency.is_finite() && (0.0..=1.0).contains(efficiency)) {
                        bad_colony(
                            site,
                            format!("module {i}: efficiency outside [0,1]"),
                            &mut out,
                        );
                    }
                }
                M::Generator {
                    fuel,
                    fuel_rate_per_s,
                    rate_per_s,
                    ..
                } => {
                    if !res.contains(fuel) {
                        bad_colony(
                            site,
                            format!("module {i}: unknown generator fuel"),
                            &mut out,
                        );
                    }
                    for (f, v) in [("fuel rate", fuel_rate_per_s), ("rate", rate_per_s)] {
                        if !(v.is_finite() && *v >= 0.0) {
                            bad_colony(site, format!("module {i}: bad generator {f}"), &mut out);
                        }
                    }
                }
                M::Drill {
                    resource,
                    base_rate_per_s,
                    min_concentration,
                    power_ec_per_s,
                } => {
                    if !res.contains(resource) {
                        bad_colony(
                            site,
                            format!("module {i}: unknown drill resource"),
                            &mut out,
                        );
                    }
                    for (f, v) in [("rate", base_rate_per_s), ("power", power_ec_per_s)] {
                        if !(v.is_finite() && *v >= 0.0) {
                            bad_colony(site, format!("module {i}: bad drill {f}"), &mut out);
                        }
                    }
                    // The part-side `ModuleDef::Drill` bound
                    // (`check_drill_module`) — the field's doc claims
                    // parity and the sim's `conc / min_concentration`
                    // scaling makes a `> 1` author a permanent output
                    // cap, which is a trap, not an intent.
                    if !(min_concentration.is_finite() && (0.0..=1.0).contains(min_concentration)) {
                        bad_colony(
                            site,
                            format!("module {i}: drill min_concentration outside [0,1]"),
                            &mut out,
                        );
                    }
                }
                M::Converter {
                    recipe,
                    power_ec_per_s,
                    ..
                } => {
                    check_id(recipe.as_str(), &mut out);
                    if !recipe_ids.contains(recipe.as_str()) {
                        bad_colony(
                            site,
                            format!("module {i}: unknown recipe {recipe:?}"),
                            &mut out,
                        );
                    }
                    if !(power_ec_per_s.is_finite() && *power_ec_per_s >= 0.0) {
                        bad_colony(site, format!("module {i}: bad converter power"), &mut out);
                    }
                    if let Some(rdef) = recipe_defs.get(recipe.as_str()) {
                        check_power_collision(
                            site,
                            i,
                            "recipe inputs",
                            &rdef.inputs,
                            *power_ec_per_s,
                            &mut out,
                        );
                    }
                }
                M::Store { resource, capacity } => {
                    if !res.contains(resource) {
                        bad_colony(
                            site,
                            format!("module {i}: unknown store resource"),
                            &mut out,
                        );
                    }
                    if !(capacity.is_finite() && *capacity >= 0.0) {
                        bad_colony(site, format!("module {i}: bad store capacity"), &mut out);
                    }
                }
                M::Battery { capacity } => {
                    if !(capacity.is_finite() && *capacity >= 0.0) {
                        bad_colony(site, format!("module {i}: bad battery capacity"), &mut out);
                    }
                }
                M::Radiator {
                    area_m2,
                    emissivity,
                } => {
                    if !(area_m2.is_finite() && *area_m2 >= 0.0) {
                        bad_colony(site, format!("module {i}: bad radiator area"), &mut out);
                    }
                    if !(emissivity.is_finite() && (0.0..=1.0).contains(emissivity)) {
                        bad_colony(
                            site,
                            format!("module {i}: emissivity outside [0,1]"),
                            &mut out,
                        );
                    }
                }
                M::LifeSupport {
                    consumes_per_crew,
                    power_ec_per_s,
                } => {
                    check_colony_pairs(
                        site,
                        "life-support consumes_per_crew",
                        consumes_per_crew,
                        res,
                        &mut out,
                    );
                    if !(power_ec_per_s.is_finite() && *power_ec_per_s >= 0.0) {
                        bad_colony(
                            site,
                            format!("module {i}: bad life-support power"),
                            &mut out,
                        );
                    }
                    check_power_collision(
                        site,
                        i,
                        "consumes_per_crew",
                        consumes_per_crew,
                        *power_ec_per_s,
                        &mut out,
                    );
                }
            }
        }
        let mut seen = BTreeSet::new();
        for s in &def.stores {
            if !seen.insert(s.resource.as_str()) {
                bad_colony(
                    site,
                    format!("duplicate store resource {:?}", s.resource),
                    &mut out,
                );
            }
            if !res.contains(&s.resource) {
                bad_colony(
                    site,
                    format!("unknown store resource {:?}", s.resource),
                    &mut out,
                );
            }
            if !(s.level.is_finite() && s.level >= 0.0) {
                bad_colony(
                    site,
                    format!("bad store level for {:?}", s.resource),
                    &mut out,
                );
            }
        }
    }
    out
}

/// Loc resolution for the colony set (the [`validate_site_loc_keys`]
/// pattern): every colony's `display_name` must resolve in `en.ron`.
/// Separate from [`validate_colonies`] because the loc file lives
/// outside the system directory.
pub fn validate_colony_loc_keys(
    file: &crate::colony::ColonyFile,
    loc: &LocFile,
) -> Vec<ValidationError> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for c in &file.colonies {
        let key = &c.display_name;
        if seen.insert(key.as_str().to_string()) && !loc.contains(key) {
            out.push(ValidationError::UnresolvedLocKey {
                context: format!("colony at site {}", c.site),
                key: key.as_str().to_string(),
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
            // `csp_phys::resolve`'s `UnresolvableStatPath` gate, mirrored
            // with its own predicate (`StatPath::base_value` — the same
            // call `check_tree_tracks` runs for upgrade tracks): a
            // multiplier on a stat the def lacks loads green today and
            // only fails at manufacture.
            if stat.base_value(part).is_none() {
                out.push(ValidationError::UnresolvableVariantStat {
                    part: part_id.to_string(),
                    variant: v.id.as_str().to_string(),
                    stat: stat.name(),
                });
            }
        }
    }
}

/// Per-module field checks — dispatch is in [`check_modules`]; the
/// engine's checks live in [`check_engine_module`]. `recipe_ids` and
/// `experiment_ids` resolve the `Converter`/`Experiment` cross-
/// references — the data set the modules point at, not the module's
/// own fields.
fn check_module(
    part_id: &str,
    res: &ResourceFile,
    recipe_ids: &BTreeSet<&str>,
    experiment_ids: &BTreeSet<&str>,
    m: &ModuleDef,
    out: &mut Vec<ValidationError>,
) {
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
            kind,
            data_units,
            situations,
            ..
        } => {
            check_id(kind.as_str(), out);
            if !experiment_ids.contains(kind.as_str()) {
                out.push(ValidationError::UnknownExperimentKind {
                    part: part_id.to_string(),
                    experiment: kind.as_str().to_string(),
                });
            }
            part_positive(part_id, "experiment.data_units", *data_units, out);
            check_situation_mask(
                *situations,
                "experiment can run in no situation",
                |reason| {
                    out.push(ValidationError::BadModule {
                        part: part_id.to_string(),
                        reason,
                    });
                },
            );
        }
        ModuleDef::Drill { .. } => check_drill_module(part_id, res, m, out),
        ModuleDef::Converter {
            recipe, power_w, ..
        } => {
            check_id(recipe.as_str(), out);
            if !recipe_ids.contains(recipe.as_str()) {
                out.push(ValidationError::UnknownRecipe {
                    part: part_id.to_string(),
                    recipe: recipe.as_str().to_string(),
                });
            }
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
        ModuleDef::Parachute { .. } => check_parachute_module(part_id, m, out),
    }
}

/// The `Parachute` module's own rules: positive catch ceilings,
/// non-negative areas with deployed > packed (an equal-or-smaller
/// canopy is a no-op deploy, not a decelerator), and a non-negative
/// inflation ramp.
fn check_parachute_module(part_id: &str, m: &ModuleDef, out: &mut Vec<ValidationError>) {
    let ModuleDef::Parachute {
        deploy_alt_m,
        deploy_speed_mps,
        packed_drag_area_m2,
        deployed_drag_area_m2,
        deploy_time_s,
    } = m
    else {
        return;
    };
    part_positive(part_id, "parachute.deploy_alt_m", *deploy_alt_m, out);
    part_positive(
        part_id,
        "parachute.deploy_speed_mps",
        *deploy_speed_mps,
        out,
    );
    if !(packed_drag_area_m2.is_finite() && *packed_drag_area_m2 >= 0.0) {
        out.push(ValidationError::BadModule {
            part: part_id.to_string(),
            reason: "parachute packed_drag_area_m2 negative or non-finite",
        });
    }
    if !(deployed_drag_area_m2.is_finite()
        && packed_drag_area_m2.is_finite()
        && *deployed_drag_area_m2 > *packed_drag_area_m2)
    {
        out.push(ValidationError::BadModule {
            part: part_id.to_string(),
            reason: "parachute deployed_drag_area_m2 must exceed packed_drag_area_m2",
        });
    }
    if !(deploy_time_s.is_finite() && *deploy_time_s >= 0.0) {
        out.push(ValidationError::BadModule {
            part: part_id.to_string(),
            reason: "parachute deploy_time_s negative or non-finite",
        });
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
        // `csp_phys::resolve::check_engine_propellants`' gate, mirrored
        // with the same polarity: a non-finite sum (two authored
        // fractions can overflow to +inf at ~1e308 apiece) is a
        // rejection, not a pass — the resolver errors on it, so the
        // validator must too.
        if !(sum.is_finite() && (sum - 1.0).abs() <= PROPELLANT_SUM_TOL) {
            out.push(ValidationError::BadModule {
                part: part_id.to_string(),
                reason: "propellant mass fractions do not sum to 1",
            });
        }
    }
}

fn check_modules(
    part: &PartDef,
    res: &ResourceFile,
    recipe_ids: &BTreeSet<&str>,
    experiment_ids: &BTreeSet<&str>,
    out: &mut Vec<ValidationError>,
) {
    let part_id = part.id.as_str();
    for m in &part.modules {
        check_module(part_id, res, recipe_ids, experiment_ids, m, out);
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
    // One decelerator per part — the sim's per-node deploy record is
    // a single flag; a second spec would be ambiguous authored data.
    if part
        .modules
        .iter()
        .filter(|m| matches!(m, ModuleDef::Parachute { .. }))
        .count()
        > 1
    {
        out.push(ValidationError::BadModule {
            part: part_id.to_string(),
            reason: "more than one parachute module",
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
/// (`build_resources`, tank capacities, propellants, drill outputs);
/// `recipes` and `experiments` resolve the `Converter::recipe` and
/// `Experiment::kind` cross-references — a converter naming a missing
/// recipe is a dead module ([`ValidationError::UnknownRecipe`]), an
/// instrument naming a missing experiment kind can never run
/// ([`ValidationError::UnknownExperimentKind`]).
pub fn validate_parts(
    loaded: &LoadedParts,
    res: &ResourceFile,
    recipes: &crate::load::LoadedRecipes,
    experiments: &crate::load::LoadedExperiments,
) -> Vec<ValidationError> {
    let mut out = Vec::new();
    let recipe_ids: BTreeSet<&str> = recipes.recipes.iter().map(|r| r.def.id.as_str()).collect();
    let experiment_ids: BTreeSet<&str> = experiments
        .experiments
        .iter()
        .map(|e| e.def.id.as_str())
        .collect();
    let mut ids = BTreeSet::new();
    for p in &loaded.parts {
        check_part(p, res, &recipe_ids, &experiment_ids, &mut out);
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
fn check_part(
    p: &LoadedPart,
    res: &ResourceFile,
    recipe_ids: &BTreeSet<&str>,
    experiment_ids: &BTreeSet<&str>,
    out: &mut Vec<ValidationError>,
) {
    let part = &p.def;
    let part_id = part.id.as_str();
    if part.schema_version != PART_SCHEMA_VERSION {
        out.push(ValidationError::SchemaVersion {
            file: format!("parts/{}", p.path.display()),
            found: part.schema_version,
            expected: PART_SCHEMA_VERSION,
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
    check_modules(part, res, recipe_ids, experiment_ids, out);
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
/// The §3.2 experiment-def rules over `data/experiments/`: `id` ↔
/// file-stem agreement, unique IDs, `schema_version` current,
/// `transmit_ratio` inside the authored `(0.2, 1.0]` band, caps finite
/// and positive with non-overlapping `(body, situation)` coverage, and
/// every `cap.body` resolvable against `sys` — an unreachable cap is
/// dead authored data (`cap_for` exact-matches the id). Typed error
/// list like every validator here — never asserts.
pub fn validate_experiments(
    loaded: &crate::load::LoadedExperiments,
    sys: &LoadedSystem,
) -> Vec<ValidationError> {
    let mut out = Vec::new();
    let body_ids: BTreeSet<&str> = sys.bodies.iter().map(|b| b.def.id.as_str()).collect();
    let mut seen_ids = BTreeSet::new();
    for le in &loaded.experiments {
        let d = &le.def;
        check_id(d.id.as_str(), &mut out);
        if d.schema_version != crate::experiment::EXPERIMENT_SCHEMA_VERSION {
            out.push(ValidationError::SchemaVersion {
                file: format!("experiments/{}", le.path.display()),
                found: d.schema_version,
                expected: crate::experiment::EXPERIMENT_SCHEMA_VERSION,
            });
        }
        let stem = le
            .path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        if stem != d.id.as_str() {
            out.push(ValidationError::BadExperiment {
                experiment: d.id.as_str().to_string(),
                reason: "experiment id does not match file stem",
            });
        }
        if !seen_ids.insert(d.id.clone()) {
            out.push(ValidationError::BadExperiment {
                experiment: d.id.as_str().to_string(),
                reason: "duplicate experiment id",
            });
        }
        if !(d.transmit_ratio.is_finite() && d.transmit_ratio >= 0.2 && d.transmit_ratio <= 1.0) {
            out.push(ValidationError::BadExperiment {
                experiment: d.id.as_str().to_string(),
                reason: "transmit_ratio outside the authored 0.2-1.0 band",
            });
        }
        // No caps at all means `cap_for` returns `None` in every
        // context — the experiment can never pay out anywhere (the
        // recipe-side "produces nothing" rule's shape).
        if d.caps.is_empty() {
            out.push(ValidationError::BadExperiment {
                experiment: d.id.as_str().to_string(),
                reason: "caps is empty — the experiment runs in no context",
            });
        }
        let mut covered: BTreeSet<(&str, crate::part::Situation)> = BTreeSet::new();
        for cap in &d.caps {
            check_id(cap.body.as_str(), &mut out);
            if !body_ids.contains(cap.body.as_str()) {
                out.push(ValidationError::BadExperiment {
                    experiment: d.id.as_str().to_string(),
                    reason: "cap names a body not in the system",
                });
            }
            {
                let experiment = d.id.as_str();
                check_situation_mask(cap.situations, "cap entry covers no situations", |reason| {
                    out.push(ValidationError::BadExperiment {
                        experiment: experiment.to_string(),
                        reason,
                    });
                });
            }
            if !(cap.cap_data_units.is_finite() && cap.cap_data_units > 0.0) {
                out.push(ValidationError::BadExperiment {
                    experiment: d.id.as_str().to_string(),
                    reason: "cap_data_units not positive and finite",
                });
            }
            for s in cap.situations.situations() {
                if !covered.insert((cap.body.as_str(), s)) {
                    out.push(ValidationError::BadExperiment {
                        experiment: d.id.as_str().to_string(),
                        reason: "overlapping (body, situation) cap entries",
                    });
                }
            }
        }
    }
    out
}

/// Loc resolution for the experiment defs (the
/// [`validate_contract_loc_keys`] pattern): every def's
/// `display_name` must resolve in `en.ron`. Separate from
/// [`validate_experiments`] because the loc file lives outside the
/// experiments directory.
pub fn validate_experiment_loc_keys(
    loaded: &crate::load::LoadedExperiments,
    loc: &LocFile,
) -> Vec<ValidationError> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for le in &loaded.experiments {
        let key = &le.def.display_name;
        if seen.insert(key.as_str().to_string()) && !loc.contains(key) {
            out.push(ValidationError::UnresolvedLocKey {
                context: format!("experiment {}", le.def.id),
                key: key.as_str().to_string(),
            });
        }
    }
    out
}

// ---------- tech tree (§3.3) ----------

/// §3.3's tree rules over the merged `data/techtree/` set, checked
/// against the loaded parts (the "every part reachable" half needs the
/// `tech_requirement` edges only `LoadedParts` carries):
///
/// - `schema_version` current on every fragment;
/// - node/track ids unique and well-formed;
/// - `science_cost` finite and ≥ 0; `requires` resolvable, deduplicated,
///   and cost-monotonic per edge (`node.cost ≥ prereq.cost` — §3.3's
///   "cost monotonic in depth", read non-decreasing so equal-cost tiers
///   stay legal);
/// - **no cycles** — a `requires` walk never revisits a node;
/// - every grant names a real track with a cap inside
///   `1..=track.levels.len()`, no track granted twice on one node;
/// - track shape: non-empty levels, `science_cost` finite ≥ 0,
///   `Multiplier` finite > 0, `Delta` finite, `len ≤ u8::MAX`
///   (grant/state counters are `u8`), `(family, stat_path)` unique;
/// - every track's family has ≥ 1 part and its `stat_path` resolves on
///   **every** part of the family — a frozen upgrade the resolver
///   cannot reach is a `ResolveError`, so authoring must guarantee
///   reachability ahead of manufacture — and a `Delta`-carrying track
///   needs a positive finite base everywhere it resolves (the
///   manufacture compose folds `(base + Σ deltas) / base`; a zero or
///   NaN base `InvalidStat`s at resolve);
/// - **every part reachable** — each `PartDef::tech_requirement` names
///   a node in the tree.
pub fn validate_techtree(
    loaded: &crate::load::LoadedTechTree,
    parts: &LoadedParts,
) -> Vec<ValidationError> {
    let mut out = Vec::new();
    // Pass 1: identity + shape — the id → def maps every later pass
    // checks references against (a fragment may reference nodes
    // authored in a *different* file).
    let (node_ids, track_ids) = tree_identity(loaded, &mut out);
    check_tree_nodes(&node_ids, &track_ids, &mut out);
    check_tree_cycles(&node_ids, &mut out);
    check_tree_tracks(&track_ids, parts, &mut out);
    // §3.3 "every part reachable" — each part's `tech_requirement`
    // must name a node in the merged tree.
    for p in &parts.parts {
        if !node_ids.contains_key(p.def.tech_requirement.as_str()) {
            out.push(ValidationError::UnknownTechRequirement {
                part: p.def.id.as_str().to_string(),
                tech: p.def.tech_requirement.as_str().to_string(),
            });
        }
    }
    out
}

/// Pass 1 — schema versions, id well-formedness, duplicate ids.
/// Returns the merged `id → def` maps the reference passes consume.
fn tree_identity<'a>(
    loaded: &'a crate::load::LoadedTechTree,
    out: &mut Vec<ValidationError>,
) -> (
    BTreeMap<&'a str, &'a TechNode>,
    BTreeMap<&'a str, &'a UpgradeTrack>,
) {
    let mut node_ids = BTreeMap::new();
    let mut track_ids = BTreeMap::new();
    for f in &loaded.trees {
        if f.def.schema_version != crate::techtree::TECHTREE_SCHEMA_VERSION {
            out.push(ValidationError::SchemaVersion {
                file: format!("techtree/{}", f.path.display()),
                found: f.def.schema_version,
                expected: crate::techtree::TECHTREE_SCHEMA_VERSION,
            });
        }
        for n in &f.def.nodes {
            let id = n.id.as_str();
            check_id(id, out);
            if let std::collections::btree_map::Entry::Vacant(v) = node_ids.entry(id) {
                v.insert(n);
            } else {
                out.push(ValidationError::DuplicateTechNodeId { id: id.to_string() });
            }
        }
        for t in &f.def.tracks {
            let id = t.id.as_str();
            check_id(id, out);
            if let std::collections::btree_map::Entry::Vacant(v) = track_ids.entry(id) {
                v.insert(t);
            } else {
                out.push(ValidationError::DuplicateUpgradeTrackId { id: id.to_string() });
            }
        }
    }
    (node_ids, track_ids)
}

/// Pass 2 — node shape: `science_cost` finite ≥ 0, `requires`
/// resolvable/deduplicated/cost-monotonic, grant refs and caps.
fn check_tree_nodes(
    node_ids: &BTreeMap<&str, &TechNode>,
    track_ids: &BTreeMap<&str, &UpgradeTrack>,
    out: &mut Vec<ValidationError>,
) {
    for (&id, n) in node_ids {
        if !(n.science_cost.is_finite() && n.science_cost >= 0.0) {
            out.push(ValidationError::BadTechNode {
                node: id.to_string(),
                reason: "science_cost is not finite and >= 0",
            });
        }
        let mut prereqs = BTreeSet::new();
        for r in &n.requires {
            if !prereqs.insert(r.as_str()) {
                out.push(ValidationError::BadTechNode {
                    node: id.to_string(),
                    reason: "duplicate prerequisite",
                });
            }
            match node_ids.get(r.as_str()) {
                None => out.push(ValidationError::UnknownTechPrerequisite {
                    node: id.to_string(),
                    prerequisite: r.as_str().to_string(),
                }),
                Some(p) => {
                    if n.science_cost.is_finite()
                        && p.science_cost.is_finite()
                        && n.science_cost < p.science_cost
                    {
                        out.push(ValidationError::TechCostNotMonotonic {
                            node: id.to_string(),
                            prerequisite: r.as_str().to_string(),
                        });
                    }
                }
            }
        }
        let mut granted = BTreeSet::new();
        for (track, cap) in &n.unlocks.track_levels {
            if !granted.insert(track.as_str()) {
                out.push(ValidationError::BadTechNode {
                    node: id.to_string(),
                    reason: "track granted twice on one node",
                });
            }
            match track_ids.get(track.as_str()) {
                None => out.push(ValidationError::UnknownUpgradeTrack {
                    node: id.to_string(),
                    track: track.as_str().to_string(),
                }),
                Some(t) => {
                    if *cap == 0 {
                        out.push(ValidationError::BadTechNode {
                            node: id.to_string(),
                            reason: "track_levels grant of 0 unlocks nothing",
                        });
                    } else if usize::from(*cap) > t.levels.len() {
                        out.push(ValidationError::BadTechNode {
                            node: id.to_string(),
                            reason: "track_levels grant exceeds the track's authored levels",
                        });
                    }
                }
            }
        }
        for f_id in &n.unlocks.facilities {
            check_id(f_id.as_str(), out);
        }
        for c_id in &n.unlocks.contract_types {
            check_id(c_id.as_str(), out);
        }
    }
}

/// Pass 3 — cycles: iterative DFS over `requires` edges. `done`
/// marks fully-explored nodes, `visiting` the current DFS path;
/// a `requires` edge landing on a `visiting` node is a cycle.
/// Missing refs were already reported; the walk skips them (a
/// dangling edge cannot close a loop).
fn check_tree_cycles(node_ids: &BTreeMap<&str, &TechNode>, out: &mut Vec<ValidationError>) {
    let mut done: BTreeSet<&str> = BTreeSet::new();
    for &id in node_ids.keys() {
        // `stack` is the current DFS path; `visiting` its membership.
        let mut stack: Vec<&str> = vec![id];
        let mut visiting: BTreeSet<&str> = [id].into_iter().collect();
        while let Some(&cur) = stack.last() {
            if done.contains(cur) {
                stack.pop();
                visiting.remove(cur);
                continue;
            }
            let mut advanced = false;
            for r in &node_ids[cur].requires {
                let rid = r.as_str();
                if visiting.contains(rid) {
                    out.push(ValidationError::TechCycle {
                        node: rid.to_string(),
                    });
                    continue;
                }
                if node_ids.contains_key(rid) && !done.contains(rid) {
                    stack.push(rid);
                    visiting.insert(rid);
                    advanced = true;
                    break;
                }
            }
            if !advanced {
                done.insert(cur);
                stack.pop();
                visiting.remove(cur);
            }
        }
    }
}

/// Pass 4 — track shape + family coverage: `(family, stat_path)`
/// unique, non-empty sane levels, ≥ 1 part in the family, and the
/// `stat_path` resolving on *every* family part.
fn check_tree_tracks(
    track_ids: &BTreeMap<&str, &UpgradeTrack>,
    parts: &LoadedParts,
    out: &mut Vec<ValidationError>,
) {
    let mut family_stat: BTreeSet<(&str, crate::part::StatPath)> = BTreeSet::new();
    for (&id, t) in track_ids {
        if !family_stat.insert((t.family.as_str(), t.stat_path)) {
            out.push(ValidationError::BadUpgradeTrack {
                track: id.to_string(),
                reason: "duplicate (family, stat_path) across tracks",
            });
        }
        check_id(t.family.as_str(), out);
        if t.levels.is_empty() {
            out.push(ValidationError::BadUpgradeTrack {
                track: id.to_string(),
                reason: "no levels authored",
            });
        }
        if t.levels.len() > u8::MAX as usize {
            out.push(ValidationError::BadUpgradeTrack {
                track: id.to_string(),
                reason: "more levels than the u8 level counter can hold",
            });
        }
        let mut has_delta = false;
        for l in &t.levels {
            if !(l.science_cost.is_finite() && l.science_cost >= 0.0) {
                out.push(ValidationError::BadUpgradeTrack {
                    track: id.to_string(),
                    reason: "level science_cost is not finite and >= 0",
                });
            }
            match l.multiplier_or_delta {
                crate::techtree::UpgradeEffect::Multiplier(m) => {
                    if !(m.is_finite() && m > 0.0) {
                        out.push(ValidationError::BadUpgradeTrack {
                            track: id.to_string(),
                            reason: "Multiplier effect is not finite and > 0",
                        });
                    }
                }
                crate::techtree::UpgradeEffect::Delta(d) => {
                    has_delta = true;
                    if !d.is_finite() {
                        out.push(ValidationError::BadUpgradeTrack {
                            track: id.to_string(),
                            reason: "Delta effect is not finite",
                        });
                    }
                }
            }
        }
        let mut in_family = 0usize;
        for p in &parts.parts {
            if p.def.family == t.family {
                in_family += 1;
                match t.stat_path.base_value(&p.def) {
                    None => {
                        out.push(ValidationError::BadUpgradeTrack {
                            track: id.to_string(),
                            reason: "stat_path does not resolve on a part of the family",
                        });
                    }
                    // A `Delta` level composes `(base + Σ deltas) /
                    // base` at manufacture — the same gate
                    // `csp_phys::resolve::compose_upgrade_levels`
                    // runs: the authored base must be positive and
                    // finite or every `resolve_built_part` on the
                    // family `InvalidStat`s. `Some(0.0)` (a
                    // zero-torque `Control`, say) used to pass here.
                    Some(base) if has_delta && !(base.is_finite() && base > 0.0) => {
                        out.push(ValidationError::BadUpgradeTrack {
                            track: id.to_string(),
                            reason: "Delta track's stat_path base is not positive and finite on a family part",
                        });
                    }
                    _ => {}
                }
            }
        }
        if in_family == 0 {
            out.push(ValidationError::BadUpgradeTrack {
                track: id.to_string(),
                reason: "family has no parts",
            });
        }
    }
}

/// Loc resolution for the tech tree (the
/// [`validate_part_loc_keys`] pattern): every node/track display name
/// and node description must resolve in `en.ron`.
pub fn validate_techtree_loc_keys(
    loaded: &crate::load::LoadedTechTree,
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
    for f in &loaded.trees {
        for n in &f.def.nodes {
            check(format!("techtree node {}", n.id), &n.display_name);
            check(format!("techtree node {}", n.id), &n.description);
        }
        for t in &f.def.tracks {
            check(format!("techtree track {}", t.id), &t.display_name);
        }
    }
    out
}

// ---------- contracts (§3.6) ----------

/// One template's `generate.space` vs the system body + experiment
/// id sets — extracted from [`validate_contracts`] for the line
/// budget. `bad` stamps the template's error variant.
#[allow(clippy::too_many_lines)] // one arm per kind reads as a table
#[allow(clippy::too_many_arguments)] // the cross-reference sets are the signature
fn validate_contract_space(
    cid: &str,
    kind: &crate::contract::ContractKind,
    space: &crate::contract::ContractParamSpace,
    body_ids: &BTreeSet<&str>,
    site_bodies: &BTreeSet<&str>,
    experiment_ids: &BTreeSet<&str>,
    res: &ResourceFile,
    out: &mut Vec<ValidationError>,
) {
    use crate::contract::{ContractParamSpace, ModuleReq, RegimeSpace};
    let bad = |reason: &'static str| ValidationError::BadContract {
        contract: cid.to_string(),
        reason,
    };
    // §3.6's "candidate lists non-empty" rule: an empty `bodies`
    // space pushes an error here — the first offer draw indexes it
    // unconditionally (`bodies.get(pick(rng, bodies.len()))` sim-side)
    // and the generation gate would degrade `Recoverable` on every
    // template pick. The membership half still reports each body that
    // misses the system table.
    let check_bodies = |bodies: &[crate::ids::BodyId], out: &mut Vec<ValidationError>| {
        if bodies.is_empty() {
            out.push(bad("candidate body list is empty"));
        }
        for b in bodies {
            if !body_ids.contains(b.as_str()) {
                out.push(bad("candidate body is not in the system"));
            }
        }
    };
    let range_ok = |r: (f64, f64), what: &'static str, out: &mut Vec<ValidationError>| -> bool {
        if r.0.is_finite() && r.1.is_finite() && r.0 > 0.0 && r.0 <= r.1 {
            true
        } else {
            out.push(bad(what));
            false
        }
    };
    match (kind, space) {
        (
            crate::contract::ContractKind::SatellitePlacement,
            ContractParamSpace::SatellitePlacement {
                bodies,
                periapsis_alt_m,
                apoapsis_alt_m,
            },
        ) => {
            check_bodies(bodies, out);
            let peri_ok = range_ok(
                *periapsis_alt_m,
                "periapsis_alt_m range is not 0 < lo <= hi",
                out,
            );
            let apo_ok = range_ok(
                *apoapsis_alt_m,
                "apoapsis_alt_m range is not 0 < lo <= hi",
                out,
            );
            // Cross-band sanity: partial overlap is legal (the sim
            // orders a drawn pair — see `ContractParamSpace`'s doc),
            // but when the apoapsis band's *ceiling* sits under the
            // periapsis band's *floor* every possible draw inverts —
            // the published window always comes out of the swap, so
            // the authored bands carry none of their stated meaning.
            // Gated on both bands passing `range_ok` so a malformed
            // band reports exactly its own error.
            if peri_ok && apo_ok && apoapsis_alt_m.1 < periapsis_alt_m.0 {
                out.push(bad(
                    "apoapsis_alt_m.hi below periapsis_alt_m.lo — every draw inverts",
                ));
            }
        }
        (
            crate::contract::ContractKind::SurveyRun,
            ContractParamSpace::SurveyRun {
                experiments: exps,
                bodies,
                min_units,
            },
        ) => {
            if exps.is_empty() {
                out.push(bad("survey candidates: no experiments authored"));
            }
            for e in exps {
                if !experiment_ids.contains(e.as_str()) {
                    out.push(bad("survey candidate experiment is not loaded"));
                }
            }
            check_bodies(bodies, out);
            if !(min_units.is_finite() && *min_units > 0.0) {
                out.push(bad("min_units is not positive and finite"));
            }
        }
        (
            crate::contract::ContractKind::PartTest,
            ContractParamSpace::PartTest { modules, regime },
        ) => {
            if modules.is_empty() {
                out.push(bad("part test: no module requirements authored"));
            }
            for m in modules {
                if let ModuleReq::Experiment(e) = m
                    && !experiment_ids.contains(e.as_str())
                {
                    out.push(bad("module requirement names an unloaded experiment"));
                }
            }
            match regime {
                RegimeSpace::Orbit { bodies, min_alt_m } => {
                    check_bodies(bodies, out);
                    range_ok(
                        *min_alt_m,
                        "part-test orbit min_alt range is not 0 < lo <= hi",
                        out,
                    );
                }
                RegimeSpace::Landed { bodies } => {
                    check_bodies(bodies, out);
                }
            }
        }
        (crate::contract::ContractKind::Rescue, ContractParamSpace::Rescue { bodies }) => {
            check_bodies(bodies, out);
        }
        (
            crate::contract::ContractKind::ColonyResupply,
            ContractParamSpace::ColonyResupply {
                bodies,
                resources,
                amount,
            },
        ) => {
            check_bodies(bodies, out);
            // C32-F1: every resupply candidate body must host ≥1
            // site — colonies only ever exist at authored sites
            // (`FoundColony` rejects unknown site ids and no
            // site-creation verb exists), so a siteless body can
            // never grow the store a delivery lands in. The sim's
            // generation gate filters live-capacity per draw, but
            // that is the runtime net, not a license to author a
            // siteless candidate (duva was exactly this miss).
            for b in bodies {
                if body_ids.contains(b.as_str()) && !site_bodies.contains(b.as_str()) {
                    out.push(bad("resupply candidate body has no authored site"));
                }
            }
            if resources.is_empty() {
                out.push(bad("resupply candidates: no resources authored"));
            }
            for r in resources {
                if !res.contains(r) {
                    out.push(bad("resupply candidate resource is not in resources.ron"));
                }
            }
            range_ok(*amount, "resupply amount range is not 0 < lo <= hi", out);
        }
        _ => out.push(bad("kind and generate.space variants disagree")),
    }
}

/// §3.6's template rules over the `data/contracts/` set, checked
/// against the system bodies, the experiment defs, the resource
/// registry, the site table, and the merged tech tree (the
/// cross-reference set the prereqs/candidates name):
///
/// - `schema_version` current, `id` ↔ file-stem agreement, unique
///   ids, well-formed id;
/// - `kind` ↔ `generate.space` variant agreement;
/// - `weight` finite > 0; rewards finite ≥ 0; `deadline_days`
///   `None` or finite > 0;
/// - candidate lists non-empty, ids resolvable (bodies in the
///   system, experiments in the loaded set, `ColonyResupply`
///   resources in `resources.ron`), draw ranges finite
///   with `0 ≤ lo ≤ hi` (altitude `lo > 0` — a zero-altitude orbit
///   band intersects the surface), and — for `SatellitePlacement` —
///   the bands must not be fully inverted (`apoapsis_alt_m.hi <
///   periapsis_alt_m.lo` would invert every draw; partial overlap
///   stays legal — the sim orders the pair);
/// - `ColonyResupply` candidate bodies each host ≥1 site — colonies
///   exist only at authored sites, so a siteless body can never
///   grow the store a delivery needs (C32-F1);
/// - prerequisites well-formed: no duplicates, `count > 0`, tech
///   ids resolvable in the merged tree, body ids in the system, and
///   `ContractsCompleted { kind }` naming an authored template kind
///   (an unauthored kind is a silently unsatisfiable gate —
///   C32-F6);
/// - the tree's `unlocks.contract_types` ids each name an authored
///   template — the other direction is free (a template may exist
///   ununlocked).
#[allow(clippy::too_many_lines)] // the per-field check list reads as a list
pub fn validate_contracts(
    loaded: &crate::load::LoadedContracts,
    sys: &LoadedSystem,
    experiments: &crate::load::LoadedExperiments,
    techtree: &crate::load::LoadedTechTree,
    res: &ResourceFile,
    sites: &SiteFile,
) -> Vec<ValidationError> {
    use crate::contract::ContractPrereq;
    let mut out = Vec::new();
    let body_ids: BTreeSet<&str> = sys.bodies.iter().map(|b| b.def.id.as_str()).collect();
    // The bodies any colony can ever sit on: `FoundColony` resolves
    // authored sites only, so the resupply candidate check reads
    // this set (C32-F1).
    let site_bodies: BTreeSet<&str> = sites.sites.iter().map(|s| s.body.as_str()).collect();
    let experiment_ids: BTreeSet<&str> = experiments
        .experiments
        .iter()
        .map(|e| e.def.id.as_str())
        .collect();
    let tree = techtree.merged();
    let node_ids: BTreeSet<&str> = tree.nodes.iter().map(|n| n.id.as_str()).collect();
    // The kind set `ContractsCompleted` prereqs may name — an
    // unauthored kind is a prereq no run can satisfy (C32-F6).
    let authored_kinds: BTreeSet<crate::contract::ContractKind> =
        loaded.contracts.iter().map(|c| c.def.kind).collect();
    let mut seen_ids = BTreeSet::new();
    let mut template_ids: BTreeSet<&str> = BTreeSet::new();
    for lc in &loaded.contracts {
        let d = &lc.def;
        let cid = d.id.as_str();
        template_ids.insert(cid);
        if d.schema_version != crate::contract::CONTRACT_SCHEMA_VERSION {
            out.push(ValidationError::SchemaVersion {
                file: format!("contracts/{}", lc.path.display()),
                found: d.schema_version,
                expected: crate::contract::CONTRACT_SCHEMA_VERSION,
            });
        }
        if lc
            .path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
            != cid
        {
            out.push(ValidationError::BadContract {
                contract: cid.to_string(),
                reason: "contract id does not match file stem",
            });
        }
        if !seen_ids.insert(d.id.clone()) {
            out.push(ValidationError::BadContract {
                contract: cid.to_string(),
                reason: "duplicate contract id",
            });
        }
        check_id(cid, &mut out);
        let bad = |reason: &'static str| ValidationError::BadContract {
            contract: cid.to_string(),
            reason,
        };
        if !(d.generate.weight.is_finite() && d.generate.weight > 0.0) {
            out.push(bad("generator weight is not finite and > 0"));
        }
        for (name, v) in [
            ("advance_funds", d.rewards.advance_funds),
            ("completion_funds", d.rewards.completion_funds),
            ("failure_penalty_funds", d.rewards.failure_penalty_funds),
        ] {
            if !(v.is_finite() && v >= 0.0) {
                out.push(bad(match name {
                    "advance_funds" => "advance_funds is not finite and >= 0",
                    "completion_funds" => "completion_funds is not finite and >= 0",
                    _ => "failure_penalty_funds is not finite and >= 0",
                }));
            }
        }
        if let Some(days) = d.deadline_days
            && !(days.is_finite() && days > 0.0)
        {
            out.push(bad("deadline_days is not positive and finite"));
        }
        validate_contract_space(
            cid,
            &d.kind,
            &d.generate.space,
            &body_ids,
            &site_bodies,
            &experiment_ids,
            res,
            &mut out,
        );
        let mut prereqs = BTreeSet::new();
        for p in &d.prerequisites {
            if !prereqs.insert(p.clone()) {
                out.push(bad("duplicate prerequisite"));
            }
            match p {
                ContractPrereq::TechResearched(n) => {
                    if !node_ids.contains(n.as_str()) {
                        out.push(bad("prerequisite tech node is not in the tree"));
                    }
                }
                ContractPrereq::ContractsCompleted { kind, count } => {
                    if *count == 0 {
                        out.push(bad("ContractsCompleted count of 0 is a tautology"));
                    }
                    // C32-F6: the kind must name an authored template
                    // kind — an unauthored one is a prereq no run can
                    // ever satisfy, which silently gates the whole
                    // template off the board.
                    if !authored_kinds.contains(kind) {
                        out.push(bad("ContractsCompleted names an unauthored contract kind"));
                    }
                }
                ContractPrereq::BodyVisited(b) => {
                    if !body_ids.contains(b.as_str()) {
                        out.push(bad("BodyVisited names a body not in the system"));
                    }
                }
                ContractPrereq::CrewLostAtMost(_) => {}
            }
        }
    }
    // The tree's `contract_types` unlocks must resolve — an unlock
    // naming a template nobody authored is dead authored surface.
    // C08-F6's drift guard, second half: generation now gates on the
    // unlock AND the template's own `prerequisites` — a template
    // unlocked by node N whose prereqs do not `TechResearched(N)`
    // makes the two authored gates disagree (offered-or-not vs
    // offered-and-eligible), so the validator pins them together.
    for n in &tree.nodes {
        for c in &n.unlocks.contract_types {
            if !template_ids.contains(c.as_str()) {
                out.push(ValidationError::UnknownContractType {
                    node: n.id.as_str().to_string(),
                    contract_type: c.as_str().to_string(),
                });
                continue;
            }
            let template = loaded
                .contracts
                .iter()
                .map(|lc| &lc.def)
                .find(|d| d.id == *c);
            if let Some(d) = template
                && !d
                    .prerequisites
                    .iter()
                    .any(|p| matches!(p, ContractPrereq::TechResearched(id) if id == &n.id))
            {
                out.push(ValidationError::BadContract {
                    contract: c.as_str().to_string(),
                    reason: "unlocked by a tech node its prerequisites do not require \
                         (add TechResearched of the unlocking node)",
                });
            }
        }
    }
    out
}

/// Loc resolution for contract templates (the
/// [`validate_techtree_loc_keys`] pattern): display name +
/// description must resolve in `en.ron`.
pub fn validate_contract_loc_keys(
    loaded: &crate::load::LoadedContracts,
    loc: &LocFile,
) -> Vec<ValidationError> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for lc in &loaded.contracts {
        for key in [&lc.def.display_name, &lc.def.description] {
            if seen.insert(key.as_str().to_string()) && !loc.contains(key) {
                out.push(ValidationError::UnresolvedLocKey {
                    context: format!("contract {}", lc.def.id),
                    key: key.as_str().to_string(),
                });
            }
        }
    }
    out
}

/// The whole `data/` tree as the loaders hand it back — one field per
/// file kind, the [`validate_dataset`] input. Borrowed, never owned:
/// the caller loads once and hands the sweep views.
#[derive(Clone, Copy, Debug)]
pub struct Dataset<'a> {
    /// `data/systems/home/` (manifest + bodies + biomes).
    pub system: &'a crate::load::LoadedSystem,
    /// `data/balance.ron`.
    pub balance: &'a crate::balance::BalanceDef,
    /// `data/resources.ron`.
    pub resources: &'a ResourceFile,
    /// `data/systems/home/sites.ron`.
    pub sites: &'a SiteFile,
    /// `data/systems/home/colonies.ron`.
    pub colonies: &'a crate::colony::ColonyFile,
    /// `data/recipes/`.
    pub recipes: &'a crate::load::LoadedRecipes,
    /// `data/experiments/`.
    pub experiments: &'a crate::load::LoadedExperiments,
    /// `data/techtree/`.
    pub techtree: &'a crate::load::LoadedTechTree,
    /// `data/contracts/`.
    pub contracts: &'a crate::load::LoadedContracts,
    /// `data/parts/`.
    pub parts: &'a LoadedParts,
    /// `data/loc/en.ron`.
    pub loc: &'a LocFile,
}

/// The §8.2 semantic gate over a fully loaded `data/` tree — the same
/// sweep the data tests run, callable from production startup so a
/// bad tree is refused loudly instead of shipping silently. Returns
/// every failure across every file kind, never just the first.
pub fn validate_dataset(d: &Dataset<'_>) -> Vec<ValidationError> {
    let mut out = Vec::new();
    out.extend(validate_system(d.system));
    out.extend(validate_resources(d.resources));
    out.extend(validate_parts(
        d.parts,
        d.resources,
        d.recipes,
        d.experiments,
    ));
    out.extend(validate_sites(d.sites, d.system, d.resources));
    out.extend(validate_experiments(d.experiments, d.system));
    out.extend(validate_techtree(d.techtree, d.parts));
    out.extend(validate_contracts(
        d.contracts,
        d.system,
        d.experiments,
        d.techtree,
        d.resources,
        d.sites,
    ));
    out.extend(validate_recipes(d.recipes, d.resources));
    out.extend(validate_balance(d.balance, d.resources));
    out.extend(validate_colonies(
        d.colonies,
        d.sites,
        d.resources,
        d.recipes,
    ));
    out.extend(validate_loc(d.loc));
    out.extend(validate_loc_keys(d.system, d.loc));
    out.extend(validate_site_loc_keys(d.sites, d.loc));
    out.extend(validate_part_loc_keys(d.parts, d.resources, d.loc));
    out.extend(validate_experiment_loc_keys(d.experiments, d.loc));
    out.extend(validate_techtree_loc_keys(d.techtree, d.loc));
    out.extend(validate_contract_loc_keys(d.contracts, d.loc));
    out.extend(validate_recipe_loc_keys(d.recipes, d.loc));
    out.extend(validate_colony_loc_keys(d.colonies, d.loc));
    check_surface_resources(d.system, d.resources, &mut out);
    out
}

/// `surface_resources` `ResourceId` keys must exist in `resources.ron`.
/// Lives here rather than in [`validate_system`] for the same reason
/// the loc gates live outside it: the resource file is not part of the
/// system directory, so `validate_system`'s signature cannot express
/// the check (the per-map `png` name refs are checked there — the
/// keys were the skipped half).
fn check_surface_resources(
    sys: &crate::load::LoadedSystem,
    res: &ResourceFile,
    out: &mut Vec<ValidationError>,
) {
    for b in &sys.bodies {
        for (r, _) in &b.def.surface_resources {
            if !res.contains(r) {
                out.push(ValidationError::UnknownSurfaceResource {
                    body: b.def.id.as_str().to_string(),
                    resource: r.as_str().to_string(),
                });
            }
        }
    }
}

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
    fn periapsis_under_parent_surface_is_rejected() {
        let mut sys = fixture();
        // e ≈ 0.9999 on a = 1e9 puts periapsis at ~1e5 m — under the
        // star's 1e6 m radius (the period check keys on `a`, unchanged).
        planet(&mut sys).orbit.as_mut().unwrap().eccentricity = 0.9999;
        let errs = validate_system(&sys);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::PeriapsisInsideParent { .. })),
            "{errs:?}"
        );
    }

    #[test]
    fn non_star_soi_missing_is_rejected() {
        let mut sys = fixture();
        // `soi_m: None` on the planet reads as an infinite SOI — the
        // child-geometry rule then has nothing finite to check.
        planet(&mut sys).soi_m = None;
        let errs = validate_system(&sys);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::SoiMissing { .. })),
            "{errs:?}"
        );
    }

    #[test]
    fn own_soi_reaching_own_periapsis_is_rejected() {
        let mut sys = fixture();
        // 2e9 m SOI on a 1e9 m periapsis orbit — the star is inside the
        // planet's SOI at closest approach.
        planet(&mut sys).soi_m = Some(2.0e9);
        let errs = validate_system(&sys);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::SoiExceedsOrbit { .. })),
            "{errs:?}"
        );
    }

    #[test]
    fn overlapping_sibling_sois_are_rejected_once() {
        let mut sys = fixture();
        // A second child of the star on a disjoint outer shell:
        // [1e9] vs [1.5e9] — a 5e8 m gap. 1e8 + 5e8 SOIs exceed it.
        let mut outer = sys.bodies[1].clone();
        outer.def.id = "outer".into();
        outer.def.soi_m = Some(5.0e8);
        let a = 1.5e9;
        let mu_star = sys.bodies[0].def.mu_m3_per_s2;
        let o = outer.def.orbit.as_mut().unwrap();
        o.semi_major_axis_m = a;
        o.period_s = 2.0 * core::f64::consts::PI * libm::sqrt(a * a * a / mu_star);
        sys.bodies.push(outer);
        sys.system.bodies.push("outer".into());
        let errs = validate_system(&sys);
        let hits = errs
            .iter()
            .filter(|e| matches!(e, ValidationError::SiblingSoiOverlap { .. }))
            .count();
        assert_eq!(hits, 1, "the unordered pair reports exactly once: {errs:?}");
    }

    #[test]
    fn disjoint_shell_sois_inside_the_gap_pass() {
        let mut sys = fixture();
        // Same outer sibling but with a small SOI: 1e8 + 1e8 < 5e8 gap.
        let mut outer = sys.bodies[1].clone();
        outer.def.id = "outer".into();
        let a = 1.5e9;
        let mu_star = sys.bodies[0].def.mu_m3_per_s2;
        let o = outer.def.orbit.as_mut().unwrap();
        o.semi_major_axis_m = a;
        o.period_s = 2.0 * core::f64::consts::PI * libm::sqrt(a * a * a / mu_star);
        sys.bodies.push(outer);
        sys.system.bodies.push("outer".into());
        let errs = validate_system(&sys);
        assert!(
            !errs
                .iter()
                .any(|e| matches!(e, ValidationError::SiblingSoiOverlap { .. })),
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

    /// A minimal valid palette-PNG byte stream for validator tests —
    /// CRCs are still unchecked, but the `IDAT` is a real zlib stream
    /// (the validator now decodes pixels, so a header-only PNG would
    /// fail `BiomeMapPngInvalid` before the rule under test). `pixels`
    /// is `w*h` palette indices, row-major.
    fn tiny_png(w: u32, h: u32, palette_entries: usize, pixels: &[u8]) -> Vec<u8> {
        assert_eq!(pixels.len(), (w * h) as usize);
        let mut raw = Vec::with_capacity((w as usize + 1) * h as usize);
        for row in pixels.chunks_exact(w as usize) {
            raw.push(0); // scanline filter: None
            raw.extend_from_slice(row);
        }
        let idat = miniz_oxide::deflate::compress_to_vec_zlib(&raw, 6);
        let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&w.to_be_bytes());
        ihdr.extend_from_slice(&h.to_be_bytes());
        ihdr.extend_from_slice(&[8, 3, 0, 0, 0]);
        for (kind, data) in [
            (*b"IHDR", ihdr),
            (*b"PLTE", vec![0u8; palette_entries * 3]),
            (*b"IDAT", idat),
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
        with_biomes(&mut sys, 5, tiny_png(4, 4, 2, &[0; 16])); // palette of 2, index 5
        let errs = validate_system(&sys);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::BiomeIndexExceedsPalette { .. })),
            "{errs:?}"
        );
    }

    /// The validator decodes pixels: a map painting a palette index
    /// the legend never names is an unnamed biome region.
    #[test]
    fn unlegended_pixel_index_is_rejected() {
        let mut sys = fixture();
        // Legend covers indices 0 and 1; the map paints a 2 (palette
        // of 4, so `BiomeIndexExceedsPalette` does not fire — this is
        // the legend-coverage rule specifically).
        let mut pixels = [0u8; 16];
        pixels[5] = 2;
        with_biomes(&mut sys, 1, tiny_png(4, 4, 4, &pixels));
        let errs = validate_system(&sys);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::BiomeIndexNotInLegend { index: 2, .. })),
            "{errs:?}"
        );
        // And the clean map passes the coverage check entirely.
        let mut sys = fixture();
        with_biomes(&mut sys, 1, tiny_png(4, 4, 4, &[1; 16]));
        let errs = validate_system(&sys);
        assert!(
            !errs
                .iter()
                .any(|e| matches!(e, ValidationError::BiomeIndexNotInLegend { .. })),
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
        with_biomes(&mut sys, 1, tiny_png(4096, 32, 2, &vec![0; 4096 * 32]));
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
        with_biomes(&mut sys, 0, tiny_png(4, 4, 2, &[0; 16])); // two entries at index 0
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

    /// Empty registries for the `Converter`/`Experiment` cross-ref
    /// checks — `part_fixture` carries an `Engine` module only, so the
    /// empty set is enough for every negative test below; the
    /// cross-ref tests install their own.
    fn empty_registries() -> (crate::load::LoadedRecipes, crate::load::LoadedExperiments) {
        (
            crate::load::LoadedRecipes {
                recipes: Vec::new(),
            },
            crate::load::LoadedExperiments {
                experiments: Vec::new(),
            },
        )
    }

    fn part_errors(def: PartDef) -> Vec<ValidationError> {
        let (recipes, experiments) = empty_registries();
        validate_parts(&loaded_part(def), &res_fixture(), &recipes, &experiments)
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
        let (recipes, experiments) = empty_registries();
        let errs = validate_parts(&loaded, &res_fixture(), &recipes, &experiments);
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
        let (recipes, experiments) = empty_registries();
        let errs = validate_parts(&loaded, &res_fixture(), &recipes, &experiments);
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
        // A stat path the def does not carry — `resolve_built_part`
        // would fail it at manufacture (`UnresolvableStatPath`).
        let mut d = part_fixture();
        d.variants[0].stat_multipliers = vec![(StatPath::ControlTorqueNm, 1.5)];
        let errs = part_errors(d);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::UnresolvableVariantStat { .. })),
            "{errs:?}"
        );
    }

    #[test]
    fn propellant_fraction_overflow_is_rejected() {
        // Two finite positive fractions whose sum overflows to +inf:
        // the resolver's gate rejects it, so the validator must too —
        // the pre-fix `sum.is_finite() && …` form passed it green.
        let mut d = part_fixture();
        let ModuleDef::Engine { propellants, .. } = &mut d.modules[0] else {
            unreachable!()
        };
        *propellants = vec![
            ("liquid_fuel".into(), 1.0e308),
            ("oxidizer".into(), 1.0e308),
        ];
        let errs = part_errors(d);
        assert!(
            errs.iter().any(|e| matches!(
                e,
                ValidationError::BadModule { reason, .. }
                    if *reason == "propellant mass fractions do not sum to 1"
            )),
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

    // ---------- tech tree (§3.3) ----------

    use crate::load::{LoadedTechFile, LoadedTechTree};
    use crate::techtree::{
        TECHTREE_SCHEMA_VERSION, TechNode, TechTreeDef, TechUnlocks, UpgradeEffect, UpgradeLevel,
        UpgradeTrack,
    };

    /// A minimal valid two-node tree + track over `part_fixture`'s
    /// family — the part's `tech_requirement` is `tech.start`, so the
    /// fixture validates green as written.
    fn tree_fixture() -> LoadedTechTree {
        LoadedTechTree {
            trees: vec![LoadedTechFile {
                path: std::path::PathBuf::from("home.ron"),
                def: TechTreeDef {
                    schema_version: TECHTREE_SCHEMA_VERSION,
                    nodes: vec![
                        TechNode {
                            id: "tech.start".into(),
                            display_name: "tech.start.name".into(),
                            description: "tech.start.desc".into(),
                            science_cost: 0.0,
                            requires: vec![],
                            unlocks: TechUnlocks::default(),
                        },
                        TechNode {
                            id: "tech.rocketry.2".into(),
                            display_name: "tech.r2.name".into(),
                            description: "tech.r2.desc".into(),
                            science_cost: 30.0,
                            requires: vec!["tech.start".into()],
                            unlocks: TechUnlocks {
                                track_levels: vec![("track.isp".into(), 2)],
                                facilities: vec!["facility.lab".into()],
                                contract_types: vec![],
                            },
                        },
                    ],
                    tracks: vec![UpgradeTrack {
                        id: "track.isp".into(),
                        display_name: "track.isp.name".into(),
                        family: part_fixture().family,
                        stat_path: crate::part::StatPath::IspVacS,
                        levels: vec![
                            UpgradeLevel {
                                science_cost: 10.0,
                                multiplier_or_delta: UpgradeEffect::Multiplier(1.03),
                            },
                            UpgradeLevel {
                                science_cost: 12.0,
                                multiplier_or_delta: UpgradeEffect::Multiplier(1.03),
                            },
                        ],
                    }],
                },
            }],
        }
    }

    fn tree_errors(tree: &LoadedTechTree, parts: &LoadedParts) -> Vec<ValidationError> {
        validate_techtree(tree, parts)
    }

    /// The fixture itself must be clean.
    #[test]
    fn tree_fixture_is_valid() {
        let errs = tree_errors(&tree_fixture(), &loaded_part(part_fixture()));
        assert!(errs.is_empty(), "{errs:?}");
    }

    /// §3.3 "no cycles" — including a self-loop and a two-node loop.
    #[test]
    fn tree_cycle_is_rejected() {
        let mut t = tree_fixture();
        t.trees[0].def.nodes[1].requires = vec!["tech.rocketry.2".into()];
        let errs = tree_errors(&t, &loaded_part(part_fixture()));
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::TechCycle { .. })),
            "{errs:?}"
        );

        let mut t = tree_fixture();
        t.trees[0].def.nodes[1].requires = vec!["tech.rocketry.3".into()];
        t.trees[0].def.nodes.push(TechNode {
            id: "tech.rocketry.3".into(),
            display_name: "k".into(),
            description: "k".into(),
            science_cost: 60.0,
            requires: vec!["tech.rocketry.2".into()],
            unlocks: TechUnlocks::default(),
        });
        let errs = tree_errors(&t, &loaded_part(part_fixture()));
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::TechCycle { .. })),
            "{errs:?}"
        );
    }

    /// §3.3 "every part reachable" — a part gated on a missing node.
    #[test]
    fn unreachable_part_is_rejected() {
        let mut d = part_fixture();
        d.tech_requirement = "tech.nonexistent".into();
        let errs = tree_errors(&tree_fixture(), &loaded_part(d));
        assert!(
            errs.iter().any(|e| matches!(
                e,
                ValidationError::UnknownTechRequirement { tech, .. } if tech == "tech.nonexistent"
            )),
            "{errs:?}"
        );
    }

    /// §3.3 "cost monotonic in depth" — a node cheaper than its
    /// prerequisite is rejected; equal cost is legal.
    #[test]
    fn non_monotonic_cost_is_rejected() {
        let mut t = tree_fixture();
        t.trees[0].def.nodes[1].science_cost = -5.0;
        let errs = tree_errors(&t, &loaded_part(part_fixture()));
        assert!(
            errs.iter().any(|e| matches!(
                e,
                ValidationError::TechCostNotMonotonic { .. } | ValidationError::BadTechNode { .. }
            )),
            "{errs:?}"
        );
        // Equal cost along the edge is monotonic (non-decreasing).
        let mut t = tree_fixture();
        t.trees[0].def.nodes[0].science_cost = 30.0;
        assert!(tree_errors(&t, &loaded_part(part_fixture())).is_empty());
    }

    /// Dangling references: unknown prerequisite, unknown track grant,
    /// grant cap outside the authored levels.
    #[test]
    fn bad_tree_refs_are_rejected() {
        let mut t = tree_fixture();
        t.trees[0].def.nodes[1].requires = vec!["tech.missing".into()];
        let errs = tree_errors(&t, &loaded_part(part_fixture()));
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::UnknownTechPrerequisite { .. })),
            "{errs:?}"
        );

        let mut t = tree_fixture();
        t.trees[0].def.nodes[1].unlocks.track_levels = vec![("track.missing".into(), 1)];
        let errs = tree_errors(&t, &loaded_part(part_fixture()));
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::UnknownUpgradeTrack { .. })),
            "{errs:?}"
        );

        let mut t = tree_fixture();
        t.trees[0].def.nodes[1].unlocks.track_levels = vec![("track.isp".into(), 9)];
        let errs = tree_errors(&t, &loaded_part(part_fixture()));
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::BadTechNode { .. })),
            "{errs:?}"
        );
    }

    /// Track shape rules: empty levels, bad effect, a stat the family
    /// cannot resolve, a family with no parts, duplicate ids.
    #[test]
    fn bad_tracks_are_rejected() {
        // A stat the fixture part cannot resolve (it has no Control).
        let mut t = tree_fixture();
        t.trees[0].def.tracks[0].stat_path = crate::part::StatPath::ControlTorqueNm;
        let errs = tree_errors(&t, &loaded_part(part_fixture()));
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::BadUpgradeTrack { .. })),
            "{errs:?}"
        );

        let mut t = tree_fixture();
        t.trees[0].def.tracks[0].family = "family.nobody".into();
        let errs = tree_errors(&t, &loaded_part(part_fixture()));
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::BadUpgradeTrack { .. })),
            "{errs:?}"
        );

        let mut t = tree_fixture();
        let dup = t.trees[0].def.tracks[0].clone();
        t.trees[0].def.tracks.push(dup);
        let errs = tree_errors(&t, &loaded_part(part_fixture()));
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::DuplicateUpgradeTrackId { .. })),
            "{errs:?}"
        );

        let mut t = tree_fixture();
        t.trees[0].def.tracks[0].levels.clear();
        let errs = tree_errors(&t, &loaded_part(part_fixture()));
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::BadUpgradeTrack { .. })),
            "{errs:?}"
        );
    }

    // ---- colony validator tests (§3.5.1) ----

    /// A sites file with one pad + a colony file over it + the recipe
    /// its converter names — the `electric_charge`-collision negative
    /// mutates the recipe's inputs.
    fn colony_fixture() -> (
        crate::colony::ColonyFile,
        SiteFile,
        ResourceFile,
        crate::load::LoadedRecipes,
    ) {
        let sites = SiteFile {
            schema_version: crate::site::SITE_SCHEMA_VERSION,
            sites: vec![crate::site::SiteDef {
                schema_version: crate::site::SITE_SCHEMA_VERSION,
                id: "pad".into(),
                display_name: LocKey::from("site.pad"),
                body: "terrin".into(),
                latitude_rad: 0.0,
                longitude_rad: 0.0,
                altitude_m: 0.0,
                launch: false,
                resource_yields: vec![("ore".into(), 0.5)],
                starting_stock: vec![],
            }],
        };
        let colonies = crate::colony::ColonyFile {
            schema_version: crate::colony::COLONY_SCHEMA_VERSION,
            colonies: vec![crate::colony::ColonyDef {
                site: "pad".into(),
                display_name: LocKey::from("colony.x"),
                modules: vec![crate::colony::ColonyModuleDef::Converter {
                    recipe: "recipe.smelt".into(),
                    priority: 5,
                    power_ec_per_s: 0.5,
                }],
                stores: vec![],
            }],
        };
        let recipes = crate::load::LoadedRecipes {
            recipes: vec![crate::load::LoadedRecipe {
                path: std::path::PathBuf::from("recipe.smelt.ron"),
                def: crate::recipe::RecipeDef {
                    schema_version: crate::recipe::RECIPE_SCHEMA_VERSION,
                    id: "recipe.smelt".into(),
                    display_name: "recipe.smelt.name".into(),
                    inputs: vec![("ore".into(), 1.0)],
                    outputs: vec![("metal".into(), 0.5)],
                },
            }],
        };
        let mut res = res_fixture();
        res.resources.push(crate::resource::ResourceDef {
            id: "ore".into(),
            display_name: "resource.ore".into(),
            mass_per_unit_kg: 1.0,
        });
        (colonies, sites, res, recipes)
    }

    /// The fixture is clean; listing `electric_charge` in the recipe's
    /// inputs AND a nonzero `power_ec_per_s` — one EC draw authored
    /// twice — is rejected, on a converter and on life support alike.
    #[test]
    fn colony_power_consumes_collision_is_rejected() {
        let (file, sites, res, recipes) = colony_fixture();
        assert!(validate_colonies(&file, &sites, &res, &recipes).is_empty());
        let (mut bad, _, _, mut bad_recipes) = colony_fixture();
        bad_recipes.recipes[0]
            .def
            .inputs
            .push(("electric_charge".into(), 1.0));
        let errs = validate_colonies(&bad, &sites, &res, &bad_recipes);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::InvalidColony { .. })),
            "{errs:?}"
        );
        // Same rule on LifeSupport's consumes_per_crew.
        bad.colonies[0].modules[0] = crate::colony::ColonyModuleDef::LifeSupport {
            consumes_per_crew: vec![("electric_charge".into(), 0.1)],
            power_ec_per_s: 0.5,
        };
        let errs = validate_colonies(&bad, &sites, &res, &recipes);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::InvalidColony { .. })),
            "{errs:?}"
        );
        // Zero power → no collision: an EC consumes entry alone is legal.
        bad.colonies[0].modules[0] = crate::colony::ColonyModuleDef::LifeSupport {
            consumes_per_crew: vec![("electric_charge".into(), 0.1)],
            power_ec_per_s: 0.0,
        };
        assert!(validate_colonies(&bad, &sites, &res, &recipes).is_empty());
    }

    // ---- Phase 9.2: module cross-references + version discipline ----

    /// `ModuleDef::Converter.recipe` and `ModuleDef::Experiment.kind`
    /// must resolve in `data/recipes`/`data/experiments` — an
    /// unresolved recipe is a dead converter in `csp-sim`, an
    /// unresolved kind can never run. (`part_errors` runs against the
    /// empty registries, so every authored ref is unresolvable.)
    #[test]
    fn converter_recipe_and_experiment_kind_resolve() {
        let mut d = part_fixture();
        d.category = PartCategory::Utility;
        d.modules = vec![
            ModuleDef::Converter {
                recipe: "recipe.missing".into(),
                priority: 0,
                power_w: 0.0,
            },
            ModuleDef::Experiment {
                kind: "exp.missing".into(),
                data_units: 1.0,
                reusable: true,
                situations: crate::part::SituationMask(1),
            },
        ];
        let errs = part_errors(d);
        assert!(
            errs.iter().any(|e| matches!(
                e,
                ValidationError::UnknownRecipe { recipe, .. } if recipe == "recipe.missing"
            )),
            "{errs:?}"
        );
        assert!(
            errs.iter().any(|e| matches!(
                e,
                ValidationError::UnknownExperimentKind { experiment, .. } if experiment == "exp.missing"
            )),
            "{errs:?}"
        );

        // Positive half: a recipe the registry *does* carry is fine.
        let mut d = part_fixture();
        d.category = PartCategory::Utility;
        d.modules = vec![ModuleDef::Converter {
            recipe: "recipe.smelt".into(),
            priority: 0,
            power_w: 0.0,
        }];
        let (_, _, _, recipes) = colony_fixture();
        let (_, experiments) = empty_registries();
        let errs = validate_parts(&loaded_part(d), &res_fixture(), &recipes, &experiments);
        assert!(
            !errs
                .iter()
                .any(|e| matches!(e, ValidationError::UnknownRecipe { .. })),
            "{errs:?}"
        );
    }

    /// `ExperimentCap.body` must name a body in the system — a cap on
    /// an unknown body is dead authored data (`cap_for`
    /// exact-matches). The experiment version gate rides the same
    /// file.
    #[test]
    fn experiment_cap_body_and_version_are_checked() {
        let mut loaded = crate::load::LoadedExperiments {
            experiments: vec![crate::load::LoadedExperiment {
                path: PathBuf::from("exp.test.ron"),
                def: crate::experiment::ExperimentDef {
                    schema_version: crate::experiment::EXPERIMENT_SCHEMA_VERSION,
                    id: "exp.test".into(),
                    display_name: "exp.test.name".into(),
                    transmit_ratio: 0.5,
                    caps: vec![crate::experiment::ExperimentCap {
                        body: "kerbin".into(),
                        situations: crate::part::SituationMask(1),
                        cap_data_units: 1.0,
                    }],
                },
            }],
        };
        let errs = validate_experiments(&loaded, &fixture());
        assert!(
            errs.iter().any(|e| matches!(
                e,
                ValidationError::BadExperiment { experiment, reason }
                    if experiment == "exp.test" && *reason == "cap names a body not in the system"
            )),
            "{errs:?}"
        );

        loaded.experiments[0].def.caps[0].body = "planet".into();
        loaded.experiments[0].def.schema_version = 0;
        let errs = validate_experiments(&loaded, &fixture());
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::SchemaVersion { found: 0, .. })),
            "{errs:?}"
        );
    }

    /// An experiment with no `caps` validates green no longer —
    /// `cap_for` returns `None` for every context, so the kind can
    /// never pay out anywhere.
    #[test]
    fn experiment_with_no_caps_is_rejected() {
        let loaded = crate::load::LoadedExperiments {
            experiments: vec![crate::load::LoadedExperiment {
                path: PathBuf::from("exp.test.ron"),
                def: crate::experiment::ExperimentDef {
                    schema_version: crate::experiment::EXPERIMENT_SCHEMA_VERSION,
                    id: "exp.test".into(),
                    display_name: "exp.test.name".into(),
                    transmit_ratio: 0.5,
                    caps: Vec::new(),
                },
            }],
        };
        let errs = validate_experiments(&loaded, &fixture());
        assert!(
            errs.iter().any(|e| matches!(
                e,
                ValidationError::BadExperiment { experiment, .. } if experiment == "exp.test"
            )),
            "{errs:?}"
        );
    }

    /// The `surface_resources` `ResourceId` keys resolve against
    /// `resources.ron` — gated in [`validate_dataset`]'s
    /// `check_surface_resources` (the resource file lives outside the
    /// system directory, so `validate_system` itself cannot express
    /// the check).
    #[test]
    fn unknown_surface_resource_is_rejected() {
        let mut sys = fixture();
        sys.bodies[1].def.surface_resources = vec![(
            "unobtainium".into(),
            crate::system::ConcentrationMapRef {
                png: "ore.png".to_string(),
            },
        )];
        let mut out = Vec::new();
        check_surface_resources(&sys, &res_fixture(), &mut out);
        assert!(
            out.iter()
                .any(|e| matches!(e, ValidationError::UnknownSurfaceResource { .. })),
            "{out:?}"
        );
        // The fixture's empty `surface_resources` reports nothing.
        let mut out = Vec::new();
        check_surface_resources(&fixture(), &res_fixture(), &mut out);
        assert!(out.is_empty(), "{out:?}");
    }

    /// Colony and recipe `display_name` keys get the same loc gate
    /// every other LocKey-carrying file already had.
    #[test]
    fn colony_and_recipe_loc_keys_are_checked() {
        let loc = LocFile {
            schema_version: SCHEMA_VERSION,
            strings: BTreeMap::new(),
        };
        let colonies = crate::colony::ColonyFile {
            schema_version: crate::colony::COLONY_SCHEMA_VERSION,
            colonies: vec![crate::colony::ColonyDef {
                site: "site.pad".into(),
                display_name: "colony.pad.name".into(),
                modules: Vec::new(),
                stores: Vec::new(),
            }],
        };
        let errs = validate_colony_loc_keys(&colonies, &loc);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::UnresolvedLocKey { key, .. } if key == "colony.pad.name")),
            "{errs:?}"
        );

        let recipes = crate::load::LoadedRecipes {
            recipes: vec![crate::load::LoadedRecipe {
                path: PathBuf::from("recipe.test.ron"),
                def: crate::recipe::RecipeDef {
                    schema_version: crate::recipe::RECIPE_SCHEMA_VERSION,
                    id: "recipe.test".into(),
                    display_name: "recipe.test.name".into(),
                    inputs: Vec::new(),
                    outputs: Vec::new(),
                },
            }],
        };
        let errs = validate_recipe_loc_keys(&recipes, &loc);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::UnresolvedLocKey { key, .. } if key == "recipe.test.name")),
            "{errs:?}"
        );
    }

    /// Each `SiteDef` entry re-declares its own version — the same
    /// current-or-previous band as the `SiteFile` wrapper.
    #[test]
    fn site_entry_schema_version_is_checked() {
        let (_, mut sites, res, _) = colony_fixture();
        sites.sites[0].schema_version = 0;
        let errs = validate_sites(&sites, &fixture(), &res);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::SchemaVersion { found: 0, .. })),
            "{errs:?}"
        );
    }

    /// C09-F2's loader-time guard: on a zero-synodic body (the
    /// fixture's planet is `rotation_period_s: None` — locked to the
    /// star, a permanently fixed sun) the site's Day/Night verdict is
    /// decided ONCE by `cos H0·cos φ`, so a longitude authored within
    /// `FIXED_SUN_TERMINATOR_MIN_COS` of the line is rejected — the
    /// shipped rask-mines π/2 (Day hung on a +6.1e-17 residue) is
    /// exactly the shape this blocks.
    #[test]
    fn zero_synodic_site_must_clear_the_terminator() {
        let (_, sites, res, _) = colony_fixture();
        // L0 = 0 in the fixture ⇒ h0 = lon − π; lon π/2 sits on the
        // line itself (|cos| = 6.1e-17 — the shipped bug, verbatim).
        let mut on_line = sites.clone();
        on_line.sites[0].body = "planet".into();
        on_line.sites[0].longitude_rad = core::f64::consts::FRAC_PI_2;
        let errs = validate_sites(&on_line, &fixture(), &res);
        assert!(
            errs.iter().any(|e| matches!(
                e,
                ValidationError::InvalidSite { reason, .. } if reason.contains("FIXED terminator")
            )),
            "{errs:?}"
        );
        // Off the line: the shipped fix's 2.4 rad (|cos| = 0.737) and
        // the antipode's permanent-night side (|cos| = 1.0 — dark is
        // a verdict, not an error) both carry real margins.
        for lon in [2.4, 0.0] {
            let mut ok = sites.clone();
            ok.sites[0].body = "planet".into();
            ok.sites[0].longitude_rad = lon;
            let errs = validate_sites(&ok, &fixture(), &res);
            assert!(errs.is_empty(), "lon {lon}: {errs:?}");
        }
        // A ROTATING body keeps cycling through day/night — grazing
        // the terminator is a phase, not a permanent verdict, so the
        // rule does not fire even at π/2.
        let mut sys = fixture();
        planet(&mut sys).rotation_period_s = Some(10_000.0);
        let errs = validate_sites(&on_line, &sys, &res);
        assert!(errs.is_empty(), "rotating body: {errs:?}");
    }

    /// `validate_contracts` runs the per-template schema_version gate.
    #[test]
    fn contract_schema_version_is_checked() {
        let contracts = crate::load::LoadedContracts {
            contracts: vec![crate::load::LoadedContract {
                path: PathBuf::from("contract.test.ron"),
                def: crate::contract::ContractTemplate {
                    schema_version: 0,
                    id: "contract.test".into(),
                    display_name: "k".into(),
                    description: "k".into(),
                    kind: crate::contract::ContractKind::Rescue,
                    generate: crate::contract::ContractGenerator {
                        weight: 1.0,
                        space: crate::contract::ContractParamSpace::Rescue {
                            bodies: vec!["planet".into()],
                        },
                    },
                    prerequisites: Vec::new(),
                    rewards: crate::contract::ContractRewards {
                        advance_funds: 1.0,
                        completion_funds: 1.0,
                        failure_penalty_funds: 1.0,
                    },
                    deadline_days: None,
                },
            }],
        };
        let (_, experiments) = empty_registries();
        let tree = tree_fixture();
        let no_sites = SiteFile {
            schema_version: crate::site::SITE_SCHEMA_VERSION,
            sites: vec![],
        };
        let errs = validate_contracts(
            &contracts,
            &fixture(),
            &experiments,
            &tree,
            &res_fixture(),
            &no_sites,
        );
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::SchemaVersion { found: 0, .. })),
            "{errs:?}"
        );
    }

    /// `validate_loc` re-checks the `LocFile` schema_version the
    /// loader gates.
    #[test]
    fn loc_schema_version_is_checked() {
        let loc = LocFile {
            schema_version: 0,
            strings: BTreeMap::new(),
        };
        assert!(matches!(
            validate_loc(&loc).as_slice(),
            [ValidationError::SchemaVersion { found: 0, .. }]
        ));
    }

    /// A `body.ron` file reference (`terrain.heightmap`, a
    /// `surface_resources` map) that is not a tame one-segment name
    /// reports `BadFileRef` — the same gate the loader runs before
    /// `join`, repeated for hand-rolled `LoadedSystem`s.
    #[test]
    fn unsafe_body_file_refs_are_rejected() {
        let mut sys = fixture();
        // bodies[0] is the star; the planet carries the refs here.
        sys.bodies[1].def.terrain.heightmap = Some("../x.png".to_string());
        sys.bodies[1].def.surface_resources = vec![(
            "ore".into(),
            crate::system::ConcentrationMapRef {
                png: "sub/map.png".to_string(),
            },
        )];
        let errs = validate_system(&sys);
        assert!(
            errs.iter()
                .filter(|e| {
                    matches!(e, ValidationError::BadFileRef { body, .. } if body == "planet")
                })
                .count()
                >= 2,
            "{errs:?}"
        );
    }

    // ---- Phase 9.3: LOW-batch gates ----

    /// `SituationMask` bits 6–7 are dead authored data —
    /// `situations()` drops them silently — so a mask setting them is
    /// rejected on the module and on an experiment cap alike (an
    /// all-spares mask is non-empty yet covers no situation).
    #[test]
    fn situation_mask_spare_bits_are_rejected() {
        let mut d = part_fixture();
        d.category = PartCategory::Utility;
        d.modules = vec![ModuleDef::Experiment {
            kind: "exp.test".into(),
            data_units: 1.0,
            reusable: true,
            situations: crate::part::SituationMask(0x40),
        }];
        let errs = part_errors(d);
        assert!(
            errs.iter().any(|e| matches!(
                e,
                ValidationError::BadModule {
                    reason: "situation mask sets spare bits outside the six situations",
                    ..
                }
            )),
            "{errs:?}"
        );

        let loaded = crate::load::LoadedExperiments {
            experiments: vec![crate::load::LoadedExperiment {
                path: PathBuf::from("exp.test.ron"),
                def: crate::experiment::ExperimentDef {
                    schema_version: crate::experiment::EXPERIMENT_SCHEMA_VERSION,
                    id: "exp.test".into(),
                    display_name: "exp.test.name".into(),
                    transmit_ratio: 0.5,
                    caps: vec![crate::experiment::ExperimentCap {
                        body: "planet".into(),
                        situations: crate::part::SituationMask(0xC0),
                        cap_data_units: 1.0,
                    }],
                },
            }],
        };
        let errs = validate_experiments(&loaded, &fixture());
        assert!(
            errs.iter().any(|e| matches!(
                e,
                ValidationError::BadExperiment {
                    reason: "situation mask sets spare bits outside the six situations",
                    ..
                }
            )),
            "{errs:?}"
        );
    }

    /// A `Delta`-effect track needs a positive finite base on every
    /// family part — the same gate `compose_upgrade_levels` runs at
    /// manufacture (`(base + Σ deltas) / base`). `Some(0.0)` used to
    /// pass the resolve check alone.
    #[test]
    fn delta_track_nonpositive_base_is_rejected() {
        let mut t = tree_fixture();
        t.trees[0].def.tracks[0].stat_path = crate::part::StatPath::MassDryKg;
        t.trees[0].def.tracks[0].levels[0].multiplier_or_delta = UpgradeEffect::Delta(5.0);
        let mut zero_base = part_fixture();
        zero_base.mass_dry_kg = 0.0;
        let errs = tree_errors(&t, &loaded_part(zero_base.clone()));
        assert!(
            errs.iter().any(|e| matches!(
                e,
                ValidationError::BadUpgradeTrack {
                    reason: "Delta track's stat_path base is not positive and finite on a family part",
                    ..
                }
            )),
            "{errs:?}"
        );
        // A Multiplier track on the same zero base stays legal — the
        // manufacture gate only fires when a delta term exists.
        let mut t = tree_fixture();
        t.trees[0].def.tracks[0].stat_path = crate::part::StatPath::MassDryKg;
        assert!(
            !tree_errors(&t, &loaded_part(zero_base))
                .iter()
                .any(|e| matches!(e, ValidationError::BadUpgradeTrack { .. })),
            "multiplier track must not trip the delta-base gate"
        );
        // Positive base + Delta: the intended authoring, still green.
        let mut t = tree_fixture();
        t.trees[0].def.tracks[0].levels[0].multiplier_or_delta = UpgradeEffect::Delta(5.0);
        assert!(
            tree_errors(&t, &loaded_part(part_fixture())).is_empty(),
            "positive-base delta track must stay valid"
        );
    }

    /// Recipe failures stamp `InvalidRecipe`, not `InvalidColony` —
    /// the recipe registry is not a colony file.
    #[test]
    fn recipe_errors_stamp_invalid_recipe() {
        let (_, _, res, mut recipes) = colony_fixture();
        recipes.recipes[0].def.outputs = vec![("unobtainium".into(), 1.0)];
        let errs = validate_recipes(&recipes, &res);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::InvalidRecipe { .. })),
            "{errs:?}"
        );
        assert!(
            !errs
                .iter()
                .any(|e| matches!(e, ValidationError::InvalidColony { .. })),
            "{errs:?}"
        );
    }

    /// A `LoadedRecipe` builder for the recipe-graph tests — `id`
    /// names file stem and def alike so the id↔stem check stays
    /// green and only the rule under test fires.
    fn recipe_fixture(
        id: &str,
        inputs: &[(&str, f64)],
        outputs: &[(&str, f64)],
    ) -> crate::load::LoadedRecipe {
        crate::load::LoadedRecipe {
            path: std::path::PathBuf::from(format!("{id}.ron")),
            def: crate::recipe::RecipeDef {
                schema_version: crate::recipe::RECIPE_SCHEMA_VERSION,
                id: id.into(),
                display_name: "recipe.test.name".into(),
                inputs: inputs.iter().map(|(r, q)| ((*r).into(), *q)).collect(),
                outputs: outputs.iter().map(|(r, q)| ((*r).into(), *q)).collect(),
            },
        }
    }

    /// C32-F2, the verifier's probe pair: `a 1.0 → b 0.5` +
    /// `b 0.5 → a 1.2` — the cycle that minted +0.2 `a`/s through
    /// the colony solver — is rejected twice over: `recipe.ba` is
    /// net-positive on its own, and the pair is a directed cycle.
    /// A self-referential row and a lone net-positive recipe fail
    /// too; an acyclic net-negative chain stays green.
    #[test]
    fn recipe_graph_rejects_cycles_and_net_positive_yield() {
        let (_, _, mut res, _) = colony_fixture();
        for r in ["a", "b"] {
            res.resources.push(crate::resource::ResourceDef {
                id: r.into(),
                display_name: "resource.test".into(),
                mass_per_unit_kg: 1.0,
            });
        }
        // The probe pair — a directed 2-cycle, and `recipe.ba` is
        // independently net-positive.
        let cyclic = crate::load::LoadedRecipes {
            recipes: vec![
                recipe_fixture("recipe.ab", &[("a", 1.0)], &[("b", 0.5)]),
                recipe_fixture("recipe.ba", &[("b", 0.5)], &[("a", 1.2)]),
            ],
        };
        let errs = validate_recipes(&cyclic, &res);
        for id in ["recipe.ab", "recipe.ba"] {
            assert!(
                errs.iter().any(|e| matches!(
                    e,
                    ValidationError::InvalidRecipe { id: eid, reason }
                        if eid == id && reason.contains("cycle")
                )),
                "{id} must name the cycle: {errs:?}"
            );
        }
        // `recipe.ba`'s 1.2-out for 0.5-in trips the net-yield gate
        // as well — one recipe id, two reasons.
        assert!(
            errs.iter().any(|e| matches!(
                e,
                ValidationError::InvalidRecipe { id, reason }
                    if id == "recipe.ba" && reason.contains("net-positive")
            )),
            "{errs:?}"
        );
        // A self-referential row is the degenerate cycle.
        let selfish = crate::load::LoadedRecipes {
            recipes: vec![recipe_fixture(
                "recipe.self",
                &[("a", 1.0)],
                &[("a", 0.5), ("b", 0.2)],
            )],
        };
        let errs = validate_recipes(&selfish, &res);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::InvalidRecipe { .. })),
            "{errs:?}"
        );
        // A lone ACYCLIC net-positive recipe mints units the same
        // way — the yield gate, not the cycle gate, catches it.
        let mint = crate::load::LoadedRecipes {
            recipes: vec![recipe_fixture(
                "recipe.mint",
                &[("ore", 1.0)],
                &[("metal", 1.5)],
            )],
        };
        let errs = validate_recipes(&mint, &res);
        assert!(
            errs.iter().any(|e| matches!(
                e,
                ValidationError::InvalidRecipe { id, reason }
                    if id == "recipe.mint" && reason.contains("net-positive")
            )),
            "{errs:?}"
        );
        // The sanctioned shape: an acyclic chain refining at a loss.
        let chained = crate::load::LoadedRecipes {
            recipes: vec![
                recipe_fixture("recipe.smelt", &[("ore", 1.0)], &[("metal", 0.6)]),
                recipe_fixture("recipe.cast", &[("metal", 1.0)], &[("a", 0.5)]),
            ],
        };
        assert!(
            validate_recipes(&chained, &res).is_empty(),
            "acyclic net-negative chain must stay green"
        );
    }

    /// `check_id` covers the remaining registry ids: sites, recipes,
    /// experiments, and biome legend entries.
    #[test]
    fn registry_ids_are_well_formed_checked() {
        let (_, mut sites, res, _) = colony_fixture();
        sites.sites[0].id = "Bad Id".into();
        let errs = validate_sites(&sites, &fixture(), &res);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::MalformedId { .. })),
            "site: {errs:?}"
        );

        let (_, _, res, mut recipes) = colony_fixture();
        recipes.recipes[0].def.id = "Bad Id".into();
        let errs = validate_recipes(&recipes, &res);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::MalformedId { .. })),
            "recipe: {errs:?}"
        );

        let mut loaded = crate::load::LoadedExperiments {
            experiments: vec![crate::load::LoadedExperiment {
                path: PathBuf::from("Bad Id.ron"),
                def: crate::experiment::ExperimentDef {
                    schema_version: crate::experiment::EXPERIMENT_SCHEMA_VERSION,
                    id: "Bad Id".into(),
                    display_name: "exp.test.name".into(),
                    transmit_ratio: 0.5,
                    caps: Vec::new(),
                },
            }],
        };
        let errs = validate_experiments(&loaded, &fixture());
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::MalformedId { .. })),
            "experiment: {errs:?}"
        );
        loaded.experiments.clear();

        let mut sys = fixture();
        with_biomes(&mut sys, 1, tiny_png(4, 4, 4, &[0; 16]));
        sys.bodies[1].biomes.as_mut().unwrap().legend.entries[0].id = "Bad Id".into();
        let errs = validate_system(&sys);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::MalformedId { .. })),
            "biome: {errs:?}"
        );
    }

    /// The colony `Drill.min_concentration` bound is the part-side
    /// `[0, 1]` — a `> 1` author permanently caps drill output.
    #[test]
    fn colony_drill_min_concentration_bound_matches_part_side() {
        let (mut file, sites, res, recipes) = colony_fixture();
        file.colonies[0].modules[0] = crate::colony::ColonyModuleDef::Drill {
            resource: "ore".into(),
            base_rate_per_s: 1.0,
            min_concentration: 1.5,
            power_ec_per_s: 1.0,
        };
        let errs = validate_colonies(&file, &sites, &res, &recipes);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::InvalidColony { .. })),
            "{errs:?}"
        );
        for edge in [0.0, 1.0] {
            file.colonies[0].modules[0] = crate::colony::ColonyModuleDef::Drill {
                resource: "ore".into(),
                base_rate_per_s: 1.0,
                min_concentration: edge,
                power_ec_per_s: 1.0,
            };
            assert!(
                validate_colonies(&file, &sites, &res, &recipes).is_empty(),
                "edge {edge} must stay legal"
            );
        }
    }

    /// §3.1 `Parachute`: sane bounds pass; every degenerate family is
    /// flagged (non-positive ceilings, negative areas, deployed ≤
    /// packed, negative ramp, a second module).
    #[test]
    fn parachute_module_bounds() {
        let good = |deployed_drag_area_m2| {
            let mut d = part_fixture();
            d.category = PartCategory::Utility;
            d.modules = vec![ModuleDef::Parachute {
                deploy_alt_m: 6_000.0,
                deploy_speed_mps: 260.0,
                packed_drag_area_m2: 0.4,
                deployed_drag_area_m2,
                deploy_time_s: 1.5,
            }];
            d
        };
        assert!(part_errors(good(120.0)).is_empty());
        // Zero ramp = instant deploy — legal.
        let mut d = good(120.0);
        let ModuleDef::Parachute { deploy_time_s, .. } = &mut d.modules[0] else {
            unreachable!()
        };
        *deploy_time_s = 0.0;
        assert!(part_errors(d).is_empty());
        // Deployed not exceeding packed is a no-op decelerator.
        let errs = part_errors(good(0.3));
        assert!(
            errs.iter().any(|e| matches!(
                e,
                ValidationError::BadModule {
                    reason: "parachute deployed_drag_area_m2 must exceed packed_drag_area_m2",
                    ..
                }
            )),
            "{errs:?}"
        );
        // Non-positive ceilings.
        for (alt, speed) in [(0.0, 260.0), (6_000.0, -1.0)] {
            let mut d = good(120.0);
            let ModuleDef::Parachute {
                deploy_alt_m,
                deploy_speed_mps,
                ..
            } = &mut d.modules[0]
            else {
                unreachable!()
            };
            *deploy_alt_m = alt;
            *deploy_speed_mps = speed;
            assert!(
                part_errors(d).iter().any(|e| matches!(
                    e,
                    ValidationError::BadModule { .. } | ValidationError::PartNonPositive { .. }
                )),
                "alt {alt} speed {speed} must be rejected"
            );
        }
        // Negative packed area / ramp.
        let mut d = good(120.0);
        let ModuleDef::Parachute {
            packed_drag_area_m2,
            deploy_time_s,
            ..
        } = &mut d.modules[0]
        else {
            unreachable!()
        };
        *packed_drag_area_m2 = -0.5;
        *deploy_time_s = -1.0;
        assert_eq!(
            part_errors(d)
                .iter()
                .filter(|e| matches!(e, ValidationError::BadModule { .. }))
                .count(),
            2
        );
        // Two decelerators on one part — ambiguous.
        let mut d = good(120.0);
        d.modules.push(d.modules[0].clone());
        let errs = part_errors(d);
        assert!(
            errs.iter().any(|e| matches!(
                e,
                ValidationError::BadModule {
                    reason: "more than one parachute module",
                    ..
                }
            )),
            "{errs:?}"
        );
    }

    /// §3.1 `Lab`: the authored lab's module shape passes (Utility
    /// bucket, positive throughput/multiplier, seats for the
    /// researchers); the degenerate fields are flagged.
    #[test]
    fn lab_module_bounds() {
        let lab_part = || {
            let mut d = part_fixture();
            d.id = "test.lab".into();
            d.category = PartCategory::Utility;
            d.modules = vec![
                ModuleDef::CrewSeat { slots: 2 },
                ModuleDef::Lab {
                    throughput_data_per_s: 0.5,
                    multiplier: 2.0,
                    researcher_slots: 2,
                },
            ];
            d
        };
        assert!(part_errors(lab_part()).is_empty());
        // Non-positive throughput / multiplier.
        for (t, m) in [(0.0, 2.0), (-1.0, 2.0), (0.5, 0.0), (f64::NAN, 2.0)] {
            let mut d = lab_part();
            let ModuleDef::Lab {
                throughput_data_per_s,
                multiplier,
                ..
            } = &mut d.modules[1]
            else {
                unreachable!()
            };
            *throughput_data_per_s = t;
            *multiplier = m;
            assert!(
                part_errors(d).iter().any(|e| matches!(
                    e,
                    ValidationError::BadModule { .. }
                        | ValidationError::PartNonPositive { .. }
                        | ValidationError::PartNonFinite { .. }
                )),
                "throughput {t} multiplier {m} must be rejected"
            );
        }
        // A lab nobody can staff is dead content.
        let mut d = lab_part();
        let ModuleDef::Lab {
            researcher_slots, ..
        } = &mut d.modules[1]
        else {
            unreachable!()
        };
        *researcher_slots = 0;
        let errs = part_errors(d);
        assert!(
            errs.iter().any(|e| matches!(
                e,
                ValidationError::BadModule {
                    reason: "lab with zero researcher slots",
                    ..
                }
            )),
            "{errs:?}"
        );
    }

    /// §3.1 `DockPort`: a port naming a real attach node is a valid
    /// part (and satisfies the Payload category rule); a port naming
    /// no node, or two ports claiming one node, is rejected.
    #[test]
    fn dock_port_module_rules() {
        let port_part = |node: &str| {
            let mut d = part_fixture();
            d.id = "test.dockport".into();
            d.category = PartCategory::Payload;
            d.attach_nodes.push(AttachNode {
                id: "mount".into(),
                position_m: csp_core::DVec3::new(0.0, 0.0, -0.9),
                direction: -csp_core::DVec3::Z,
                diameter_m: 1.25,
            });
            d.modules = vec![ModuleDef::DockPort {
                size: crate::part::PortSize::Medium,
                node: node.into(),
            }];
            d
        };
        // "top" is the fixture's only authored node.
        assert!(part_errors(port_part("top")).is_empty());
        // A port face that names no attach node anchors nothing.
        let errs = part_errors(port_part("ghost"));
        assert!(
            errs.iter().any(|e| matches!(
                e,
                ValidationError::BadModule {
                    reason: "dock_port node names no attach node",
                    ..
                }
            )),
            "{errs:?}"
        );
        // Two ports, one face — ambiguous authored data.
        let mut d = port_part("top");
        d.modules.push(d.modules[0].clone());
        let errs = part_errors(d);
        assert!(
            errs.iter().any(|e| matches!(
                e,
                ValidationError::BadModule {
                    reason: "two dock ports claim the same attach node",
                    ..
                }
            )),
            "{errs:?}"
        );
    }
}
