//! `data/contracts/*.ron` loads green — the §3.6 templates' file-
//! shape + structural gate against the shipping system, experiments,
//! tech tree, and loc table.

#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap_or_else(|_| Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

fn loaded() -> (
    csp_data::LoadedContracts,
    csp_data::LoadedSystem,
    csp_data::LoadedExperiments,
    csp_data::LoadedTechTree,
    csp_data::ResourceFile,
) {
    let root = repo_root();
    let contracts = csp_data::load_contracts(&root.join("data/contracts")).unwrap();
    let sys = csp_data::load_system(&root.join("data/systems/home")).unwrap();
    let experiments = csp_data::load_experiments(&root.join("data/experiments")).unwrap();
    let tree = csp_data::load_techtree(&root.join("data/techtree")).unwrap();
    let res = csp_data::load_resources(&root.join("data/resources.ron")).unwrap();
    (contracts, sys, experiments, tree, res)
}

/// Every authored template parses at the current schema, passes the
/// structural validator, and resolves its loc keys — including the
/// tree's `contract_types` unlocks naming real templates and the
/// resupply template's resource candidates resolving in
/// `resources.ron`.
#[test]
fn contract_templates_validate_against_shipping_data() {
    let (contracts, sys, experiments, tree, res) = loaded();
    assert_eq!(contracts.contracts.len(), 6);
    let errors = csp_data::validate_contracts(&contracts, &sys, &experiments, &tree, &res);
    assert!(errors.is_empty(), "{errors:?}");
}

/// The loc gate — every `display_name`/`description` resolves in
/// `en.ron`.
#[test]
fn contract_loc_keys_resolve() {
    let (contracts, ..) = loaded();
    let text = std::fs::read_to_string(repo_root().join("data/loc/en.ron")).unwrap();
    let loc: csp_data::LocFile = ron::de::from_str(&text).unwrap();
    let errors = csp_data::validate_contract_loc_keys(&contracts, &loc);
    assert!(errors.is_empty(), "{errors:?}");
}

/// §3.6 "candidate lists non-empty" — an empty `bodies` space is a
/// `BadContract`, not a silent pass that Fatals at the first offer
/// draw (R3-F1/R10-F6: `check_bodies`'s predecessor returned the
/// check as a `bool` and every call site discarded it).
#[test]
fn empty_body_space_is_a_validation_error() {
    let (mut contracts, sys, experiments, tree, res) = loaded();
    let sat = contracts
        .contracts
        .iter_mut()
        .find(|c| c.def.id.as_str() == "contract.satellite")
        .unwrap();
    let csp_data::ContractParamSpace::SatellitePlacement { bodies, .. } =
        &mut sat.def.generate.space
    else {
        panic!("contract.satellite is not a SatellitePlacement")
    };
    bodies.clear();
    let errors = csp_data::validate_contracts(&contracts, &sys, &experiments, &tree, &res);
    assert!(
        errors.iter().any(|e| matches!(
            e,
            csp_data::ValidationError::BadContract { contract, reason }
                if contract == "contract.satellite" && reason.contains("empty")
        )),
        "empty bodies must fail validation: {errors:?}"
    );
}

/// Point `contract.satellite`'s draw bands at `(peri, apo)` — the
/// mutation the cross-band test needs, scoped so the borrow ends
/// before each `validate_contracts` call.
fn set_satellite_bands(
    contracts: &mut csp_data::LoadedContracts,
    peri: (f64, f64),
    apo: (f64, f64),
) {
    let sat = contracts
        .contracts
        .iter_mut()
        .find(|c| c.def.id.as_str() == "contract.satellite")
        .unwrap();
    let csp_data::ContractParamSpace::SatellitePlacement {
        periapsis_alt_m,
        apoapsis_alt_m,
        ..
    } = &mut sat.def.generate.space
    else {
        panic!("contract.satellite is not a SatellitePlacement")
    };
    *periapsis_alt_m = peri;
    *apoapsis_alt_m = apo;
}

/// The satellite cross-band rule: `apoapsis_alt_m.hi` below
/// `periapsis_alt_m.lo` inverts every draw — the authored bands carry
/// none of their stated meaning, so the validator rejects the space
/// (R2-F3/R3-F12). Partial overlap stays legal — the shipping file
/// itself overlaps partially and validates green.
#[test]
fn fully_inverted_satellite_bands_are_a_validation_error() {
    let (mut contracts, sys, experiments, tree, res) = loaded();
    // apo ceiling (60 km) under the peri floor (100 km): fully
    // inverted — every draw comes out of the sim's pair-ordering swap.
    set_satellite_bands(&mut contracts, (100_000.0, 250_000.0), (50_000.0, 60_000.0));
    let errors = csp_data::validate_contracts(&contracts, &sys, &experiments, &tree, &res);
    assert!(
        errors.iter().any(|e| matches!(
            e,
            csp_data::ValidationError::BadContract { contract, reason }
                if contract == "contract.satellite" && reason.contains("inverts")
        )),
        "inverted bands must fail validation: {errors:?}"
    );

    // The boundary the rule deliberately does NOT cross: apo ceiling
    // equal to the peri floor is partial-overlap territory, still
    // legal (a draw can produce ra == rp).
    set_satellite_bands(
        &mut contracts,
        (100_000.0, 250_000.0),
        (50_000.0, 100_000.0),
    );
    let errors = csp_data::validate_contracts(&contracts, &sys, &experiments, &tree, &res);
    assert!(
        !errors.iter().any(|e| matches!(
            e,
            csp_data::ValidationError::BadContract { contract, .. }
                if contract == "contract.satellite"
        )),
        "boundary-touching bands must stay legal: {errors:?}"
    );
}
