//! The §3.5 `resources.ron` registry rules: `schema_version`, unique
//! well-formed IDs, and `mass_per_unit_kg` finite and `≥ 0`.

use std::collections::BTreeSet;

use crate::resource::{RESOURCE_SCHEMA_VERSION, ResourceFile};

use super::ValidationError;

use super::common::check_id;

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
