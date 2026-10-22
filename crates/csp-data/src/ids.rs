//! Stable string identifiers for the data domain (PLAN §3.1: "Part IDs are
//! stable strings — `csp.engine.lv30` — never indices; saves depend on it").
//!
//! These are *data-file* IDs — they name things authored under `data/` —
//! and live here with the defs that consume them. They are a different
//! family from the §2.2 sim-side IDs (`VesselId`, `PartInstanceId`, …):
//! those are monotonic `u64`s allocated by the sim and belong to the
//! csp-core/csp-sim "IDs" role §2.1 assigns there. A data ID is never
//! recycled into a sim ID or vice versa.
//!
//! `serde(transparent)` keeps the RON spelling a bare string
//! (`id: "terrin"`, not `id: (…)`) so data files stay terse.

use std::fmt;

use serde::{Deserialize, Serialize};

macro_rules! id_newtype {
    ($(#[$doc:meta])+ $name:ident) => {
        $(#[$doc])+
        ///
        /// Wraps a [`String`]. Ordering is lexicographic so IDs can key
        /// `BTreeMap`/`BTreeSet` — the determinism-domain containers
        /// (§2.2 bans `HashMap`/`HashSet` here).
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            /// Borrow the ID's text.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<&str> for $name {
            fn from(s: &str) -> Self {
                Self(s.to_string())
            }
        }

        impl From<String> for $name {
            fn from(s: String) -> Self {
                Self(s)
            }
        }
    };
}

id_newtype! {
    /// Identifier of a star system (`data/systems/<id>/`).
    SystemId
}
id_newtype! {
    /// Identifier of a celestial body within its system (`<sys>/<id>/body.ron`).
    BodyId
}
id_newtype! {
    /// Identifier of a biome, stable across terrain changes (§3.0.1 —
    /// science keys on `(body, biome, situation)`, so biome IDs may only
    /// change through a deliberate data-set change).
    BiomeId
}
id_newtype! {
    /// Identifier of a resource type (propellants, mineables — §3.1).
    ResourceId
}
id_newtype! {
    /// Indirection into `data/loc/en.ron` (§1.2: v1 is English-only;
    /// every user-facing data string goes through a `LocKey`).
    LocKey
}
id_newtype! {
    /// Identifier of a part definition (`data/parts/<id>.ron`). Stable
    /// across the life of the project — §3.1: "Part IDs are stable
    /// strings (`csp.engine.lv30`), never indices — saves depend on it."
    /// A `BuiltPart` in a §7.2 save names one of these.
    PartId
}
id_newtype! {
    /// Identifier of a part *family* — the grouping §3.3's
    /// `UpgradeTrack { family, stat_path, … }` and §3.4's
    /// `FamilyQualification` key on. Parts in a family share upgrade
    /// tracks and qualification state, so a family ID changes only when
    /// the design intent is "this is a different lineage".
    FamilyId
}
id_newtype! {
    /// Identifier of one of a part's authored variants
    /// (`PartDef::variants`, §3.3 "scaling + variants"). A `BuiltPart`'s
    /// `variant` field names one of these; the resolver applies its
    /// authored stat multipliers at manufacture.
    VariantId
}
id_newtype! {
    /// Identifier of a tech-tree node (§3.3, `data/techtree/*.ron`).
    /// Part defs *reference* these (`PartDef::tech_requirement`); the
    /// tree loader that resolves them lands with the tech tree itself.
    TechNodeId
}
id_newtype! {
    /// Identifier of an §3.3 `UpgradeTrack` (`data/techtree/*.ron`). A
    /// track couples one `FamilyId` to one `StatPath`; tech nodes grant
    /// level caps on it and the sim's `ResearchUpgradeLevel` command
    /// buys the levels individually.
    UpgradeTrackId
}
id_newtype! {
    /// Identifier of a program facility a tech node unlocks
    /// (§3.3 — labs, test stands, pads). The facility defs themselves
    /// land with the facility economy milestone; the id type lands
    /// with the tree that references it.
    FacilityId
}
id_newtype! {
    /// Identifier of a §3.6 contract type a tech node unlocks
    /// ("satellite placement", "survey data run", …). The contract
    /// template defs land with the contracts milestone; the id type
    /// lands with the tree that references it.
    ContractTypeId
}
id_newtype! {
    /// Identifier of a science experiment kind (§3.2 `Experiment`
    /// modules key data records on it).
    ExperimentId
}
id_newtype! {
    /// Identifier of a converter recipe (§3.5.1 `Converter` modules
    /// name one). Recipe defs land with the colony data set.
    RecipeId
}
id_newtype! {
    /// Identifier of a §3.5 surface site — a launch pad or recovery
    /// location on a body (`data/systems/<sys>/sites.ron`). Sites are
    /// where vessels launch from and where `RecoverVessel` credits
    /// resources; a launch site recovers at `recovery_efficiency` 1.0.
    SiteId
}
