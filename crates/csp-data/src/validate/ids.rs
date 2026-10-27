//! The §7.7 ID-stability gate: the loaded part set against the
//! committed `data/id-baseline.ron` — no disappearance without a
//! deprecation row, no unrecorded live ID, and a coherent baseline.

use std::collections::BTreeSet;

use crate::load::LoadedParts;

use super::ValidationError;

/// §7.7's ID-stability gate over the loaded part set and the committed
/// baseline (`data/id-baseline.ron`, [`crate::ids::IdBaseline`]):
///
/// - every baseline `parts` id must still load, or carry a
///   `deprecated` → replacement row ([`ValidationError::PartIdDisappeared`]);
/// - every loaded part id must be in the baseline
///   ([`ValidationError::PartIdUnrecorded`]) — a new id is permanent
///   the moment it ships, so the baseline update lands in the same
///   commit;
/// - the baseline itself must be coherent: no duplicate `parts`
///   entries, a `deprecated` key must not still ship, and a
///   replacement must name a live id ([`ValidationError::BadIdBaseline`]).
///
/// The `schema_version` gate runs here too — `load_id_baseline` checks
/// it at parse, but the "validator re-checks what serde enforced"
/// convention applies to hand-rolled baselines.
pub fn validate_id_stability(
    loaded: &LoadedParts,
    baseline: &crate::ids::IdBaseline,
) -> Vec<ValidationError> {
    let mut out = Vec::new();
    if baseline.schema_version != crate::ids::ID_BASELINE_SCHEMA_VERSION {
        out.push(ValidationError::SchemaVersion {
            file: "id-baseline.ron".to_string(),
            found: baseline.schema_version,
            expected: crate::ids::ID_BASELINE_SCHEMA_VERSION,
        });
    }
    let live: BTreeSet<&str> = loaded.parts.iter().map(|p| p.def.id.as_str()).collect();
    let mut recorded = BTreeSet::new();
    for id in &baseline.parts {
        let id = id.as_str();
        if !recorded.insert(id) {
            out.push(ValidationError::BadIdBaseline {
                reason: format!("parts lists {id:?} twice"),
            });
        }
        if !live.contains(id) && !baseline.deprecated.keys().any(|k| k.as_str() == id) {
            out.push(ValidationError::PartIdDisappeared { id: id.to_string() });
        }
    }
    for id in &live {
        if !recorded.contains(id) {
            out.push(ValidationError::PartIdUnrecorded {
                id: (*id).to_string(),
            });
        }
    }
    for (old, new) in &baseline.deprecated {
        if live.contains(old.as_str()) {
            out.push(ValidationError::BadIdBaseline {
                reason: format!(
                    "deprecated id {old:?} still loads — retire the part file or drop the row"
                ),
            });
        }
        if !live.contains(new.as_str()) {
            out.push(ValidationError::BadIdBaseline {
                reason: format!(
                    "deprecated id {old:?} names replacement {new:?}, which no live part carries"
                ),
            });
        }
    }
    out
}
