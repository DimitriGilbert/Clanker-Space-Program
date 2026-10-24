//! The `deny_unknown_fields` gate (R2-F1): unknown RON keys must be
//! parse errors, never silently dropped. Misspelled `Option`/defaulted
//! fields — `insolation_w_m2`, `soi_m`, `resource_yields`,
//! `deadline_days` — used to deserialize as `None`/empty while the
//! typo sat dead in the file; now the loader fails loudly.
//!
//! Also covered: the sites v1→v2 compatibility path — a v1 file
//! *lacking* `resource_yields` must still parse (the field's
//! `#[serde(default)]` supplies the empty list; `deny_unknown_fields`
//! only rejects keys the schema does not declare).

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

fn data() -> PathBuf {
    repo_root().join("data")
}

/// Inject `bogus_typo_field` right after the file's first
/// `schema_version:` line — inside the top-level struct.
fn with_extra_key(text: &str) -> String {
    let (head, rest) = text
        .split_once("schema_version:")
        .expect("data file declares schema_version");
    let comma = rest.find(',').expect("schema_version line has a comma");
    let (ver, tail) = rest.split_at(comma + 1);
    format!("{head}schema_version:{ver}\n    bogus_typo_field: 0,{tail}")
}

/// Rename the first `field:` key to a misspelling — the "dead data"
/// path: the real field drops to its serde default while the typo
/// used to be ignored. The `:` anchor keeps header-comment mentions
/// of the field name out of the substitution.
fn misspelled(text: &str, field: &str) -> String {
    let key = format!("{field}:");
    assert!(
        text.contains(&key),
        "fixture text must contain {key} to misspell"
    );
    text.replacen(&key, &format!("{field}_typo:"), 1)
}

/// RON's unknown-key diagnostic names the field — assert the
/// rejection names the typo, not some unrelated parse failure.
fn assert_rejects<T: serde::de::DeserializeOwned>(path: &Path, doctored: &str) {
    let err = ron::de::from_str::<T>(doctored)
        .err()
        .unwrap_or_else(|| panic!("{}: doctored RON accepted", path.display()));
    assert!(
        err.to_string().contains("typo"),
        "{}: unexpected error text: {err}",
        path.display()
    );
}

/// `(path, parse-and-check)` — one entry per data-file schema.
type RejectCase = (PathBuf, fn(&str) -> Result<(), String>);

/// Every top-level data-file schema rejects an unknown key — one
/// representative file per loader and per def-file registry.
#[test]
fn unknown_top_level_keys_are_parse_errors() {
    let root = data();
    let cases: Vec<RejectCase> = vec![
        (root.join("balance.ron"), reject::<csp_data::BalanceDef>),
        (root.join("resources.ron"), reject::<csp_data::ResourceFile>),
        (root.join("loc/en.ron"), reject::<csp_data::LocFile>),
        (
            root.join("systems/home/system.ron"),
            reject::<csp_data::SystemDef>,
        ),
        (
            root.join("systems/home/terrin/body.ron"),
            reject::<csp_data::BodyDef>,
        ),
        (
            root.join("systems/home/terrin/biomes.ron"),
            reject::<csp_data::BiomeLegend>,
        ),
        (
            root.join("systems/home/sites.ron"),
            reject::<csp_data::SiteFile>,
        ),
        (
            root.join("systems/home/colonies.ron"),
            reject::<csp_data::ColonyFile>,
        ),
        (
            root.join("experiments/exp.materials_study.ron"),
            reject::<csp_data::ExperimentDef>,
        ),
        (
            root.join("contracts/contract.satellite.ron"),
            reject::<csp_data::ContractTemplate>,
        ),
        (
            root.join("recipes/recipe.smelt_ore.ron"),
            reject::<csp_data::RecipeDef>,
        ),
        (
            root.join("techtree/home.ron"),
            reject::<csp_data::TechTreeDef>,
        ),
        (
            root.join("parts/csp.engine.sparc.ron"),
            reject::<csp_data::PartDef>,
        ),
    ];
    for (path, reject) in cases {
        let text = std::fs::read_to_string(&path).unwrap();
        reject(&with_extra_key(&text)).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    }
}

/// `reject::<T>` parses `doctored` and reports why it should have
/// failed — kept as a `fn` so the case table stays declarative.
fn reject<T: serde::de::DeserializeOwned>(doctored: &str) -> Result<(), String> {
    match ron::de::from_str::<T>(doctored) {
        Ok(_) => Err("doctored RON parsed without error".to_string()),
        Err(e) if e.to_string().contains("bogus_typo_field") => Ok(()),
        Err(e) => Err(format!("rejected, but not for the typo: {e}")),
    }
}

/// A misspelled `#[serde(default)]` field (`resource_yields` inside a
/// site entry) is a parse error — before `deny_unknown_fields` the
/// yields silently read as empty.
#[test]
fn misspelled_defaulted_site_field_is_a_parse_error() {
    let path = data().join("systems/home/sites.ron");
    let text = std::fs::read_to_string(&path).unwrap();
    assert_rejects::<csp_data::SiteFile>(&path, &misspelled(&text, "resource_yields"));
}

/// A misspelled `Option` field (`deadline_days` on a contract
/// template) is a parse error — before `deny_unknown_fields` the
/// deadline silently read as `None`.
#[test]
fn misspelled_optional_contract_field_is_a_parse_error() {
    let path = data().join("contracts/contract.satellite.ron");
    let text = std::fs::read_to_string(&path).unwrap();
    assert_rejects::<csp_data::ContractTemplate>(&path, &misspelled(&text, "deadline_days"));
}

/// A misspelled `Option` field on `BodyDef` (`soi_m`) is a parse
/// error — before `deny_unknown_fields` the SOI silently read as
/// `None` (infinite).
#[test]
fn misspelled_optional_body_field_is_a_parse_error() {
    let path = data().join("systems/home/terrin/body.ron");
    let text = std::fs::read_to_string(&path).unwrap();
    assert_rejects::<csp_data::BodyDef>(&path, &misspelled(&text, "soi_m"));
}

/// A misspelled `Option` field *inside an enum's struct variant*
/// (`ignitions` inside `ModuleDef::Engine`) is a parse error —
/// `deny_unknown_fields` on the enum covers its variants' field sets,
/// so the typo can no longer deserialize `ignitions` as `None` while
/// the misspelled key sits dead in the file.
#[test]
fn misspelled_engine_module_field_is_a_parse_error() {
    let path = data().join("parts/csp.engine.sparc.ron");
    let text = std::fs::read_to_string(&path).unwrap();
    assert_rejects::<csp_data::PartDef>(&path, &misspelled(&text, "ignitions"));
}

/// v1 `sites.ron` (no `resource_yields`) still parses —
/// `deny_unknown_fields` only rejects *undeclared* keys; the
/// `#[serde(default)]` on the v2 field supplies the empty list. The
/// frozen `flight-launch` fixture is the real v1 file.
#[test]
fn v1_sites_file_still_parses() {
    let sites =
        csp_data::load_sites(&data().join("test-fixtures/flight-launch/sites.ron")).unwrap();
    assert!(!sites.sites.is_empty());
    for s in &sites.sites {
        assert!(
            s.resource_yields.is_empty(),
            "v1 site {} gained yields from nowhere",
            s.id
        );
    }
}
