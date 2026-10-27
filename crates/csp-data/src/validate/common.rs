//! Check helpers shared across the validator's content kinds — ID
//! well-formedness, situation-mask hygiene, and the finite/positive
//! scalar gates that stamp [`ValidationError`]s with their owning
//! context's name.

use super::ValidationError;

/// IDs are directory names under `data/systems/<sys>/` — keep them tame:
/// non-empty, `[a-z0-9._-]` only, never a `.`/`..` segment, and never a
/// DOS device name (the same Windows trap [`crate::load::authored_name_ok`]
/// guards file refs against — an id named `con` is a directory name too).
pub(crate) fn id_well_formed(id: &str) -> bool {
    !id.is_empty()
        && id != "."
        && id != ".."
        && id.chars().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '_' || c == '-'
        })
        && !crate::load::is_dos_device_name(id)
}

pub(crate) fn check_id(id: &str, out: &mut Vec<ValidationError>) {
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
pub(crate) fn check_situation_mask(
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

pub(crate) fn check_finite(
    body: &str,
    field: &'static str,
    v: f64,
    out: &mut Vec<ValidationError>,
) {
    if !v.is_finite() {
        out.push(ValidationError::NonFinite {
            body: body.to_string(),
            field,
        });
    }
}

pub(crate) fn check_positive(
    body: &str,
    field: &'static str,
    v: f64,
    out: &mut Vec<ValidationError>,
) {
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

pub(crate) fn part_positive(
    part: &str,
    field: &'static str,
    v: f64,
    out: &mut Vec<ValidationError>,
) {
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
