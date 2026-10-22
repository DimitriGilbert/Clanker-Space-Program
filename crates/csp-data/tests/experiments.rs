//! M7-A §3.2 gate test over the *authored* experiment set (PLAN §8.2's
//! "Data validators" row): `data/experiments/` must load and validate
//! green — id ↔ file-stem agreement, unique ids, `transmit_ratio` in
//! band, and non-overlapping `(body, situation)` cap coverage. This is
//! what keeps `validate_experiments` live: the validator runs against
//! the real files here, not a synthetic fixture.

#![forbid(unsafe_code)]
// Tests signal failure by panicking — §2.13's no-panic rule is for the
// shipping fallible paths, so unwrap/expect are allowed in test code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap_or_else(|_| Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

/// The authored set loads, validates green, and is non-empty (an empty
/// directory would read as "no experiments" — legal per the loader, but
/// the seeded `exp.materials_study` must be there for M7-A).
#[test]
fn authored_experiments_load_and_validate_green() {
    let loaded = csp_data::load_experiments(&repo_root().join("data/experiments")).unwrap();
    assert!(
        !loaded.experiments.is_empty(),
        "data/experiments/ must seed at least one experiment"
    );
    let errs = csp_data::validate_experiments(&loaded);
    assert!(errs.is_empty(), "validator errors: {errs:#?}");
}

/// The seeded def answers `cap_for` coherently — the sim's yield path
/// depends on every cap entry being reachable through it.
#[test]
fn seeded_experiment_caps_resolve() {
    let loaded = csp_data::load_experiments(&repo_root().join("data/experiments")).unwrap();
    for le in &loaded.experiments {
        let def = &le.def;
        // `cap_for` hands back exactly the authored entry — spot-check
        // the (body, situation) reachability the validator proved
        // non-overlapping.
        for cap in &def.caps {
            for s in cap.situations.situations() {
                assert_eq!(
                    def.cap_for(&cap.body, s),
                    Some(cap.cap_data_units),
                    "{}: ({} , {s:?}) cap unreachable",
                    def.id,
                    cap.body,
                );
            }
        }
    }
}
