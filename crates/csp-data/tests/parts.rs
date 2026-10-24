//! M3 data-layer gate tests over the *authored* part set (PLAN §3.1,
//! §3.3, §3.4; §8.2 "Data validators" row): `data/parts/` +
//! `data/resources.ron` must load, validate green, resolve every loc
//! key, and — the foundation for M3's live Δv readout — produce a sane
//! Tsiolkovsky budget when stacked into the craft the set is tuned for.

#![forbid(unsafe_code)]
// Tests signal failure by panicking — §2.13's no-panic rule is for the
// shipping fallible paths, so unwrap/expect are allowed in test code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use csp_data::{
    G0, LoadedExperiments, LoadedParts, LoadedRecipes, LocFile, ModuleDef, PartDef, ResourceFile,
    load_experiments, load_parts, load_recipes, load_resources, validate_part_loc_keys,
    validate_parts, validate_resources,
};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap_or_else(|_| Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

fn parts() -> LoadedParts {
    load_parts(&repo_root().join("data/parts")).unwrap()
}

fn resources() -> ResourceFile {
    load_resources(&repo_root().join("data/resources.ron")).unwrap()
}

fn recipes() -> LoadedRecipes {
    load_recipes(&repo_root().join("data/recipes")).unwrap()
}

fn experiments() -> LoadedExperiments {
    load_experiments(&repo_root().join("data/experiments")).unwrap()
}

fn def<'a>(loaded: &'a LoadedParts, id: &str) -> &'a PartDef {
    loaded
        .parts
        .iter()
        .map(|p| &p.def)
        .find(|d| d.id.as_str() == id)
        .unwrap()
}

/// Wet mass of one tank module: `Σ units · mass_per_unit_kg` through
/// `resources.ron` — the same conversion the effective-stat resolver
/// will perform.
fn propellant_mass(def: &PartDef, res: &ResourceFile) -> f64 {
    def.modules
        .iter()
        .map(|m| match m {
            ModuleDef::Tank { capacity } => capacity
                .iter()
                .map(|(r, units)| units * res.mass_per_unit(r).unwrap())
                .sum(),
            _ => 0.0,
        })
        .sum()
}

/// The engine module's `(thrust_vac_n, isp_vac_s, isp_sl_s)` — tests
/// over the authored set only ever look at single-engine parts (the
/// validator forbids more than one).
fn engine_stats(def: &PartDef) -> (f64, f64, f64) {
    def.modules
        .iter()
        .find_map(|m| match m {
            ModuleDef::Engine {
                thrust_vac_n,
                isp_vac_s,
                isp_sl_s,
                ..
            } => Some((*thrust_vac_n, *isp_vac_s, *isp_sl_s)),
            _ => None,
        })
        .unwrap()
}

/// The authored resource registry validates green on its own.
#[test]
fn resources_validate_green() {
    let errs = validate_resources(&resources());
    assert!(errs.is_empty(), "validator errors: {errs:#?}");
}

/// §3.1/§8.2: the authored part set loads and passes every validator
/// rule, including resource references against `resources.ron`.
#[test]
fn authored_parts_load_and_validate_green() {
    let loaded = parts();
    let res = resources();
    assert_eq!(loaded.parts.len(), 10, "the M3 starter set is 10 parts");
    let errs = validate_parts(&loaded, &res, &recipes(), &experiments());
    assert!(errs.is_empty(), "validator errors: {errs:#?}");
}

/// Every LocKey the part set + resource registry reference must
/// resolve in `data/loc/en.ron`.
#[test]
fn part_loc_keys_resolve() {
    let loc_path = repo_root().join("data/loc/en.ron");
    let text = std::fs::read_to_string(&loc_path).unwrap();
    let loc: LocFile = ron::de::from_str(&text).unwrap();
    let errs = validate_part_loc_keys(&parts(), &resources(), &loc);
    assert!(errs.is_empty(), "unresolved loc keys: {errs:#?}");
}

/// Δv coherence — the reason the numbers were authored the way they
/// are. The starter stack `pod + s320 + sparc` is the M3 equivalent of
/// the M2 flight fixture's 200 kN / 5 t craft: this test computes
/// Tsiolkovsky *from the authored files* (wet mass via
/// `resources.ron`, Δv via `g₀·Isp·ln(m₀/m₁)`) and asserts the stack
/// lands in the band §3.0's LEO anchor (2 287 m/s circular at 75 km)
/// implies once launch losses are paid.
///
/// The numbers, worked: wet 800+440+900+3600 = 5 740 kg, dry 2 140 kg;
/// `ln(5740/2140) ≈ 0.986`; `320 · 9.81 · 0.986 ≈ 3 096 m/s` vacuum.
#[test]
fn starter_stack_delta_v_is_sane() {
    let loaded = parts();
    let res = resources();
    let pod = def(&loaded, "csp.pod.mk1");
    let tank = def(&loaded, "csp.tank.s320");
    let engine = def(&loaded, "csp.engine.sparc");

    let prop = propellant_mass(tank, &res);
    let m0 = pod.mass_dry_kg + tank.mass_dry_kg + engine.mass_dry_kg + prop;
    let m1 = m0 - prop;
    let (thrust_vac, isp_vac, isp_sl) = engine_stats(engine);

    // Vacuum Δv: enough for Terrin LEO (2 287 m/s + losses), not so
    // much that staging is pointless.
    let dv_vac = isp_vac * G0 * libm::log(m0 / m1);
    assert!(
        (2_800.0..=3_400.0).contains(&dv_vac),
        "single-stage vacuum Δv {dv_vac:.0} m/s outside the LEO-coherent band"
    );

    // Sea-level TWR at liftoff: the M2 fixture flew at ~4; anything in
    // [1.2, 5] is a sane launch stack.
    let twr = thrust_vac / (m0 * G0);
    assert!(
        (1.2..=5.0).contains(&twr),
        "liftoff TWR {twr:.2} outside [1.2, 5]"
    );

    // Burn time sanity: mass flow F/(Isp·g₀) ⇒ a launch stack that
    // empties in ~55–80 s at sea-level Isp is coherent with the 75 km
    // ascent the fixture demonstrated.
    let mdot_sl = thrust_vac / (isp_sl * G0);
    let burn_s = prop / mdot_sl;
    assert!(
        (40.0..=100.0).contains(&burn_s),
        "sea-level burn {burn_s:.0} s outside [40, 100]"
    );
}

/// The two-stage stack the set is *for*: the same math as above, with
// `s160 + ember` as the upper stage riding on `s320 + sparc`. Total Δv
/// must clear the LEO budget with the margin a starter rocket should
/// have.
#[test]
fn two_stage_stack_reaches_leo_budget() {
    let loaded = parts();
    let res = resources();
    let pod = def(&loaded, "csp.pod.mk1");

    // Stage 2: pod + s160 + ember.
    let s160 = def(&loaded, "csp.tank.s160");
    let ember = def(&loaded, "csp.engine.ember");
    let prop2 = propellant_mass(s160, &res);
    let m0_2 = pod.mass_dry_kg + s160.mass_dry_kg + ember.mass_dry_kg + prop2;
    let dv2 = engine_stats(ember).1 * G0 * libm::log(m0_2 / (m0_2 - prop2));

    // Stage 1: s320 + sparc lifting the whole stage 2 + a decoupler.
    let s320 = def(&loaded, "csp.tank.s320");
    let sparc = def(&loaded, "csp.engine.sparc");
    let decoupler = def(&loaded, "csp.decoupler.s125");
    let prop1 = propellant_mass(s320, &res);
    let m0_1 = m0_2 + s320.mass_dry_kg + sparc.mass_dry_kg + decoupler.mass_dry_kg + prop1;
    let dv1 = engine_stats(sparc).1 * G0 * libm::log(m0_1 / (m0_1 - prop1));

    let total = dv1 + dv2;
    assert!(
        (4_000.0..=6_000.0).contains(&total),
        "two-stage Δv {total:.0} m/s (s1 {dv1:.0} + s2 {dv2:.0}) — expected a comfortable LEO margin"
    );
}

/// Schema-shape checks that don't need a broken file: the enum surface
/// the resolver will consume exists on the authored set (engine stats,
/// variant multipliers, situation mask on the experiment).
#[test]
fn authored_set_exercises_the_schema() {
    let loaded = parts();
    let sparc = def(&loaded, "csp.engine.sparc");
    // The qualified variant is the §3.3 cost/quality trade.
    let qualified = sparc
        .variants
        .iter()
        .find(|v| v.id.as_str() == "qualified")
        .unwrap();
    assert!(qualified.cost_multiplier > 1.0);
    assert!(!qualified.stat_multipliers.is_empty());

    // The canister's situation mask is non-empty and carries space.
    let canister = def(&loaded, "csp.science.canister");
    let mask = canister
        .modules
        .iter()
        .find_map(|m| match m {
            ModuleDef::Experiment { situations, .. } => Some(*situations),
            _ => None,
        })
        .unwrap();
    assert!(!mask.is_empty());
    assert_ne!(mask.0 & csp_data::SituationMask::SPACE_LOW.0, 0);

    // Every part has at least one attach node and a stock variant —
    // the editor's minimum viable shape.
    for p in &loaded.parts {
        assert!(!p.def.attach_nodes.is_empty(), "{}", p.def.id);
        assert!(
            p.def.variants.iter().any(|v| v.id.as_str() == "stock"),
            "{} lacks a stock variant",
            p.def.id
        );
    }

    // Resources resolve to *something* mass-wise except charge.
    let res = resources();
    let mut masses = BTreeMap::new();
    for r in &res.resources {
        masses.insert(r.id.as_str(), r.mass_per_unit_kg);
    }
    assert_eq!(masses["electric_charge"], 0.0);
    assert!(masses["liquid_fuel"] > 0.0);
}
