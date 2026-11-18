//! §3.5 `sites.ron` validator tests (C32-F5): the file-level launch-site
//! floor and negative coverage for the W2.6 `starting_stock` rows —
//! `validate_site_stock` shipped without a single negative test.

#![forbid(unsafe_code)]
// Tests signal failure by panicking — §2.13's no-panic rule is for the
// shipping fallible paths, so unwrap/expect are allowed in test code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use csp_data::{
    BodyDef, LocKey, ResourceDef, ResourceFile, SiteDef, SiteFile, TerrainDef, ValidationError,
    load_sites, load_system, validate_sites,
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

/// A minimal system the site fixtures validate against: one rotating
/// star body named "terra" (rotation_period `Some` keeps
/// `fixed_sun_cos_margin` out of the way — the terminator rule has its
/// own tests).
fn sys() -> csp_data::LoadedSystem {
    let body = csp_data::LoadedBody {
        def: BodyDef {
            schema_version: csp_data::BODY_SCHEMA_VERSION,
            id: "terra".into(),
            display_name: "body.terra.name".into(),
            parent: None,
            radius_m: 1.0e6,
            mu_m3_per_s2: 1.0e14,
            soi_m: None,
            rotation_period_s: Some(21_600.0),
            tilt_rad: 0.0,
            orbit: None,
            atmosphere: None,
            ocean: None,
            terrain: TerrainDef {
                noise_seed: 0,
                heightmap: None,
                amplitude_frac: 0.0, // the star is flat
                relief: Default::default(),
            },
            biome_map: None,
            surface_resources: Vec::new(),
            insolation_w_m2: Some(1361.0),
        },
        biomes: None,
    };
    csp_data::LoadedSystem {
        system: csp_data::SystemDef {
            schema_version: csp_data::SCHEMA_VERSION,
            id: "test".into(),
            star: "terra".into(),
            bodies: vec!["terra".into()],
            epoch_note: "k".into(),
        },
        bodies: vec![body],
        orphans: Vec::new(),
    }
}

fn resources() -> ResourceFile {
    ResourceFile {
        schema_version: csp_data::RESOURCE_SCHEMA_VERSION,
        resources: vec![ResourceDef {
            id: "metal".into(),
            display_name: "resource.metal".into(),
            mass_per_unit_kg: 1.0,
        }],
    }
}

fn site(id: &str, launch: bool) -> SiteDef {
    SiteDef {
        schema_version: 0,
        id: id.into(),
        display_name: LocKey::from(format!("site.{id}")),
        body: "terra".into(),
        latitude_rad: 0.0,
        longitude_rad: 0.0,
        altitude_m: 0.0,
        launch,
        resource_yields: Vec::new(),
        starting_stock: Vec::new(),
    }
}

fn file(sites: Vec<SiteDef>) -> SiteFile {
    SiteFile {
        schema_version: csp_data::SITE_SCHEMA_VERSION,
        sites,
    }
}

/// C32-F5: a non-empty `sites.ron` with no `launch: true` site must not
/// validate — deleting the pad row used to pass green while every
/// `ManufactureCraft` order failed at runtime with a message that never
/// named the missing pad. An intentionally site-less system authors an
/// EMPTY list instead.
#[test]
fn non_empty_sites_need_a_launch_site() {
    // The floor only fires on a non-empty set: an authored site-less
    // system (empty list) is legal.
    let empty = file(vec![]);
    assert!(validate_sites(&empty, &sys(), &resources()).is_empty());

    let no_pad = file(vec![site("a", false), site("b", false)]);
    let errs = validate_sites(&no_pad, &sys(), &resources());
    assert!(
        errs.iter().any(|e| matches!(
            e,
            ValidationError::InvalidSite { id, reason }
                if id.is_empty() && reason.contains("launch")
        )),
        "{errs:?}"
    );

    let pad = file(vec![site("a", false), site("pad", true)]);
    assert!(validate_sites(&pad, &sys(), &resources()).is_empty());
}

/// W2.6 `starting_stock` negatives (C32-F5's test gap): duplicated
/// resources, unknown resources, and negative/non-finite units all
/// report `InvalidSite`.
#[test]
fn starting_stock_negatives_are_rejected() {
    let res = resources();
    for (label, stock) in [
        (
            "duplicate",
            vec![("metal".into(), 1.0), ("metal".into(), 2.0)],
        ),
        ("unknown", vec![("unobtainium".into(), 1.0)]),
        ("negative", vec![("metal".into(), -1.0)]),
        ("non-finite", vec![("metal".into(), f64::NAN)]),
    ] {
        let mut s = site("pad", true);
        s.starting_stock = stock;
        let errs = validate_sites(&file(vec![s]), &sys(), &res);
        assert!(
            errs.iter()
                .any(|e| matches!(e, ValidationError::InvalidSite { .. })),
            "{label}: {errs:?}"
        );
    }
    // …and the clean row validates.
    let mut s = site("pad", true);
    s.starting_stock = vec![("metal".into(), 100.0)];
    assert!(validate_sites(&file(vec![s]), &sys(), &res).is_empty());
}

/// The shipped home-system sites must keep validating green under the
/// new floor (terrin-launch carries `launch: true`).
#[test]
fn shipping_sites_validate_green() {
    let loaded_sys = load_system(&home_dir()).unwrap();
    let sites = load_sites(&home_dir().join("sites.ron")).unwrap();
    let res = csp_data::load_resources(&data().join("resources.ron")).unwrap();
    let errs = validate_sites(&sites, &loaded_sys, &res);
    assert!(errs.is_empty(), "validator errors: {errs:#?}");
}
