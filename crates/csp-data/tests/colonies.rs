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
    validate_colony_loc_keys, validate_recipe_loc_keys, validate_recipes,
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
/// — the `validate_recipe_loc_keys` gate.
#[test]
fn shipping_recipe_loc_keys_resolve() {
    let recipes = load_recipes(&data().join("recipes")).unwrap();
    let loc = en_loc();
    let errs = validate_recipe_loc_keys(&recipes, &loc);
    assert!(errs.is_empty(), "unresolved recipe loc keys: {errs:#?}");
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

/// Every authored site's `display_name` LocKey resolves in
/// `data/loc/en.ron` — the `validate_site_loc_keys` gate.
#[test]
fn home_site_loc_keys_resolve() {
    let sites = load_sites(&home_dir().join("sites.ron")).unwrap();
    let loc = en_loc();
    let errs = csp_data::validate_site_loc_keys(&sites, &loc);
    assert!(errs.is_empty(), "unresolved site loc keys: {errs:#?}");
}

/// Every authored colony's `display_name` LocKey resolves in
/// `data/loc/en.ron` — the `validate_colony_loc_keys` gate.
#[test]
fn home_colony_loc_keys_resolve() {
    let file = load_colonies(&home_dir().join("colonies.ron")).unwrap();
    let loc = en_loc();
    let errs = validate_colony_loc_keys(&file, &loc);
    assert!(errs.is_empty(), "unresolved colony loc keys: {errs:#?}");
}

/// Fix-plan 3.1's dead-recipe gate: `data/recipes/recipe.composite.ron`
/// ships but is dead data unless an authored `Converter` names it
/// (colony modules are fixed at founding — no code path adds one
/// later). Pin that the authored Rask colony runs it, and that its
/// `composite` output has a `Store` to land in — the §3.5.1 sink rule
/// pins a storeless-output converter at fraction 0.
#[test]
fn authored_colony_runs_recipe_composite() {
    let file = load_colonies(&home_dir().join("colonies.ron")).unwrap();
    let rask = file
        .colonies
        .iter()
        .find(|c| c.site.as_str() == "rask-mines")
        .expect("colonies.ron must author the rask-mines colony");
    assert!(
        rask.modules.iter().any(|m| matches!(
            m,
            csp_data::ColonyModuleDef::Converter { recipe, .. }
                if recipe.as_str() == "recipe.composite"
        )),
        "rask-mines must author a Converter on recipe.composite — \
         the recipe is dead data otherwise: {:?}",
        rask.modules
    );
    assert!(
        rask.modules.iter().any(|m| matches!(
            m,
            csp_data::ColonyModuleDef::Store { resource, capacity }
                if resource.as_str() == "composite" && *capacity > 0.0
        )),
        "rask-mines needs a composite Store — the §3.5.1 sink rule \
         pins the converter at 0 without one: {:?}",
        rask.modules
    );
    // And the shipped recipe table must actually contain it — the
    // converter's RecipeId resolves against `load_recipes`.
    let recipes = load_recipes(&data().join("recipes")).unwrap();
    assert!(
        recipes
            .recipes
            .iter()
            .any(|r| r.def.id.as_str() == "recipe.composite"),
        "data/recipes lost recipe.composite while a colony consumes it"
    );
}
