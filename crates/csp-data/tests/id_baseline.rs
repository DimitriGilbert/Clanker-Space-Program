//! §7.7 ID-stability tests (C32-F3): `data/id-baseline.ron` is the
//! committed record of the permanent `PartId` set;
//! `validate_id_stability` fails in CI on an id that disappears
//! without a `deprecated` row, on a loaded id the baseline does not
//! record, and on a malformed baseline.

#![forbid(unsafe_code)]
// Tests signal failure by panicking — §2.13's no-panic rule is for the
// shipping fallible paths, so unwrap/expect are allowed in test code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use csp_data::{
    EmbeddedSource, IdBaseline, LoadedPart, LoadedParts, PartDef, PartId, ValidationError,
    load_id_baseline, load_parts, load_parts_from, validate_id_stability,
};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap_or_else(|_| Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

fn data() -> PathBuf {
    repo_root().join("data")
}

/// A `LoadedPart` whose def only needs a plausible `id` — the
/// stability check reads `def.id` and nothing else, so a real shipped
/// def with a rewritten id is the least fragile stub (a hand-rolled
/// `PartDef` RON would drift against the schema).
fn stub_part(id: &str) -> LoadedPart {
    let text = std::fs::read_to_string(data().join("parts").join("csp.pod.mk1.ron")).unwrap();
    let mut def = ron::de::from_str::<PartDef>(&text).unwrap();
    def.id = PartId::from(id);
    LoadedPart {
        def,
        path: PathBuf::from(format!("{id}.ron")),
    }
}

fn loaded(ids: &[&str]) -> LoadedParts {
    LoadedParts {
        parts: ids.iter().map(|id| stub_part(id)).collect(),
    }
}

fn baseline(ids: &[&str], deprecated: &[(&str, &str)]) -> IdBaseline {
    IdBaseline {
        schema_version: csp_data::ID_BASELINE_SCHEMA_VERSION,
        parts: ids.iter().map(|s| PartId::from(*s)).collect(),
        deprecated: deprecated
            .iter()
            .map(|(a, b)| (PartId::from(*a), PartId::from(*b)))
            .collect(),
    }
}

/// The shipped baseline parses, covers every shipped part id exactly,
/// and validates green against `data/parts/`. This is the §7.7 CI
/// gate's green half — deleting a part file reddens it (see
/// `deleted_part_file_fails_the_gate`).
#[test]
fn shipping_baseline_matches_shipped_parts() {
    let baseline = load_id_baseline(&data().join("id-baseline.ron"))
        .unwrap()
        .expect("data/id-baseline.ron must exist — the §7.7 gate is armed by it");
    let parts = load_parts(&data().join("parts")).unwrap();
    let errs = validate_id_stability(&parts, &baseline);
    assert!(errs.is_empty(), "validator errors: {errs:#?}");
}

/// A baseline id with no loaded def and no `deprecated` row reports
/// `PartIdDisappeared` — §7.7's required CI failure.
#[test]
fn disappearing_id_fails() {
    let parts = loaded(&["a", "b"]);
    let b = baseline(&["a", "b", "retired"], &[]);
    let errs = validate_id_stability(&parts, &b);
    assert!(
        errs.iter().any(|e| matches!(
            e,
            ValidationError::PartIdDisappeared { id } if id == "retired"
        )),
        "{errs:?}"
    );
    // …unless the retirement is recorded with a live replacement.
    let b = baseline(&["a", "b", "retired"], &[("retired", "b")]);
    assert!(validate_id_stability(&parts, &b).is_empty());
}

/// A loaded id absent from the baseline reports `PartIdUnrecorded` —
/// a new permanent id must be blessed into the baseline in the same
/// commit that authors the part.
#[test]
fn unrecorded_id_fails() {
    let parts = loaded(&["a", "new"]);
    let b = baseline(&["a"], &[]);
    let errs = validate_id_stability(&parts, &b);
    assert!(
        errs.iter().any(|e| matches!(
            e,
            ValidationError::PartIdUnrecorded { id } if id == "new"
        )),
        "{errs:?}"
    );
}

/// Baseline shape checks: duplicate `parts` entries, a `deprecated`
/// key still shipped, a replacement naming no live id, and the
/// file's own schema_version gate.
#[test]
fn malformed_baseline_fails() {
    let parts = loaded(&["a", "b"]);

    let mut b = baseline(&["a", "b"], &[]);
    b.parts.push("a".into());
    let errs = validate_id_stability(&parts, &b);
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::BadIdBaseline { .. })),
        "duplicate: {errs:?}"
    );

    // `deprecated` key still live.
    let b = baseline(&["a", "b"], &[("b", "a")]);
    let errs = validate_id_stability(&parts, &b);
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::BadIdBaseline { .. })),
        "still-live key: {errs:?}"
    );

    // Replacement names no live id — a retirement recorded only as
    // baseline-listing (`retired` stays in `parts` here so the only
    // defect is the dangling replacement).
    let b = baseline(&["a", "b", "retired"], &[("retired", "ghost")]);
    let errs = validate_id_stability(&parts, &b);
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::BadIdBaseline { .. })),
        "dangling replacement: {errs:?}"
    );

    // The baseline's own version is re-checked (serde enforced it at
    // load; a hand-rolled baseline gets the validator's copy).
    let mut b = baseline(&["a", "b"], &[]);
    b.schema_version = 99;
    let errs = validate_id_stability(&parts, &b);
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::SchemaVersion { found: 99, .. })),
        "{errs:?}"
    );
}

/// The plan's acceptance probe: delete one part file from a scratch
/// copy of `data/` and the gate must fire — `PartIdDisappeared` naming
/// the deleted id.
#[test]
fn deleted_part_file_fails_the_gate() {
    let root = std::env::temp_dir().join(format!("csp-idbase-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("parts")).unwrap();
    for e in std::fs::read_dir(data().join("parts")).unwrap() {
        let p = e.unwrap().path();
        std::fs::copy(&p, root.join("parts").join(p.file_name().unwrap())).unwrap();
    }
    std::fs::copy(data().join("id-baseline.ron"), root.join("id-baseline.ron")).unwrap();
    // Delete one part file — the baseline still names it.
    std::fs::remove_file(root.join("parts").join("csp.engine.ember.ron")).unwrap();
    let parts = load_parts_from(&csp_data::FsDataSource, &root.join("parts")).unwrap();
    let baseline = load_id_baseline(&root.join("id-baseline.ron"))
        .unwrap()
        .unwrap();
    let errs = validate_id_stability(&parts, &baseline);
    let _ = std::fs::remove_dir_all(&root);
    assert!(
        errs.iter().any(|e| matches!(
            e,
            ValidationError::PartIdDisappeared { id } if id == "csp.engine.ember"
        )),
        "{errs:?}"
    );
}

/// A missing baseline file loads as `None` — the gate is opt-in per
/// data set (the §6.3 demo's reduced tree ships none), not an error.
#[test]
fn missing_baseline_disables_the_gate() {
    let src = EmbeddedSource::new(&[]);
    let loaded = csp_data::load_id_baseline_from(&src, Path::new("data/id-baseline.ron")).unwrap();
    assert!(loaded.is_none());
}
