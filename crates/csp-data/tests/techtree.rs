//! The `data/techtree/` shipping-tree gate (PLAN §3.3, §8.2): loads
//! the real tree and asserts the validator's structural rules hold
//! against the real part set — no cycles, every part's
//! `tech_requirement` resolvable (the starter nodes cover the starter
//! parts), every track family/stat sane, and every loc key resolves.

#![forbid(unsafe_code)]
// Tests signal failure by panicking — §2.13's no-panic rule is for the
// shipping fallible paths, so unwrap/expect are allowed in test code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use csp_data::{
    LocFile, load_experiments, load_parts, load_recipes, load_resources, load_techtree,
    validate_parts, validate_techtree, validate_techtree_loc_keys,
};

fn data_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data")
}

#[test]
fn shipping_techtree_is_valid_against_the_part_set() {
    let root = data_root();
    let tree = load_techtree(&root.join("techtree")).unwrap();
    assert!(!tree.trees.is_empty(), "expected data/techtree/*.ron files");
    let parts = load_parts(&root.join("parts")).unwrap();
    let resources = load_resources(&root.join("resources.ron")).unwrap();
    // The part set itself must be clean before the tree gate means
    // anything.
    let recipes = load_recipes(&root.join("recipes")).unwrap();
    let experiments = load_experiments(&root.join("experiments")).unwrap();
    assert!(validate_parts(&parts, &resources, &recipes, &experiments).is_empty());
    let errs = validate_techtree(&tree, &parts);
    assert!(errs.is_empty(), "techtree errors: {errs:#?}");
}

#[test]
fn shipping_techtree_loc_keys_resolve() {
    let root = data_root();
    let tree = load_techtree(&root.join("techtree")).unwrap();
    let loc: LocFile =
        ron::from_str(&std::fs::read_to_string(root.join("loc/en.ron")).unwrap()).unwrap();
    let errs = validate_techtree_loc_keys(&tree, &loc);
    assert!(errs.is_empty(), "unresolved techtree loc keys: {errs:#?}");
}
