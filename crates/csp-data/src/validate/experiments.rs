//! The §3.2 `data/experiments/*.ron` rules: id↔stem, unique IDs,
//! `schema_version`, `transmit_ratio` band, cap sanity and
//! non-overlapping `(body, situation)` coverage, `cap.body`
//! resolvable, and the experiment set's [`LocKey`] sweep.

use std::collections::BTreeSet;

use crate::load::LoadedSystem;

use crate::loc::LocFile;

use super::ValidationError;
use super::common::{check_id, check_situation_mask};
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
/// [`crate::validate::validate_contract_loc_keys`] pattern): every def's
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
