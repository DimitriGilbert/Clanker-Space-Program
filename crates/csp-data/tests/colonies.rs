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
    LocFile, ValidationError, load_colonies, load_recipes, load_resources, load_sites,
    validate_colonies, validate_colony_loc_keys, validate_recipe_loc_keys, validate_recipes,
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

// ---------------------------------------------------------------------------
// C32-F4: colony cross-field gates (synthetic fixtures — the shipped set is
// the green path covered above).
// ---------------------------------------------------------------------------

/// A colony at site "pad" (one ore yield) with a drill on it plus the
/// minimal resource set — every negative below mutates one piece.
fn cross_fixture() -> (
    csp_data::ColonyFile,
    csp_data::SiteFile,
    csp_data::ResourceFile,
    csp_data::LoadedRecipes,
) {
    let site = csp_data::SiteDef {
        schema_version: 0,
        id: "pad".into(),
        display_name: csp_data::LocKey::from("site.pad"),
        body: "terra".into(),
        latitude_rad: 0.0,
        longitude_rad: 0.0,
        altitude_m: 0.0,
        launch: true,
        resource_yields: vec![("ore".into(), 0.5)],
        starting_stock: Vec::new(),
    };
    let sites = csp_data::SiteFile {
        schema_version: csp_data::SITE_SCHEMA_VERSION,
        sites: vec![site],
    };
    let colony = csp_data::ColonyDef {
        site: "pad".into(),
        display_name: csp_data::LocKey::from("colony.pad"),
        modules: vec![
            csp_data::ColonyModuleDef::Drill {
                resource: "ore".into(),
                base_rate_per_s: 0.5,
                min_concentration: 0.5,
                power_ec_per_s: 1.0,
            },
            csp_data::ColonyModuleDef::Store {
                resource: "ore".into(),
                capacity: 100.0,
            },
            csp_data::ColonyModuleDef::Battery { capacity: 50.0 },
        ],
        stores: vec![
            csp_data::ColonyStoreInit {
                resource: "ore".into(),
                level: 10.0,
            },
            csp_data::ColonyStoreInit {
                resource: "electric_charge".into(),
                level: 20.0,
            },
        ],
    };
    let file = csp_data::ColonyFile {
        schema_version: csp_data::COLONY_SCHEMA_VERSION,
        colonies: vec![colony],
    };
    let res = csp_data::ResourceFile {
        schema_version: csp_data::RESOURCE_SCHEMA_VERSION,
        resources: ["ore", "metal", "electric_charge"]
            .iter()
            .map(|id| csp_data::ResourceDef {
                id: (*id).into(),
                display_name: csp_data::LocKey::from(format!("resource.{id}")),
                mass_per_unit_kg: 1.0,
            })
            .collect(),
    };
    let recipes = csp_data::LoadedRecipes { recipes: vec![] };
    (file, sites, res, recipes)
}

/// A founding `stores` row naming a resource NO `Store`/`Battery`
/// module holds clamps to 0 at apply — the founding stock silently
/// vanishes; the validator must flag it (C32-F4).
#[test]
fn store_without_capacity_is_rejected() {
    let (mut file, sites, res, recipes) = cross_fixture();
    assert!(validate_colonies(&file, &sites, &res, &recipes).is_empty());
    // "metal" stock with no Store module anywhere.
    file.colonies[0].stores.push(csp_data::ColonyStoreInit {
        resource: "metal".into(),
        level: 5.0,
    });
    let errs = validate_colonies(&file, &sites, &res, &recipes);
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::InvalidColony { reason, .. } if reason.contains("capacity"))),
        "{errs:?}"
    );
    // A zero-level row for a capacity-less resource is still dead
    // authored surface — the sim installs a 0-capacity row that can
    // never accumulate.
    file.colonies[0].stores[2].level = 0.0;
    let errs = validate_colonies(&file, &sites, &res, &recipes);
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::InvalidColony { reason, .. } if reason.contains("capacity"))),
        "{errs:?}"
    );
}

/// A founding level over the aggregate `Store` capacity clamps down
/// silently at apply — an authored mistake, flagged (C32-F4).
#[test]
fn over_capacity_store_level_is_rejected() {
    let (mut file, sites, res, recipes) = cross_fixture();
    file.colonies[0].stores[0].level = 150.0; // ore capacity is 100
    let errs = validate_colonies(&file, &sites, &res, &recipes);
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::InvalidColony { reason, .. } if reason.contains("exceeds"))),
        "{errs:?}"
    );
    // At capacity exactly is fine.
    file.colonies[0].stores[0].level = 100.0;
    assert!(validate_colonies(&file, &sites, &res, &recipes).is_empty());
}

/// A `Drill` on a resource the founding site's `resource_yields` does
/// not carry (or carries at zero) reads concentration 0 forever while
/// drawing EC — authored dead weight, flagged (C32-F4).
#[test]
fn drill_on_barren_ground_is_rejected() {
    let (mut file, sites, res, recipes) = cross_fixture();
    // Drill "metal": known resource, but the site yields only ore.
    file.colonies[0].modules[0] = csp_data::ColonyModuleDef::Drill {
        resource: "metal".into(),
        base_rate_per_s: 0.5,
        min_concentration: 0.5,
        power_ec_per_s: 1.0,
    };
    let errs = validate_colonies(&file, &sites, &res, &recipes);
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::InvalidColony { reason, .. } if reason.contains("yield"))),
        "{errs:?}"
    );
    // A zero yield is equally barren.
    let (mut file, mut sites, res, recipes) = cross_fixture();
    sites.sites[0].resource_yields.push(("metal".into(), 0.0));
    file.colonies[0].modules[0] = csp_data::ColonyModuleDef::Drill {
        resource: "metal".into(),
        base_rate_per_s: 0.5,
        min_concentration: 0.5,
        power_ec_per_s: 1.0,
    };
    let errs = validate_colonies(&file, &sites, &res, &recipes);
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::InvalidColony { reason, .. } if reason.contains("yield"))),
        "{errs:?}"
    );
}
