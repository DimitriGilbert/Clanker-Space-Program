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
