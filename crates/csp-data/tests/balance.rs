//! `data/balance.ron` loads green — the §8.3 tuning file's file-shape gate.

#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap_or_else(|_| Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

/// The authored balance file parses, versions correctly, and carries the
/// §2.6.5 event budget the sim mirrors.
#[test]
fn balance_ron_loads_and_carries_the_event_budget() {
    let b = csp_data::load_balance(&repo_root().join("data/balance.ron")).unwrap();
    assert_eq!(b.schema_version, csp_data::BALANCE_SCHEMA_VERSION);
    assert_eq!(b.advance_to_max_events_per_frame, 2_000);
    assert!(b.bubble_radius_m > 0.0);
}

fn resources() -> csp_data::ResourceFile {
    csp_data::load_resources(&repo_root().join("data/resources.ron")).unwrap()
}

/// The authored file passes the semantic validator — the finiteness/
/// range gates over the ~45 tunables and the `reliability_bench_resource`
/// cross-ref against `resources.ron`.
#[test]
fn balance_ron_validates_green() {
    let b = csp_data::load_balance(&repo_root().join("data/balance.ron")).unwrap();
    let errs = csp_data::validate_balance(&b, &resources());
    assert!(errs.is_empty(), "validator errors: {errs:#?}");
}

/// `reliability_bench_resource` must resolve in `resources.ron` — the
/// bench run reads the site inventory through it, so an unresolved id
/// would silently charge nothing.
#[test]
fn balance_unknown_bench_resource_is_rejected() {
    let mut b = csp_data::load_balance(&repo_root().join("data/balance.ron")).unwrap();
    b.reliability_bench_resource = "unobtainium".into();
    let errs = csp_data::validate_balance(&b, &resources());
    assert!(
        errs.iter().any(|e| matches!(
            e,
            csp_data::ValidationError::UnknownResource { resource, .. } if resource == "unobtainium"
        )),
        "{errs:#?}"
    );
}

/// Version discipline: a stale `schema_version` reports as
/// `SchemaVersion` (here through the validator; the loader's prescan
/// covers the file side).
#[test]
fn balance_stale_schema_version_is_rejected() {
    let mut b = csp_data::load_balance(&repo_root().join("data/balance.ron")).unwrap();
    b.schema_version = 1;
    let errs = csp_data::validate_balance(&b, &resources());
    assert!(
        errs.iter().any(|e| matches!(
            e,
            csp_data::ValidationError::SchemaVersion {
                found: 1,
                expected: csp_data::BALANCE_SCHEMA_VERSION,
                ..
            }
        )),
        "{errs:#?}"
    );
}

/// The ranged gates: `recovery_efficiency` outside `[0,1]` and a
/// `dock_soft_pull_per_s` past the soft-capture Euler-stability bound
/// (`pull·dt < 2` at the pinned `1/64` s tick → `< 128`) both report.
#[test]
fn balance_out_of_range_tunables_are_rejected() {
    let mut b = csp_data::load_balance(&repo_root().join("data/balance.ron")).unwrap();
    b.recovery_efficiency = 1.5;
    let errs = csp_data::validate_balance(&b, &resources());
    assert!(
        errs.iter().any(|e| matches!(
            e,
            csp_data::ValidationError::BadBalance {
                field: "recovery_efficiency",
                ..
            }
        )),
        "{errs:#?}"
    );

    let mut b = csp_data::load_balance(&repo_root().join("data/balance.ron")).unwrap();
    b.dock_soft_pull_per_s = 128.0;
    let errs = csp_data::validate_balance(&b, &resources());
    assert!(
        errs.iter().any(|e| matches!(
            e,
            csp_data::ValidationError::BadBalance {
                field: "dock_soft_pull_per_s",
                ..
            }
        )),
        "{errs:#?}"
    );

    let mut b = csp_data::load_balance(&repo_root().join("data/balance.ron")).unwrap();
    b.science_softcap_k = f64::NAN;
    let errs = csp_data::validate_balance(&b, &resources());
    assert!(
        errs.iter().any(|e| matches!(
            e,
            csp_data::ValidationError::PartNonFinite {
                field: "science_softcap_k",
                ..
            }
        )),
        "{errs:#?}"
    );
}
