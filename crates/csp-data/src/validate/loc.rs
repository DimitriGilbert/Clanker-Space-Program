//! The `loc/en.ron` file-shape re-check — the resolution half lives
//! in the `validate_*_loc_keys` family carried by each content kind.

use crate::loc::LocFile;

use super::ValidationError;

/// §1.2's `loc/en.ron` file-shape check repeated validator-side — the
/// same `schema_version` gate [`crate::load::load_loc`] runs, here for
/// hand-rolled tables (the "validator re-checks what serde enforced"
/// convention). The key-resolution half lives in the
/// `validate_*_loc_keys` family.
pub fn validate_loc(loc: &LocFile) -> Vec<ValidationError> {
    if loc.schema_version != crate::loc::LOC_SCHEMA_VERSION {
        vec![ValidationError::SchemaVersion {
            file: "loc/en.ron".to_string(),
            found: loc.schema_version,
            expected: crate::loc::LOC_SCHEMA_VERSION,
        }]
    } else {
        Vec::new()
    }
}
