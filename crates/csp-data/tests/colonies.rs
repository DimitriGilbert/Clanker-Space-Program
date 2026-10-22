//! M8-B §3.5.1 gate tests over the *authored* colony and recipe data:
//! `data/systems/home/colonies.ron` and `data/recipes/` must load and
//! validate green against `sites.ron` + `resources.ron` — the §8.2
//! "Data validators" CI row for the RTS layer's authored surface (the
//! `home_system`/`parts` gate precedent). Without this gate the file
//! would be dead data: `load_colonies`/`load_recipes` had no shipping
//! caller until M8-B wired founding into `csp-view`/`csp-bench` world
//! builds.

#![forbid(unsafe_code)]
// Tests signal failure by panicking — §2.13's no-panic rule is for the
// shipping fallible paths, so unwrap/expect are allowed in test code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use csp_data::{
    LocFile, load_colonies, load_recipes, load_resources, load_sites, validate_colonies,
    validate_recipes,
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

fn home_dir() -> PathBuf {
    data().join("systems").join("home")
}

fn en_loc() -> LocFile {
    let text = std::fs::read_to_string(data().join("loc").join("en.ron")).unwrap();
    ron::de::from_str(&text).unwrap()
}

/// `data/recipes/*.ron`: the converter recipe registry loads, is
/// non-empty (a recipe-less shipping set would leave every authored
/// `Converter` module unresolvable), and passes `validate_recipes`
/// against the shipped resource table.
#[test]
fn shipping_recipes_load_and_validate_green() {
    let recipes = load_recipes(&data().join("recipes")).unwrap();
    assert!(
        !recipes.recipes.is_empty(),
        "data/recipes must ship at least one recipe"
    );
    let res = load_resources(&data().join("resources.ron")).unwrap();
    let errs = validate_recipes(&recipes, &res);
    assert!(errs.is_empty(), "recipe validator errors: {errs:#?}");
}

/// Every recipe's `display_name` LocKey resolves in `data/loc/en.ron`
/// (the `validate_part_loc_keys` contract — no dedicated validator
/// exists for recipes, so the gate asserts it directly).
#[test]
fn shipping_recipe_loc_keys_resolve() {
    let recipes = load_recipes(&data().join("recipes")).unwrap();
    let loc = en_loc();
    for r in &recipes.recipes {
        assert!(
            loc.contains(&r.def.display_name),
            "recipe {:?} display_name {:?} unresolved in en.ron",
            r.def.id,
            r.def.display_name
        );
    }
}

/// `data/systems/home/colonies.ron`: the authored starting colonies
/// load, are non-empty (the file exists to seed the session — an empty
/// set would be dead data), and pass `validate_colonies` against the
/// shipped sites, resources, and recipe tables.
#[test]
fn home_colonies_load_and_validate_green() {
    let file = load_colonies(&home_dir().join("colonies.ron")).unwrap();
    assert!(
        !file.colonies.is_empty(),
        "home colonies.ron must author at least one starting colony"
    );
    let sites = load_sites(&home_dir().join("sites.ron")).unwrap();
    let res = load_resources(&data().join("resources.ron")).unwrap();
    let recipes = load_recipes(&data().join("recipes")).unwrap();
    let errs = validate_colonies(&file, &sites, &res, &recipes);
    assert!(errs.is_empty(), "colony validator errors: {errs:#?}");
}

/// Every authored colony's `display_name` LocKey resolves in
/// `data/loc/en.ron`.
#[test]
fn home_colony_loc_keys_resolve() {
    let file = load_colonies(&home_dir().join("colonies.ron")).unwrap();
    let loc = en_loc();
    for c in &file.colonies {
        assert!(
            loc.contains(&c.display_name),
            "colony at site {:?} display_name {:?} unresolved in en.ron",
            c.site,
            c.display_name
        );
    }
}
