//! M1 gate tests over the *authored* data set (PLAN §3.0, §8.2 "Data
//! validators" row): `data/systems/home/` must load, validate green, and
//! reproduce the sheet's derived numbers when computed FROM the loaded
//! defs — so the tests prove the data, not constants repeated in code.

#![forbid(unsafe_code)]
// Tests signal failure by panicking — §2.13's no-panic rule is for the
// shipping fallible paths, so unwrap/expect are allowed in test code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use csp_data::{
    BodyDef, LoadedSystem, LocFile, data_set_hash, inspect_indexed_png, load_system,
    validate_loc_keys, validate_system,
};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap_or_else(|_| Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

fn home() -> LoadedSystem {
    load_system(&repo_root().join("data/systems/home")).unwrap()
}

fn body<'a>(sys: &'a LoadedSystem, id: &str) -> &'a BodyDef {
    sys.bodies
        .iter()
        .map(|b| &b.def)
        .find(|d| d.id.as_str() == id)
        .unwrap()
}

fn rel_err(actual: f64, expected: f64) -> f64 {
    ((actual - expected) / expected).abs()
}

/// §3.0: the authored system loads and passes every validator rule.
/// This is the §8.2 "Data validators" CI row for system data.
#[test]
fn home_system_loads_and_validates_green() {
    let sys = home();
    assert_eq!(sys.system.id.as_str(), "home");
    assert_eq!(sys.system.star.as_str(), "solen");
    assert_eq!(sys.bodies.len(), 4);
    let errs = validate_system(&sys);
    assert!(errs.is_empty(), "validator errors: {errs:#?}");
}

/// Every LocKey the system references must resolve in `data/loc/en.ron`.
#[test]
fn home_system_loc_keys_resolve() {
    let sys = home();
    let loc_path = repo_root().join("data/loc/en.ron");
    let text = std::fs::read_to_string(&loc_path).unwrap();
    let loc: LocFile = ron::de::from_str(&text).unwrap();
    let errs = validate_loc_keys(&sys, &loc);
    assert!(errs.is_empty(), "unresolved loc keys: {errs:#?}");
}

/// §3.0 derived check: Terrin surface escape velocity
/// `sqrt(2μ/R)` = 3 431 m/s ±0.1%.
#[test]
fn terrin_escape_velocity() {
    let sys = home();
    let t = body(&sys, "terrin");
    let v = libm::sqrt(2.0 * t.mu_m3_per_s2 / t.radius_m);
    assert!(
        rel_err(v, 3431.0) <= 0.001,
        "escape velocity {v} m/s not within 0.1% of 3431"
    );
}

/// §3.0 derived check: Terrin 75 km circular orbit velocity
/// `sqrt(μ/(R+h))` = 2 287 m/s ±1% — the canonical "home LEO" number all
/// Δv budgets are tuned against.
#[test]
fn terrin_leo_velocity() {
    let sys = home();
    let t = body(&sys, "terrin");
    let v = libm::sqrt(t.mu_m3_per_s2 / (t.radius_m + 75_000.0));
    assert!(
        rel_err(v, 2287.0) <= 0.01,
        "LEO velocity {v} m/s not within 1% of 2287"
    );
}

/// §3.0 derived check: Rask orbital period = 1.390e5 s ±0.1%
/// (6.44 sim-days) — from the loaded elements AND from the closed form
/// over the loaded parent μ.
#[test]
fn rask_orbital_period() {
    let sys = home();
    let rask = body(&sys, "rask");
    let o = rask.orbit.as_ref().unwrap();
    assert!(
        rel_err(o.period_s, 1.390e5) <= 0.001,
        "authored period {} s not within 0.1% of 1.390e5",
        o.period_s
    );
    let terrin = body(&sys, "terrin");
    let a = o.semi_major_axis_m;
    let computed = 2.0 * core::f64::consts::PI * libm::sqrt(a * a * a / terrin.mu_m3_per_s2);
    assert!(
        rel_err(computed, 1.390e5) <= 0.001,
        "computed period {computed} s not within 0.1% of 1.390e5"
    );
    // 6.44 sim-days of 21 600 s, per §3.0.
    assert!(rel_err(o.period_s / 21_600.0, 6.44) <= 0.01);
}

/// §3.0.1: every referenced biome map is a real 8-bit palette PNG inside
/// the 2048×1024 cap, and the loader picked up its legend.
#[test]
fn biome_maps_are_real_indexed_pngs() {
    let sys = home();
    for (id, palette_len) in [("terrin", 7usize), ("rask", 4), ("duva", 4)] {
        let b = sys.bodies.iter().find(|b| b.def.id.as_str() == id).unwrap();
        let bm = b.biomes.as_ref().unwrap();
        let info = inspect_indexed_png(&bm.png).unwrap();
        assert_eq!((info.width, info.height), (64, 32), "{id}");
        assert_eq!(info.palette_len, palette_len, "{id}");
        assert_eq!(bm.legend.entries.len(), palette_len, "{id}");
    }
    // The star carries no biome map.
    assert!(body(&sys, "solen").biome_map.is_none());
}

/// §3.1: the shipping data-set hash is deterministic and covers the new
/// system data (biome PNGs included — `data/art/` extensions do not
/// apply outside `data/art/`). Two runs must agree; and the hash must
/// differ from one computed over a data tree missing the system — proven
/// here indirectly by hashing the real tree twice plus asserting it is
/// not the hash of an empty stream.
#[test]
fn data_set_hash_is_stable_and_nonempty() {
    let root = repo_root().join("data");
    let h1 = data_set_hash(&root).unwrap();
    let h2 = data_set_hash(&root).unwrap();
    assert_eq!(h1, h2, "data_set_hash must be deterministic");
    assert_ne!(h1, *blake3::hash(b"").as_bytes(), "empty-stream hash?");
}

/// Boundary check (§3.1): `data/test-fixtures/` is NOT part of the
/// shipping data-set hash — it is hashed separately as
/// `data_fixture_hash` by csp-bench. Prove it by hashing a copy of the
/// tree concept is overkill; instead assert the fixture dir's files
/// cannot perturb the hash: compute the hash, then verify that an
/// identical tree *without* test-fixtures hashes the same. We simulate
/// "without test-fixtures" by hashing a temp copy of `data/` that omits
/// the directory — cheap for a repo this size.
#[test]
fn data_set_hash_excludes_test_fixtures() {
    let root = repo_root().join("data");
    let with_fixtures = data_set_hash(&root).unwrap();

    // Copy data/ minus test-fixtures into a temp dir and re-hash.
    let tmp = std::env::temp_dir().join(format!("csp-data-hash-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    copy_without_fixtures(&root, &tmp);
    let without_fixtures = data_set_hash(&tmp).unwrap();
    let _ = std::fs::remove_dir_all(&tmp);
    assert_eq!(
        with_fixtures, without_fixtures,
        "test-fixtures must not enter the shipping data-set hash"
    );
}

fn copy_without_fixtures(src: &Path, dst: &Path) {
    for entry in std::fs::read_dir(src).unwrap().flatten() {
        let name = entry.file_name();
        if entry.path().is_dir() {
            if name == "test-fixtures" {
                continue;
            }
            let sub = dst.join(&name);
            std::fs::create_dir_all(&sub).unwrap();
            copy_without_fixtures(&entry.path(), &sub);
        } else {
            std::fs::copy(entry.path(), dst.join(&name)).unwrap();
        }
    }
}
