//! The `data/resources.ron` schema — the §3.5 resource registry.
//!
//! §3.5 pins the v1 resource *set* (`Ore`, `Metal`, `Composite`,
//! `LiquidFuel`, `Oxidizer`, `Monoprop`, `ElectricCharge`, `Water`,
//! `Food`, `Oxygen` — "ten, not thirty") but the numbers that make a
//! resource physical — what a unit weighs — need somewhere to live.
//! This file is that place: part `Tank` capacities and engine
//! `propellants` are authored in resource units and fractions, and the
//! effective-stat resolver plus the editor's Δv readout convert units
//! to mass through this table. Without it "capacity: 400 units of
//! LiquidFuel" has no mass and Tsiolkovsky cannot run.

use serde::{Deserialize, Serialize};

use crate::ids::{LocKey, ResourceId};

/// The `schema_version` this loader understands for `resources.ron` —
/// its own namespace, like `balance.ron` (§3.1 first-field convention).
pub const RESOURCE_SCHEMA_VERSION: u32 = 1;

/// One resource type: identity, display name, and the mass of one
/// unit. `mass_per_unit_kg` is the only physical datum the sim needs —
/// a `Tank`'s wet mass is `Σ units·mass_per_unit_kg`, and
/// `ElectricCharge` legitimately authors `0.0` (charge is massless; the
/// validator allows exactly `>= 0` here rather than `> 0`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResourceDef {
    /// Stable resource ID (`"liquid_fuel"` — matched case-insensitively
    /// nowhere; IDs are strings and must be spelled identically in
    /// tanks, engines, and colony defs).
    pub id: ResourceId,
    /// Loc key for the display name.
    pub display_name: LocKey,
    /// Mass of one unit of this resource (kg). `0.0` is legal and means
    /// "massless" (`ElectricCharge`).
    pub mass_per_unit_kg: f64,
}

/// `data/resources.ron` — the full v1 resource set. The part validator
/// checks every `ResourceId` a part references (build materials, tank
/// capacities, engine propellants, drill outputs) against this table.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResourceFile {
    /// First field of every data file (§3.1). Must equal
    /// [`RESOURCE_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// The resource set. IDs must be unique (validator rule).
    pub resources: Vec<ResourceDef>,
}

impl ResourceFile {
    /// Mass of one unit of `id`, or `None` if the resource is not in
    /// the table. Linear scan — the §3.5 set is ten entries and this
    /// runs at load/edit time, not in the tick.
    pub fn mass_per_unit(&self, id: &ResourceId) -> Option<f64> {
        self.resources
            .iter()
            .find(|r| r.id == *id)
            .map(|r| r.mass_per_unit_kg)
    }

    /// Whether `id` is a declared resource.
    pub fn contains(&self, id: &ResourceId) -> bool {
        self.resources.iter().any(|r| r.id == *id)
    }
}
