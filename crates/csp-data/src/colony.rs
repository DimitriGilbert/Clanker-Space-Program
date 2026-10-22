//! The `colonies.ron` schema — the §3.5.1 colony flow-network's authored
//! surface.
//!
//! A colony is a surface installation at a [`SiteId`]: a module list plus
//! initial resource stores. The sim resolves the site's body and
//! body-fixed pose at founding and runs the §3.5.1 piecewise-linear
//! network over the modules — so this file authors *rates and
//! capacities only*. M8-B landed the production details the M8-A
//! network's abstract rates stood in for: drills read the site's
//! authored `resource_yields` against their `min_concentration`,
//! converters run named [`crate::RecipeDef`]s instead of inline rate
//! pairs, and generators burn a fuel resource for their EC.
//!
//! Power is a resource like any other: [`ColonyModuleDef::Solar`] and
//! [`ColonyModuleDef::Generator`] produce `electric_charge` units/s,
//! every powered module's `power_ec_per_s` consumes it, and
//! [`ColonyModuleDef::Battery`] is its store — the Day/Night insolation
//! step, the battery drain, and the `PowerLost` starvation transition
//! all fall out of the same linear store machinery as ore and oxygen.

use serde::{Deserialize, Serialize};

use crate::ids::{LocKey, RecipeId, ResourceId, SiteId};

/// The `schema_version` this loader understands for `colonies.ron` —
/// its own namespace, like `sites.ron` (§3.1 first-field convention).
///
/// Version 2 (M8-B): `Drill` gained `min_concentration` (renaming
/// `rate_per_s` to `base_rate_per_s` — the authored rate is now scaled
/// by the site yield at solve time), `Converter` swapped its inline
/// `consumes`/`produces` pairs for the `RecipeId` indirection the
/// part-side `ModuleDef` already used, and `Generator` gained its fuel
/// burn + allocation priority.
pub const COLONY_SCHEMA_VERSION: u32 = 2;

/// One `electric_charge`-denominated store capacity or resource bin.
/// `ColonyDef::stores` uses this for the founding levels; the
/// sim-side store record adds the anchor/rate fields the linear
/// model needs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ColonyStoreInit {
    /// The resource stocked.
    pub resource: ResourceId,
    /// Level at founding (units). Non-negative; the sim clamps it to
    /// the aggregate capacity the `Store`/`Battery` modules give it.
    pub level: f64,
}

/// §3.5.1's module set — the functional payloads a colony carries.
/// One entry = one `ModuleIndex` (the founding order IS the index
/// order — the allocation pass's third tie-break key). Discriminants
/// are explicit, stable, append-only (§7.1 rule 7).
#[repr(u16)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ColonyModuleDef {
    /// Solar collector — produces
    /// `day_insolation · area_m2 · efficiency` `electric_charge` units/s
    /// while the colony is `Day`-lit, nothing at `Night` (§3.5.1 rule
    /// 1's two-state step; the daylit insolation average is resolved
    /// sim-side from the body's authored `insolation_w_m2` and the
    /// site latitude).
    Solar {
        /// Collector area (m²).
        area_m2: f64,
        /// Conversion efficiency (0..=1): flux → EC units/s.
        efficiency: f64,
    },
    /// Fuel-burning power source (M8-B): consumes `fuel` at
    /// `fuel_rate_per_s` units/s through the allocation pass like
    /// every other consumer, producing `rate_per_s` EC units/s at its
    /// granted fraction — a starved generator throttles with its fuel
    /// grant. `fuel_rate_per_s: 0.0` is the RTG reading: no request
    /// registers, so the module runs unthrottled in either insolation
    /// state. Generators keep producing in `Degraded` (the §3.5.1
    /// "solar/generator output continues" clause — burning stockpiled
    /// fuel is exactly how a `PowerLost` colony climbs out).
    Generator {
        /// The fuel resource consumed (`fuel_rate_per_s > 0` must
        /// name a real resource — the validator checks).
        fuel: ResourceId,
        /// Fuel draw at full operation (units/s); `0.0` = RTG.
        fuel_rate_per_s: f64,
        /// EC production at full operation (units/s).
        rate_per_s: f64,
        /// The fuel request's allocation priority (§3.5.1 rule 3:
        /// lower wins) — authored like the converter's so the data
        /// can say whether the generator outranks the converters it
        /// powers when fuel is scarce.
        priority: u8,
    },
    /// Surface harvester (M8-B): produces `resource` at
    /// `base_rate_per_s · min(1, concentration / min_concentration)`
    /// units/s — `concentration` is the colony site's authored
    /// `SiteDef::resource_yields` entry for the resource (0 when the
    /// site lists none, so a drill founded on barren ground produces
    /// nothing). Draws `power_ec_per_s` EC while running.
    Drill {
        /// Resource extracted.
        resource: ResourceId,
        /// Extraction rate (units/s) at full concentration —
        /// `min(1, yield / min_concentration)` scales it.
        base_rate_per_s: f64,
        /// The yield fraction at which the drill reaches full rate
        /// (matches the part-side `ModuleDef::Drill::min_concentration`).
        min_concentration: f64,
        /// EC draw while running (units/s).
        power_ec_per_s: f64,
    },
    /// ISRU converter — runs `recipe` at its allocated operating
    /// fraction: the recipe's `inputs` are the consumer requests and
    /// its `outputs` the production rows (M8-B's `RecipeId`
    /// indirection replacing M8-A's inline rate pairs). `priority` is
    /// the §3.5.1 rule-3 tie-break head, and `power_ec_per_s`
    /// registers an additional EC draw that participates in the same
    /// starvation pro-rating.
    Converter {
        /// The `data/recipes/` def this converter runs.
        recipe: RecipeId,
        /// §3.5.1 rule-3 allocation priority — lower wins.
        priority: u8,
        /// EC draw while running (units/s).
        power_ec_per_s: f64,
    },
    /// Resource storage — adds `capacity` units of store for
    /// `resource` to the colony's aggregate per-resource store.
    Store {
        /// Resource stored.
        resource: ResourceId,
        /// Capacity added (units).
        capacity: f64,
    },
    /// Battery — `electric_charge` storage (§3.5's "a Cargo bay is
    /// just a Tank"; the linear-reservoir rule §3.5.1 rule 1 relies
    /// on). Adds `capacity` EC units to the EC store.
    Battery {
        /// EC capacity added (units).
        capacity: f64,
    },
    /// Radiator — contributes `emissivity · σ · area` to the
    /// interval's algebraic steady-state solve
    /// `Q_in = εσA·T_eq⁴` (§3.5.1 rule 2). Holds no rate of its own.
    Radiator {
        /// Radiating area (m²).
        area_m2: f64,
        /// Surface emissivity ε (0..=1).
        emissivity: f64,
    },
    /// Crew life support — draws `power_ec_per_s` EC plus each
    /// `consumes_per_crew` entry scaled by the crew count aboard,
    /// while any crew are aboard. Empty crew → the module idles.
    /// An unmet required resource is what the `LifeSupportDepleted`
    /// transition keys on (§3.5.1 rule 5).
    LifeSupport {
        /// `(resource, units/s)` each crew member aboard consumes.
        consumes_per_crew: Vec<(ResourceId, f64)>,
        /// EC draw while crew are aboard (units/s).
        power_ec_per_s: f64,
    },
}

impl csp_core::StableDiscriminant for ColonyModuleDef {
    fn discriminant(&self) -> u16 {
        match self {
            Self::Solar { .. } => 0,
            Self::Generator { .. } => 1,
            Self::Drill { .. } => 2,
            Self::Converter { .. } => 3,
            Self::Store { .. } => 4,
            Self::Battery { .. } => 5,
            Self::Radiator { .. } => 6,
            Self::LifeSupport { .. } => 7,
        }
    }
}

/// One authored colony (§3.5.1): the site it occupies, the module
/// list (founding order = `ModuleIndex` order), and the founding
/// store levels. The sim command carries this whole def verbatim —
/// the same log-contained pattern `SpawnPhysicalVessel`'s `craft`
/// uses — so a replay needs no def table to re-found the colony.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ColonyDef {
    /// The site the colony occupies — resolves body and body-fixed
    /// pose at founding.
    pub site: SiteId,
    /// Loc key for the display name.
    pub display_name: LocKey,
    /// The module list — index order is the allocation pass's
    /// `ModuleIndex` tie-break.
    pub modules: Vec<ColonyModuleDef>,
    /// Founding store levels, clamped at apply to the aggregate
    /// capacities the `Store`/`Battery` modules declare.
    pub stores: Vec<ColonyStoreInit>,
}

/// `data/systems/<sys>/colonies.ron` — authored starting colonies,
/// optional like `sites.ron` (a missing file reads as an empty set:
/// a system without authored colonies simply starts with none —
/// `FoundColony` commands found the rest).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ColonyFile {
    /// First field of every data file (§3.1). Must equal
    /// [`COLONY_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// The authored colonies.
    pub colonies: Vec<ColonyDef>,
}
