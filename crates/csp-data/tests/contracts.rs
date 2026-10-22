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
) {
    let root = repo_root();
    let contracts = csp_data::load_contracts(&root.join("data/contracts")).unwrap();
    let sys = csp_data::load_system(&root.join("data/systems/home")).unwrap();
    let experiments = csp_data::load_experiments(&root.join("data/experiments")).unwrap();
    let tree = csp_data::load_techtree(&root.join("data/techtree")).unwrap();
    (contracts, sys, experiments, tree)
}

/// Every authored template parses at the current schema, passes the
/// structural validator, and resolves its loc keys — including the
/// tree's `contract_types` unlocks naming real templates.
#[test]
fn contract_templates_validate_against_shipping_data() {
    let (contracts, sys, experiments, tree) = loaded();
    assert_eq!(contracts.contracts.len(), 5);
    let errors = csp_data::validate_contracts(&contracts, &sys, &experiments, &tree);
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
