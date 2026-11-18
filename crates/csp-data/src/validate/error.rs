//! [`ValidationError`] — the typed failure list every validator in
//! this module returns (CI and the editor consume the same list), plus
//! its `Display` rendering and the `fmt_part`/`fmt_tech` delegation
//! helpers.

use std::fmt;

use crate::biome::{BIOME_MAP_MAX_H, BIOME_MAP_MAX_W};

use super::PERIOD_REL_TOL;

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
    /// `terrain.amplitude_frac` outside the `[0.0, 0.02]` band (M5,
    /// §5.2-R2): `0.0` is a legal flat body, negatives fold relief
    /// inward, and past 2% of the radius the displaced surface stops
    /// reading as terrain.
    TerrainAmplitudeOutOfRange {
        /// The body.
        body: String,
        /// The offending amplitude fraction.
        value: f64,
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
    /// [`crate::validate::validate_dataset`] instead.
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
    /// A module's attach-node *reference* is inconsistent — a
    /// `DockPort` naming no attach node, or two port modules claiming
    /// the same one. Carries the node so the diagnostic can name it —
    /// every other check in the file names the offending id (C32-F8);
    /// `BadModule`'s `&'static str` reason alone could not.
    BadModuleNode {
        /// The part.
        part: String,
        /// The attach-node id the module names or claims.
        node: String,
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
    /// A file or directory under `data/systems/<sys>/` that the
    /// manifest never references — an unlisted `<body>/` directory
    /// (a body that loads nowhere), or an entry inside a body dir the
    /// def does not name (a stray `biomes.png` beside a
    /// `biome_map: None` body). The manifest-driven load skips these
    /// silently while the data-set hash still covers them, so the
    /// tree could drift with zero diagnostics (C31-F3).
    OrphanContent {
        /// The unreferenced path, as the loader recorded it.
        path: String,
    },
    /// §7.7's ID-stability tripwire: the committed baseline
    /// (`data/id-baseline.ron`) names a `PartId` the loaded set no
    /// longer carries and `deprecated` records no replacement — the
    /// CI failure §7.7 requires for an ID that disappears.
    PartIdDisappeared {
        /// The vanished ID.
        id: String,
    },
    /// §7.7's other half: a loaded `PartId` is absent from the
    /// committed baseline — IDs are permanent the moment they ship,
    /// so the baseline must be updated in the same commit that
    /// authors the part.
    PartIdUnrecorded {
        /// The unrecorded ID.
        id: String,
    },
    /// `data/id-baseline.ron` is internally inconsistent — a
    /// duplicated `parts` entry, a `deprecated` key still shipped as
    /// a live id, or a replacement naming no live id.
    BadIdBaseline {
        /// Why it failed.
        reason: String,
    },
}

impl fmt::Display for ValidationError {
    #[allow(clippy::too_many_lines)] // lint-allow (§8.6): one arm per variant + the explicit delegation alternation
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
            Self::TerrainAmplitudeOutOfRange { body, value } => {
                write!(
                    f,
                    "{body:?}.terrain.amplitude_frac: {value} outside [0.0, 0.02] (0 = flat body)"
                )
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
            Self::OrphanContent { path } => write!(
                f,
                "{path}: unreferenced content — not listed in system.ron's manifest \
                 (or not referenced by its body.ron); loads nowhere but still feeds \
                 the data-set hash"
            ),
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
            | Self::BadModuleNode { .. }
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
            | Self::PartIdDisappeared { .. }
            | Self::PartIdUnrecorded { .. }
            | Self::BadIdBaseline { .. }
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
    #[allow(clippy::too_many_lines)] // lint-allow (§8.6): one arm per variant is the table
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
            Self::BadModuleNode { part, node, reason } => {
                write!(f, "{part:?}: module node {node:?}: {reason}")
            }
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
            Self::PartIdDisappeared { id } => write!(
                f,
                "id-baseline.ron: part id {id:?} disappeared — §7.7 requires a \
                 `deprecated` replacement entry (or a migration) for a removed id"
            ),
            Self::PartIdUnrecorded { id } => write!(
                f,
                "id-baseline.ron: part id {id:?} is new — record it in the §7.7 \
                 baseline in the same commit"
            ),
            Self::BadIdBaseline { reason } => {
                write!(f, "id-baseline.ron: {reason}")
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
