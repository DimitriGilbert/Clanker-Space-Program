//! Unit tests — one minimal-but-valid fixture per content kind; every
//! negative test mutates it so exactly one rule fires.

use super::dataset::check_surface_resources;
use super::*;
use crate::biome::BiomeLegendEntry;
use crate::ids::{BiomeId, LocKey, SystemId};
use crate::load::{LoadedBiomeMap, LoadedBody, LoadedPart, LoadedParts, LoadedSystem};
use crate::loc::LocFile;
use crate::resource::ResourceFile;
use crate::site::SiteFile;
use crate::system::{
    AtmosphereDef, BODY_SCHEMA_VERSION, BodyDef, KeplerElements, OceanDef, SCHEMA_VERSION,
    SystemDef, TemperatureKnot, TerrainDef,
};
use std::collections::BTreeMap;

/// A minimal but *valid* two-body system (star + one planet) — every
/// negative test mutates this fixture so exactly one rule fires.
fn fixture() -> LoadedSystem {
    let mu_star = 1.0e14;
    let a = 1.0e9;
    let period = 2.0 * core::f64::consts::PI * libm::sqrt(a * a * a / mu_star);
    let star = LoadedBody {
        def: BodyDef {
            schema_version: BODY_SCHEMA_VERSION,
            id: "star".into(),
            display_name: "body.star.name".into(),
            parent: None,
            radius_m: 1.0e6,
            mu_m3_per_s2: mu_star,
            soi_m: None,
            rotation_period_s: Some(100_000.0),
            tilt_rad: 0.0,
            orbit: None,
            atmosphere: None,
            ocean: None,
            terrain: TerrainDef {
                noise_seed: 1,
                heightmap: None,
            },
            biome_map: None,
            surface_resources: Vec::new(),
            insolation_w_m2: Some(1361.0),
        },
        biomes: None,
    };
    let planet = LoadedBody {
        def: BodyDef {
            schema_version: BODY_SCHEMA_VERSION,
            id: "planet".into(),
            display_name: "body.planet.name".into(),
            parent: Some("star".into()),
            radius_m: 1.0e5,
            mu_m3_per_s2: 1.0e11,
            soi_m: Some(1.0e8),
            rotation_period_s: None, // tidally locked is legal
            tilt_rad: 0.0,
            orbit: Some(KeplerElements {
                semi_major_axis_m: a,
                eccentricity: 0.0,
                inclination_rad: 0.0,
                ascending_node_rad: 0.0,
                arg_periapsis_rad: 0.0,
                mean_anomaly_epoch_rad: 0.0,
                period_s: period,
            }),
            atmosphere: None,
            ocean: Some(OceanDef {
                datum_radius_m: 1.0e5,
            }),
            terrain: TerrainDef {
                noise_seed: 2,
                heightmap: None,
            },
            biome_map: None,
            surface_resources: Vec::new(),
            insolation_w_m2: Some(100.0),
        },
        biomes: None,
    };
    LoadedSystem {
        system: SystemDef {
            schema_version: SCHEMA_VERSION,
            id: SystemId("test".into()),
            star: "star".into(),
            bodies: vec!["star".into(), "planet".into()],
            epoch_note: "system.test.epoch_note".into(),
        },
        bodies: vec![star, planet],
        orphans: Vec::new(),
    }
}

fn planet(sys: &mut LoadedSystem) -> &mut BodyDef {
    &mut sys.bodies[1].def
}

/// The fixture itself must be clean — otherwise the negative tests
/// below attribute nothing.
#[test]
fn fixture_is_valid() {
    assert!(validate_system(&fixture()).is_empty());
}

#[test]
fn soi_below_child_apoapsis_is_rejected() {
    let mut sys = fixture();
    // Star's SOI smaller than the planet's 1e9 m apoapsis.
    sys.bodies[0].def.soi_m = Some(5.0e8);
    let errs = validate_system(&sys);
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::SoiTooSmall { .. })),
        "{errs:?}"
    );
}

#[test]
fn periapsis_under_parent_surface_is_rejected() {
    let mut sys = fixture();
    // e ≈ 0.9999 on a = 1e9 puts periapsis at ~1e5 m — under the
    // star's 1e6 m radius (the period check keys on `a`, unchanged).
    planet(&mut sys).orbit.as_mut().unwrap().eccentricity = 0.9999;
    let errs = validate_system(&sys);
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::PeriapsisInsideParent { .. })),
        "{errs:?}"
    );
}

#[test]
fn non_star_soi_missing_is_rejected() {
    let mut sys = fixture();
    // `soi_m: None` on the planet reads as an infinite SOI — the
    // child-geometry rule then has nothing finite to check.
    planet(&mut sys).soi_m = None;
    let errs = validate_system(&sys);
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::SoiMissing { .. })),
        "{errs:?}"
    );
}

#[test]
fn own_soi_reaching_own_periapsis_is_rejected() {
    let mut sys = fixture();
    // 2e9 m SOI on a 1e9 m periapsis orbit — the star is inside the
    // planet's SOI at closest approach.
    planet(&mut sys).soi_m = Some(2.0e9);
    let errs = validate_system(&sys);
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::SoiExceedsOrbit { .. })),
        "{errs:?}"
    );
}

#[test]
fn overlapping_sibling_sois_are_rejected_once() {
    let mut sys = fixture();
    // A second child of the star on a disjoint outer shell:
    // [1e9] vs [1.5e9] — a 5e8 m gap. 1e8 + 5e8 SOIs exceed it.
    let mut outer = sys.bodies[1].clone();
    outer.def.id = "outer".into();
    outer.def.soi_m = Some(5.0e8);
    let a = 1.5e9;
    let mu_star = sys.bodies[0].def.mu_m3_per_s2;
    let o = outer.def.orbit.as_mut().unwrap();
    o.semi_major_axis_m = a;
    o.period_s = 2.0 * core::f64::consts::PI * libm::sqrt(a * a * a / mu_star);
    sys.bodies.push(outer);
    sys.system.bodies.push("outer".into());
    let errs = validate_system(&sys);
    let hits = errs
        .iter()
        .filter(|e| matches!(e, ValidationError::SiblingSoiOverlap { .. }))
        .count();
    assert_eq!(hits, 1, "the unordered pair reports exactly once: {errs:?}");
}

#[test]
fn disjoint_shell_sois_inside_the_gap_pass() {
    let mut sys = fixture();
    // Same outer sibling but with a small SOI: 1e8 + 1e8 < 5e8 gap.
    let mut outer = sys.bodies[1].clone();
    outer.def.id = "outer".into();
    let a = 1.5e9;
    let mu_star = sys.bodies[0].def.mu_m3_per_s2;
    let o = outer.def.orbit.as_mut().unwrap();
    o.semi_major_axis_m = a;
    o.period_s = 2.0 * core::f64::consts::PI * libm::sqrt(a * a * a / mu_star);
    sys.bodies.push(outer);
    sys.system.bodies.push("outer".into());
    let errs = validate_system(&sys);
    assert!(
        !errs
            .iter()
            .any(|e| matches!(e, ValidationError::SiblingSoiOverlap { .. })),
        "{errs:?}"
    );
}

#[test]
fn parent_cycle_is_rejected() {
    let mut sys = fixture();
    // Add a moon pair that orbit each other.
    let mut a = sys.bodies[1].clone();
    a.def.id = "a".into();
    a.def.parent = Some("b".into());
    let mut b = sys.bodies[1].clone();
    b.def.id = "b".into();
    b.def.parent = Some("a".into());
    sys.bodies.push(a);
    sys.bodies.push(b);
    sys.system.bodies.push("a".into());
    sys.system.bodies.push("b".into());
    let errs = validate_system(&sys);
    assert!(
        errs.iter()
            .filter(|e| matches!(e, ValidationError::ParentCycle { .. }))
            .count()
            >= 2,
        "both cycle members report: {errs:?}"
    );
}

#[test]
fn period_inconsistent_with_parent_mu_is_rejected() {
    let mut sys = fixture();
    let o = planet(&mut sys).orbit.as_mut().unwrap();
    o.period_s *= 1.000_001;
    let errs = validate_system(&sys);
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::PeriodInconsistent { .. })),
        "{errs:?}"
    );
}

#[test]
fn atmosphere_top_below_scale_height_is_rejected() {
    let mut sys = fixture();
    planet(&mut sys).atmosphere = Some(AtmosphereDef {
        sea_level_pressure_pa: 101_325.0,
        scale_height_m: 5_600.0,
        top_m: 5_000.0, // below H — §3.0 violation
        molar_mass_kg_per_mol: 0.029,
        heat_capacity_ratio: 1.4,
        temperature_knots: vec![
            TemperatureKnot {
                altitude_m: 0.0,
                temperature_k: 288.0,
            },
            TemperatureKnot {
                altitude_m: 5_000.0,
                temperature_k: 200.0,
            },
        ],
    });
    let errs = validate_system(&sys);
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::AtmosphereTopTooLow { .. })),
        "{errs:?}"
    );
}

#[test]
fn dangling_parent_is_rejected() {
    let mut sys = fixture();
    planet(&mut sys).parent = Some("ghost".into());
    let errs = validate_system(&sys);
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::ParentNotFound { .. })),
        "{errs:?}"
    );
}

#[test]
fn second_root_is_rejected() {
    let mut sys = fixture();
    let mut stray = sys.bodies[1].clone();
    stray.def.id = "stray".into();
    stray.def.parent = None;
    stray.def.orbit = None;
    sys.bodies.push(stray);
    sys.system.bodies.push("stray".into());
    let errs = validate_system(&sys);
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::RootNotStar { .. })),
        "{errs:?}"
    );
}

#[test]
fn bad_schema_version_is_rejected() {
    let mut sys = fixture();
    planet(&mut sys).schema_version = 99;
    let errs = validate_system(&sys);
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::SchemaVersion { found: 99, .. })),
        "{errs:?}"
    );
}

#[test]
fn hyperbolic_and_negative_eccentricity_are_rejected() {
    for e in [1.0, 1.5, -0.1] {
        let mut sys = fixture();
        planet(&mut sys).orbit.as_mut().unwrap().eccentricity = e;
        let errs = validate_system(&sys);
        assert!(
            errs.iter()
                .any(|x| matches!(x, ValidationError::EccentricityOutOfRange { .. })),
            "e={e}: {errs:?}"
        );
    }
}

#[test]
fn non_finite_radius_is_rejected() {
    let mut sys = fixture();
    planet(&mut sys).radius_m = f64::NAN;
    let errs = validate_system(&sys);
    assert!(
        errs.iter().any(|e| matches!(
            e,
            ValidationError::NonFinite {
                field: "radius_m",
                ..
            }
        )),
        "{errs:?}"
    );
}

/// A minimal valid palette-PNG byte stream for validator tests —
/// CRCs are still unchecked, but the `IDAT` is a real zlib stream
/// (the validator now decodes pixels, so a header-only PNG would
/// fail `BiomeMapPngInvalid` before the rule under test). `pixels`
/// is `w*h` palette indices, row-major.
fn tiny_png(w: u32, h: u32, palette_entries: usize, pixels: &[u8]) -> Vec<u8> {
    assert_eq!(pixels.len(), (w * h) as usize);
    let mut raw = Vec::with_capacity((w as usize + 1) * h as usize);
    for row in pixels.chunks_exact(w as usize) {
        raw.push(0); // scanline filter: None
        raw.extend_from_slice(row);
    }
    let idat = miniz_oxide::deflate::compress_to_vec_zlib(&raw, 6);
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 3, 0, 0, 0]);
    for (kind, data) in [
        (*b"IHDR", ihdr),
        (*b"PLTE", vec![0u8; palette_entries * 3]),
        (*b"IDAT", idat),
        (*b"IEND", Vec::new()),
    ] {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(&kind);
        out.extend_from_slice(&data);
        out.extend_from_slice(&[0, 0, 0, 0]); // unchecked CRC
    }
    out
}

fn with_biomes(sys: &mut LoadedSystem, legend_idx: u8, png: Vec<u8>) {
    let b = &mut sys.bodies[1];
    b.def.biome_map = Some(crate::system::BiomeMapRef {
        png: "biomes.png".into(),
        legend: "biomes.ron".into(),
    });
    b.biomes = Some(LoadedBiomeMap {
        legend: crate::biome::BiomeLegend {
            schema_version: crate::biome::BIOME_SCHEMA_VERSION,
            entries: vec![
                BiomeLegendEntry {
                    index: 0,
                    id: BiomeId("planet.one".into()),
                    name: "biome.planet.one".into(),
                },
                BiomeLegendEntry {
                    index: legend_idx,
                    id: BiomeId("planet.two".into()),
                    name: "biome.planet.two".into(),
                },
            ],
        },
        png,
    });
}

#[test]
fn biome_legend_index_beyond_palette_is_rejected() {
    let mut sys = fixture();
    with_biomes(&mut sys, 5, tiny_png(4, 4, 2, &[0; 16])); // palette of 2, index 5
    let errs = validate_system(&sys);
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::BiomeIndexExceedsPalette { .. })),
        "{errs:?}"
    );
}

/// The validator decodes pixels: a map painting a palette index
/// the legend never names is an unnamed biome region.
#[test]
fn unlegended_pixel_index_is_rejected() {
    let mut sys = fixture();
    // Legend covers indices 0 and 1; the map paints a 2 (palette
    // of 4, so `BiomeIndexExceedsPalette` does not fire — this is
    // the legend-coverage rule specifically).
    let mut pixels = [0u8; 16];
    pixels[5] = 2;
    with_biomes(&mut sys, 1, tiny_png(4, 4, 4, &pixels));
    let errs = validate_system(&sys);
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::BiomeIndexNotInLegend { index: 2, .. })),
        "{errs:?}"
    );
    // And the clean map passes the coverage check entirely.
    let mut sys = fixture();
    with_biomes(&mut sys, 1, tiny_png(4, 4, 4, &[1; 16]));
    let errs = validate_system(&sys);
    assert!(
        !errs
            .iter()
            .any(|e| matches!(e, ValidationError::BiomeIndexNotInLegend { .. })),
        "{errs:?}"
    );
}

#[test]
fn non_png_biome_map_is_rejected() {
    let mut sys = fixture();
    with_biomes(&mut sys, 1, b"not a png".to_vec());
    let errs = validate_system(&sys);
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::BiomeMapPngInvalid { .. })),
        "{errs:?}"
    );
}

#[test]
fn oversized_biome_map_is_rejected() {
    let mut sys = fixture();
    with_biomes(&mut sys, 1, tiny_png(4096, 32, 2, &vec![0; 4096 * 32]));
    let errs = validate_system(&sys);
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::BiomeMapTooLarge { .. })),
        "{errs:?}"
    );
}

#[test]
fn duplicate_legend_index_is_rejected() {
    let mut sys = fixture();
    with_biomes(&mut sys, 0, tiny_png(4, 4, 2, &[0; 16])); // two entries at index 0
    let errs = validate_system(&sys);
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::BiomeLegendDuplicate { .. })),
        "{errs:?}"
    );
}

#[test]
fn unresolved_loc_key_is_rejected() {
    let mut loc = std::collections::BTreeMap::new();
    loc.insert("system.test.epoch_note".to_string(), "x".to_string());
    loc.insert("body.star.name".to_string(), "x".to_string());
    // Deliberately omit body.planet.name.
    let loc = LocFile {
        schema_version: crate::loc::LOC_SCHEMA_VERSION,
        strings: loc,
    };
    let errs = validate_loc_keys(&fixture(), &loc);
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::UnresolvedLocKey { key, .. } if key == "body.planet.name")),
        "{errs:?}"
    );
}

// ---- part/resource validator tests (§3.1, §3.4) ----

use std::path::PathBuf;

use crate::part::{
    AeroDef, AeroKnot, AttachNode, ColliderDef, ModuleDef, PART_SCHEMA_VERSION, PartCategory,
    PartDef, PartVariant, QualCurve, QualKnot, ReliabilityDef, ScaleRule, StressKind, StressRule,
    StructuralDef, ThermalDef, ThrottleRange,
};
use crate::resource::{RESOURCE_SCHEMA_VERSION, ResourceDef};

/// A minimal *valid* resource table — enough for the part fixture's
/// references (propellants + build materials + a battery).
fn res_fixture() -> ResourceFile {
    ResourceFile {
        schema_version: RESOURCE_SCHEMA_VERSION,
        resources: vec![
            ResourceDef {
                id: "liquid_fuel".into(),
                display_name: "resource.liquid_fuel".into(),
                mass_per_unit_kg: 5.0,
            },
            ResourceDef {
                id: "oxidizer".into(),
                display_name: "resource.oxidizer".into(),
                mass_per_unit_kg: 5.0,
            },
            ResourceDef {
                id: "metal".into(),
                display_name: "resource.metal".into(),
                mass_per_unit_kg: 10.0,
            },
            ResourceDef {
                id: "electric_charge".into(),
                display_name: "resource.electric_charge".into(),
                mass_per_unit_kg: 0.0,
            },
        ],
    }
}

/// A minimal *valid* engine def — the negative tests each mutate
/// one rule so attribution is unambiguous.
fn part_fixture() -> PartDef {
    PartDef {
        schema_version: PART_SCHEMA_VERSION,
        id: "test.engine".into(),
        family: "test.family".into(),
        display_name: "part.test.engine.name".into(),
        category: PartCategory::Engine,
        mass_dry_kg: 900.0,
        cost_funds: 1500.0,
        size_m: csp_core::DVec3::new(1.25, 1.25, 1.8),
        build_resources: vec![("metal".into(), 70.0)],
        attach_nodes: vec![AttachNode {
            id: "top".into(),
            position_m: csp_core::DVec3::new(0.0, 0.0, 0.9),
            direction: csp_core::DVec3::Z,
            diameter_m: 1.25,
        }],
        scale: ScaleRule::None,
        collider: ColliderDef::Cylinder {
            radius_m: 0.625,
            half_height_m: 0.9,
        },
        aero: AeroDef {
            cd_curve: vec![
                AeroKnot {
                    mach: 0.0,
                    coeff: 0.5,
                },
                AeroKnot {
                    mach: 8.0,
                    coeff: 0.3,
                },
            ],
            cl_curve: vec![AeroKnot {
                mach: 0.0,
                coeff: 0.0,
            }],
            ref_len_m: 1.8,
        },
        thermal: ThermalDef {
            heat_capacity_j_per_k: 450_000.0,
            max_temp_k: 1600.0,
            emissivity: 0.8,
            conduction_w_per_k: 60.0,
        },
        structural: StructuralDef {
            max_axial_n: 600_000.0,
            max_shear_n: 150_000.0,
            max_torque_nm: 25_000.0,
        },
        variants: vec![PartVariant {
            id: "stock".into(),
            display_name: "part.variant.stock".into(),
            cost_multiplier: 1.0,
            stat_multipliers: Vec::new(),
        }],
        modules: vec![ModuleDef::Engine {
            thrust_vac_n: 200_000.0,
            isp_vac_s: 320.0,
            isp_sl_s: 280.0,
            throttle_range: ThrottleRange { min: 0.3, max: 1.0 },
            gimbal_deg: 3.0,
            ignitions: Some(6),
            rated_burn_s: 240.0,
            propellants: vec![("liquid_fuel".into(), 0.45), ("oxidizer".into(), 0.55)],
            ullage_required: false,
        }],
        reliability: ReliabilityDef {
            base_mtbf_s: 400_000.0,
            infant_mortality: 0.2,
            qualification_curve: QualCurve {
                knots: vec![
                    QualKnot {
                        qualification_units: 0.0,
                        hazard_multiplier: 1.0,
                    },
                    QualKnot {
                        qualification_units: 100.0,
                        hazard_multiplier: 0.5,
                    },
                ],
            },
            regime_stress: vec![StressRule {
                kind: StressKind::ExcessG,
                threshold: 12.0,
                multiplier: 2.0,
            }],
            dormant_stress: 0.02,
            failure_modes: vec![(crate::part::FailureMode::DegradedThrust, 1.0)],
        },
        tech_requirement: "tech.start".into(),
    }
}

fn loaded_part(def: PartDef) -> LoadedParts {
    LoadedParts {
        parts: vec![LoadedPart {
            path: PathBuf::from(format!("{}.ron", def.id)),
            def,
        }],
    }
}

/// Empty registries for the `Converter`/`Experiment` cross-ref
/// checks — `part_fixture` carries an `Engine` module only, so the
/// empty set is enough for every negative test below; the
/// cross-ref tests install their own.
fn empty_registries() -> (crate::load::LoadedRecipes, crate::load::LoadedExperiments) {
    (
        crate::load::LoadedRecipes {
            recipes: Vec::new(),
        },
        crate::load::LoadedExperiments {
            experiments: Vec::new(),
        },
    )
}

fn part_errors(def: PartDef) -> Vec<ValidationError> {
    let (recipes, experiments) = empty_registries();
    validate_parts(&loaded_part(def), &res_fixture(), &recipes, &experiments)
}

/// The fixture itself must be clean — otherwise the negative tests
/// below attribute nothing.
#[test]
fn part_fixture_is_valid() {
    assert!(validate_resources(&res_fixture()).is_empty());
    assert!(part_errors(part_fixture()).is_empty());
}

#[test]
fn part_bad_schema_version_is_rejected() {
    let mut d = part_fixture();
    d.schema_version = 42;
    let errs = part_errors(d);
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::SchemaVersion { found: 42, .. })),
        "{errs:?}"
    );
}

#[test]
fn duplicate_part_id_is_rejected() {
    let d = part_fixture();
    let loaded = LoadedParts {
        parts: vec![
            LoadedPart {
                path: PathBuf::from("test.engine.ron"),
                def: d.clone(),
            },
            LoadedPart {
                path: PathBuf::from("copy/test.engine.ron"),
                def: d,
            },
        ],
    };
    let (recipes, experiments) = empty_registries();
    let errs = validate_parts(&loaded, &res_fixture(), &recipes, &experiments);
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::DuplicatePartId { .. })),
        "{errs:?}"
    );
}

#[test]
fn part_id_file_mismatch_is_rejected() {
    let loaded = LoadedParts {
        parts: vec![LoadedPart {
            path: PathBuf::from("wrong.name.ron"),
            def: part_fixture(),
        }],
    };
    let (recipes, experiments) = empty_registries();
    let errs = validate_parts(&loaded, &res_fixture(), &recipes, &experiments);
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::PartIdFileMismatch { .. })),
        "{errs:?}"
    );
}

#[test]
fn non_positive_part_mass_is_rejected() {
    let mut d = part_fixture();
    d.mass_dry_kg = -1.0;
    let errs = part_errors(d);
    assert!(
        errs.iter().any(|e| matches!(
            e,
            ValidationError::PartNonPositive {
                field: "mass_dry_kg",
                ..
            }
        )),
        "{errs:?}"
    );
}

#[test]
fn non_finite_part_field_is_rejected() {
    let mut d = part_fixture();
    d.size_m.x = f64::NAN;
    let errs = part_errors(d);
    assert!(
        errs.iter().any(|e| matches!(
            e,
            ValidationError::PartNonFinite {
                field: "size_m",
                ..
            }
        )),
        "{errs:?}"
    );
}

#[test]
fn unknown_and_duplicate_resources_are_rejected() {
    let mut d = part_fixture();
    d.build_resources = vec![("metal".into(), 1.0), ("metal".into(), 2.0)];
    let errs = part_errors(d);
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::DuplicateResource { .. })),
        "{errs:?}"
    );

    let mut d = part_fixture();
    d.build_resources = vec![("unobtainium".into(), 1.0)];
    let errs = part_errors(d);
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::UnknownResource { resource, .. } if resource == "unobtainium")),
        "{errs:?}"
    );
}

#[test]
fn empty_and_duplicate_variants_are_rejected() {
    let mut d = part_fixture();
    d.variants.clear();
    let errs = part_errors(d);
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::EmptyVariants { .. })),
        "{errs:?}"
    );

    let mut d = part_fixture();
    d.variants.push(d.variants[0].clone());
    let errs = part_errors(d);
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::DuplicateVariantId { .. })),
        "{errs:?}"
    );
}

#[test]
fn variant_stat_errors_are_rejected() {
    use crate::part::StatPath;
    // Same stat twice.
    let mut d = part_fixture();
    d.variants[0].stat_multipliers = vec![(StatPath::IspVacS, 1.1), (StatPath::IspVacS, 1.2)];
    let errs = part_errors(d);
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::DuplicateVariantStat { .. })),
        "{errs:?}"
    );
    // Non-positive multiplier.
    let mut d = part_fixture();
    d.variants[0].stat_multipliers = vec![(StatPath::ThrustVacN, 0.0)];
    let errs = part_errors(d);
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::BadVariantMultiplier { .. })),
        "{errs:?}"
    );
    // A stat path the def does not carry — `resolve_built_part`
    // would fail it at manufacture (`UnresolvableStatPath`).
    let mut d = part_fixture();
    d.variants[0].stat_multipliers = vec![(StatPath::ControlTorqueNm, 1.5)];
    let errs = part_errors(d);
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::UnresolvableVariantStat { .. })),
        "{errs:?}"
    );
}

#[test]
fn propellant_fraction_overflow_is_rejected() {
    // Two finite positive fractions whose sum overflows to +inf:
    // the resolver's gate rejects it, so the validator must too —
    // the pre-fix `sum.is_finite() && …` form passed it green.
    let mut d = part_fixture();
    let ModuleDef::Engine { propellants, .. } = &mut d.modules[0] else {
        unreachable!()
    };
    *propellants = vec![
        ("liquid_fuel".into(), 1.0e308),
        ("oxidizer".into(), 1.0e308),
    ];
    let errs = part_errors(d);
    assert!(
        errs.iter().any(|e| matches!(
            e,
            ValidationError::BadModule { reason, .. }
                if *reason == "propellant mass fractions do not sum to 1"
        )),
        "{errs:?}"
    );
}

#[test]
fn bad_scale_rules_are_rejected() {
    // Discrete without the 1.0 baseline.
    let mut d = part_fixture();
    d.scale = ScaleRule::Discrete(vec![0.5, 2.0]);
    let errs = part_errors(d);
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::BadScaleRule { .. })),
        "{errs:?}"
    );
    // Discrete unsorted.
    let mut d = part_fixture();
    d.scale = ScaleRule::Discrete(vec![2.0, 1.0]);
    let errs = part_errors(d);
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::BadScaleRule { .. })),
        "{errs:?}"
    );
    // Continuous range that does not contain 1.0.
    let mut d = part_fixture();
    d.scale = ScaleRule::Continuous {
        min: 1.5,
        max: 3.0,
        exponents: crate::part::ScaleExponents {
            mass: 3.0,
            cost: 3.0,
            thrust: 2.0,
            capacity: 3.0,
            structural: 2.0,
        },
    };
    let errs = part_errors(d);
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::BadScaleRule { .. })),
        "{errs:?}"
    );
    // Continuous exponent out of the sane bound.
    let mut d = part_fixture();
    d.scale = ScaleRule::Continuous {
        min: 0.5,
        max: 2.0,
        exponents: crate::part::ScaleExponents {
            mass: 9.0,
            cost: 3.0,
            thrust: 2.0,
            capacity: 3.0,
            structural: 2.0,
        },
    };
    let errs = part_errors(d);
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::BadScaleRule { .. })),
        "{errs:?}"
    );
}

#[test]
fn bad_attach_nodes_are_rejected() {
    // No nodes at all.
    let mut d = part_fixture();
    d.attach_nodes.clear();
    let errs = part_errors(d);
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::BadAttachNode { .. })),
        "{errs:?}"
    );
    // Non-unit direction.
    let mut d = part_fixture();
    d.attach_nodes[0].direction = csp_core::DVec3::new(0.0, 0.0, 2.0);
    let errs = part_errors(d);
    assert!(
        errs.iter().any(|e| matches!(
            e,
            ValidationError::BadAttachNode {
                reason: "direction is not unit length",
                ..
            }
        )),
        "{errs:?}"
    );
    // Duplicate node id.
    let mut d = part_fixture();
    d.attach_nodes.push(d.attach_nodes[0].clone());
    let errs = part_errors(d);
    assert!(
        errs.iter().any(|e| matches!(
            e,
            ValidationError::BadAttachNode {
                reason: "duplicate node id",
                ..
            }
        )),
        "{errs:?}"
    );
}

#[test]
fn category_module_mismatch_is_rejected() {
    let mut d = part_fixture();
    d.modules = Vec::new(); // category Engine without an Engine module
    let errs = part_errors(d);
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::CategoryModuleMismatch { .. })),
        "{errs:?}"
    );
}

#[test]
fn discrete_scaled_engine_or_tank_is_rejected() {
    // Engine module on a Discrete rule: the size menu cannot scale
    // its stats — forbidden outright.
    let mut d = part_fixture();
    d.scale = ScaleRule::Discrete(vec![0.5, 1.0, 2.0]);
    let errs = part_errors(d);
    assert!(
        errs.iter().any(|e| matches!(
            e,
            ValidationError::BadModule {
                reason: "discrete scaling carries no exponents — tank/engine stats would not scale",
                ..
            }
        )),
        "{errs:?}"
    );
    // A tank-only part fares no better.
    let mut d = part_fixture();
    d.scale = ScaleRule::Discrete(vec![0.5, 1.0, 2.0]);
    d.modules = vec![ModuleDef::Tank {
        capacity: vec![("liquid_fuel".into(), 100.0)],
    }];
    d.category = PartCategory::FuelTank;
    let errs = part_errors(d);
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::BadModule { .. })),
        "{errs:?}"
    );
    // Discrete on a passive part is fine.
    let mut d = part_fixture();
    d.scale = ScaleRule::Discrete(vec![0.5, 1.0, 2.0]);
    d.modules = vec![ModuleDef::Decoupler];
    d.category = PartCategory::Utility;
    assert!(part_errors(d).is_empty());
    // And a Continuous rule keeps its engine legal.
    let mut d = part_fixture();
    d.scale = ScaleRule::Continuous {
        min: 0.5,
        max: 2.0,
        exponents: crate::part::ScaleExponents {
            mass: 3.0,
            cost: 3.0,
            thrust: 2.0,
            capacity: 3.0,
            structural: 2.0,
        },
    };
    assert!(part_errors(d).is_empty());
}

#[test]
fn bad_engine_fields_are_rejected() {
    // isp_sl above isp_vac.
    let mut d = part_fixture();
    if let ModuleDef::Engine { isp_sl_s, .. } = &mut d.modules[0] {
        *isp_sl_s = 400.0;
    }
    let errs = part_errors(d);
    assert!(
        errs.iter().any(|e| matches!(
            e,
            ValidationError::BadModule {
                reason: "isp_sl_s exceeds isp_vac_s",
                ..
            }
        )),
        "{errs:?}"
    );
    // Propellant fractions not summing to 1.
    let mut d = part_fixture();
    if let ModuleDef::Engine { propellants, .. } = &mut d.modules[0] {
        propellants[1].1 = 0.9;
    }
    let errs = part_errors(d);
    assert!(
        errs.iter().any(|e| matches!(
            e,
            ValidationError::BadModule {
                reason: "propellant mass fractions do not sum to 1",
                ..
            }
        )),
        "{errs:?}"
    );
    // Broken throttle band.
    let mut d = part_fixture();
    if let ModuleDef::Engine { throttle_range, .. } = &mut d.modules[0] {
        *throttle_range = ThrottleRange { min: 1.2, max: 1.4 };
    }
    let errs = part_errors(d);
    assert!(
        errs.iter().any(|e| matches!(
            e,
            ValidationError::BadModule {
                reason: "throttle_range outside 0 <= min < max <= 1",
                ..
            }
        )),
        "{errs:?}"
    );
}

#[test]
fn bad_reliability_fields_are_rejected() {
    // dormant_stress above 1.
    let mut d = part_fixture();
    d.reliability.dormant_stress = 1.5;
    let errs = part_errors(d);
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::BadReliability { .. })),
        "{errs:?}"
    );
    // Empty qualification curve.
    let mut d = part_fixture();
    d.reliability.qualification_curve.knots.clear();
    let errs = part_errors(d);
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::BadReliability { .. })),
        "{errs:?}"
    );
    // Stress multiplier below 1 (stress raises hazard, §3.4).
    let mut d = part_fixture();
    d.reliability.regime_stress[0].multiplier = 0.5;
    let errs = part_errors(d);
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::BadReliability { .. })),
        "{errs:?}"
    );
    // Empty failure modes.
    let mut d = part_fixture();
    d.reliability.failure_modes.clear();
    let errs = part_errors(d);
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::BadReliability { .. })),
        "{errs:?}"
    );
}

#[test]
fn bad_collider_is_rejected() {
    let mut d = part_fixture();
    d.collider = ColliderDef::Convex {
        points: vec![csp_core::DVec3::ZERO; 3],
    };
    let errs = part_errors(d);
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::BadCollider { .. })),
        "{errs:?}"
    );
}

#[test]
fn bad_aero_curve_is_rejected() {
    let mut d = part_fixture();
    d.aero.cd_curve = vec![
        AeroKnot {
            mach: 8.0,
            coeff: 0.3,
        },
        AeroKnot {
            mach: 0.0,
            coeff: 0.5,
        },
    ];
    let errs = part_errors(d);
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::BadAeroCurve { .. })),
        "{errs:?}"
    );
}

/// §3.4's Euler-stability bound: `conduction_w_per_k` above
/// `heat_capacity_j_per_k` is rejected; equality is the accepted
/// edge.
#[test]
fn conduction_exceeding_capacity_is_rejected() {
    let mut d = part_fixture();
    d.thermal.conduction_w_per_k = d.thermal.heat_capacity_j_per_k + 1.0;
    let errs = part_errors(d);
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::ConductionExceedsCapacity { .. })),
        "{errs:?}"
    );
    // The boundary itself is valid.
    let mut d = part_fixture();
    d.thermal.conduction_w_per_k = d.thermal.heat_capacity_j_per_k;
    assert!(part_errors(d).is_empty());
}

#[test]
fn bad_resource_table_is_rejected() {
    // Duplicate resource id.
    let mut r = res_fixture();
    r.resources.push(r.resources[0].clone());
    let errs = validate_resources(&r);
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::DuplicateResource { .. })),
        "{errs:?}"
    );
    // Negative mass per unit.
    let mut r = res_fixture();
    r.resources[0].mass_per_unit_kg = -1.0;
    let errs = validate_resources(&r);
    assert!(
        errs.iter().any(|e| matches!(
            e,
            ValidationError::PartNonPositive {
                field: "mass_per_unit_kg",
                ..
            }
        )),
        "{errs:?}"
    );
}

#[test]
fn part_loc_key_resolution_is_rejected_when_missing() {
    let loaded = loaded_part(part_fixture());
    let loc = LocFile {
        schema_version: crate::loc::LOC_SCHEMA_VERSION,
        strings: std::collections::BTreeMap::new(),
    };
    let errs = validate_part_loc_keys(&loaded, &res_fixture(), &loc);
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::UnresolvedLocKey { key, .. } if key == "part.test.engine.name")),
        "{errs:?}"
    );
}

// ---------- tech tree (§3.3) ----------

use crate::load::{LoadedTechFile, LoadedTechTree};
use crate::techtree::{
    TECHTREE_SCHEMA_VERSION, TechNode, TechTreeDef, TechUnlocks, UpgradeEffect, UpgradeLevel,
    UpgradeTrack,
};

/// A minimal valid two-node tree + track over `part_fixture`'s
/// family — the part's `tech_requirement` is `tech.start`, so the
/// fixture validates green as written.
fn tree_fixture() -> LoadedTechTree {
    LoadedTechTree {
        trees: vec![LoadedTechFile {
            path: std::path::PathBuf::from("home.ron"),
            def: TechTreeDef {
                schema_version: TECHTREE_SCHEMA_VERSION,
                nodes: vec![
                    TechNode {
                        id: "tech.start".into(),
                        display_name: "tech.start.name".into(),
                        description: "tech.start.desc".into(),
                        science_cost: 0.0,
                        requires: vec![],
                        unlocks: TechUnlocks::default(),
                    },
                    TechNode {
                        id: "tech.rocketry.2".into(),
                        display_name: "tech.r2.name".into(),
                        description: "tech.r2.desc".into(),
                        science_cost: 30.0,
                        requires: vec!["tech.start".into()],
                        unlocks: TechUnlocks {
                            track_levels: vec![("track.isp".into(), 2)],
                            facilities: vec!["facility.lab".into()],
                            contract_types: vec![],
                        },
                    },
                ],
                tracks: vec![UpgradeTrack {
                    id: "track.isp".into(),
                    display_name: "track.isp.name".into(),
                    family: part_fixture().family,
                    stat_path: crate::part::StatPath::IspVacS,
                    levels: vec![
                        UpgradeLevel {
                            science_cost: 10.0,
                            multiplier_or_delta: UpgradeEffect::Multiplier(1.03),
                        },
                        UpgradeLevel {
                            science_cost: 12.0,
                            multiplier_or_delta: UpgradeEffect::Multiplier(1.03),
                        },
                    ],
                }],
            },
        }],
    }
}

fn tree_errors(tree: &LoadedTechTree, parts: &LoadedParts) -> Vec<ValidationError> {
    validate_techtree(tree, parts)
}

/// The fixture itself must be clean.
#[test]
fn tree_fixture_is_valid() {
    let errs = tree_errors(&tree_fixture(), &loaded_part(part_fixture()));
    assert!(errs.is_empty(), "{errs:?}");
}

/// §3.3 "no cycles" — including a self-loop and a two-node loop.
#[test]
fn tree_cycle_is_rejected() {
    let mut t = tree_fixture();
    t.trees[0].def.nodes[1].requires = vec!["tech.rocketry.2".into()];
    let errs = tree_errors(&t, &loaded_part(part_fixture()));
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::TechCycle { .. })),
        "{errs:?}"
    );

    let mut t = tree_fixture();
    t.trees[0].def.nodes[1].requires = vec!["tech.rocketry.3".into()];
    t.trees[0].def.nodes.push(TechNode {
        id: "tech.rocketry.3".into(),
        display_name: "k".into(),
        description: "k".into(),
        science_cost: 60.0,
        requires: vec!["tech.rocketry.2".into()],
        unlocks: TechUnlocks::default(),
    });
    let errs = tree_errors(&t, &loaded_part(part_fixture()));
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::TechCycle { .. })),
        "{errs:?}"
    );
}

/// §3.3 "every part reachable" — a part gated on a missing node.
#[test]
fn unreachable_part_is_rejected() {
    let mut d = part_fixture();
    d.tech_requirement = "tech.nonexistent".into();
    let errs = tree_errors(&tree_fixture(), &loaded_part(d));
    assert!(
        errs.iter().any(|e| matches!(
            e,
            ValidationError::UnknownTechRequirement { tech, .. } if tech == "tech.nonexistent"
        )),
        "{errs:?}"
    );
}

/// §3.3 "cost monotonic in depth" — a node cheaper than its
/// prerequisite is rejected; equal cost is legal.
#[test]
fn non_monotonic_cost_is_rejected() {
    let mut t = tree_fixture();
    t.trees[0].def.nodes[1].science_cost = -5.0;
    let errs = tree_errors(&t, &loaded_part(part_fixture()));
    assert!(
        errs.iter().any(|e| matches!(
            e,
            ValidationError::TechCostNotMonotonic { .. } | ValidationError::BadTechNode { .. }
        )),
        "{errs:?}"
    );
    // Equal cost along the edge is monotonic (non-decreasing).
    let mut t = tree_fixture();
    t.trees[0].def.nodes[0].science_cost = 30.0;
    assert!(tree_errors(&t, &loaded_part(part_fixture())).is_empty());
}

/// Dangling references: unknown prerequisite, unknown track grant,
/// grant cap outside the authored levels.
#[test]
fn bad_tree_refs_are_rejected() {
    let mut t = tree_fixture();
    t.trees[0].def.nodes[1].requires = vec!["tech.missing".into()];
    let errs = tree_errors(&t, &loaded_part(part_fixture()));
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::UnknownTechPrerequisite { .. })),
        "{errs:?}"
    );

    let mut t = tree_fixture();
    t.trees[0].def.nodes[1].unlocks.track_levels = vec![("track.missing".into(), 1)];
    let errs = tree_errors(&t, &loaded_part(part_fixture()));
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::UnknownUpgradeTrack { .. })),
        "{errs:?}"
    );

    let mut t = tree_fixture();
    t.trees[0].def.nodes[1].unlocks.track_levels = vec![("track.isp".into(), 9)];
    let errs = tree_errors(&t, &loaded_part(part_fixture()));
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::BadTechNode { .. })),
        "{errs:?}"
    );
}

/// Track shape rules: empty levels, bad effect, a stat the family
/// cannot resolve, a family with no parts, duplicate ids.
#[test]
fn bad_tracks_are_rejected() {
    // A stat the fixture part cannot resolve (it has no Control).
    let mut t = tree_fixture();
    t.trees[0].def.tracks[0].stat_path = crate::part::StatPath::ControlTorqueNm;
    let errs = tree_errors(&t, &loaded_part(part_fixture()));
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::BadUpgradeTrack { .. })),
        "{errs:?}"
    );

    let mut t = tree_fixture();
    t.trees[0].def.tracks[0].family = "family.nobody".into();
    let errs = tree_errors(&t, &loaded_part(part_fixture()));
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::BadUpgradeTrack { .. })),
        "{errs:?}"
    );

    let mut t = tree_fixture();
    let dup = t.trees[0].def.tracks[0].clone();
    t.trees[0].def.tracks.push(dup);
    let errs = tree_errors(&t, &loaded_part(part_fixture()));
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::DuplicateUpgradeTrackId { .. })),
        "{errs:?}"
    );

    let mut t = tree_fixture();
    t.trees[0].def.tracks[0].levels.clear();
    let errs = tree_errors(&t, &loaded_part(part_fixture()));
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::BadUpgradeTrack { .. })),
        "{errs:?}"
    );
}

// ---- colony validator tests (§3.5.1) ----

/// A sites file with one pad + a colony file over it + the recipe
/// its converter names — the `electric_charge`-collision negative
/// mutates the recipe's inputs.
fn colony_fixture() -> (
    crate::colony::ColonyFile,
    SiteFile,
    ResourceFile,
    crate::load::LoadedRecipes,
) {
    let sites = SiteFile {
        schema_version: crate::site::SITE_SCHEMA_VERSION,
        sites: vec![crate::site::SiteDef {
            schema_version: 0,
            id: "pad".into(),
            display_name: LocKey::from("site.pad"),
            body: "planet".into(),
            latitude_rad: 0.0,
            longitude_rad: 0.0,
            altitude_m: 0.0,
            launch: true,
            resource_yields: vec![("ore".into(), 0.5)],
            starting_stock: vec![],
        }],
    };
    let colonies = crate::colony::ColonyFile {
        schema_version: crate::colony::COLONY_SCHEMA_VERSION,
        colonies: vec![crate::colony::ColonyDef {
            site: "pad".into(),
            display_name: LocKey::from("colony.x"),
            modules: vec![crate::colony::ColonyModuleDef::Converter {
                recipe: "recipe.smelt".into(),
                priority: 5,
                power_ec_per_s: 0.5,
            }],
            stores: vec![],
        }],
    };
    let recipes = crate::load::LoadedRecipes {
        recipes: vec![crate::load::LoadedRecipe {
            path: std::path::PathBuf::from("recipe.smelt.ron"),
            def: crate::recipe::RecipeDef {
                schema_version: crate::recipe::RECIPE_SCHEMA_VERSION,
                id: "recipe.smelt".into(),
                display_name: "recipe.smelt.name".into(),
                inputs: vec![("ore".into(), 1.0)],
                outputs: vec![("metal".into(), 0.5)],
            },
        }],
    };
    let mut res = res_fixture();
    res.resources.push(crate::resource::ResourceDef {
        id: "ore".into(),
        display_name: "resource.ore".into(),
        mass_per_unit_kg: 1.0,
    });
    (colonies, sites, res, recipes)
}

/// The fixture is clean; listing `electric_charge` in the recipe's
/// inputs AND a nonzero `power_ec_per_s` — one EC draw authored
/// twice — is rejected, on a converter and on life support alike.
#[test]
fn colony_power_consumes_collision_is_rejected() {
    let (file, sites, res, recipes) = colony_fixture();
    assert!(validate_colonies(&file, &sites, &res, &recipes).is_empty());
    let (mut bad, _, _, mut bad_recipes) = colony_fixture();
    bad_recipes.recipes[0]
        .def
        .inputs
        .push(("electric_charge".into(), 1.0));
    let errs = validate_colonies(&bad, &sites, &res, &bad_recipes);
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::InvalidColony { .. })),
        "{errs:?}"
    );
    // Same rule on LifeSupport's consumes_per_crew.
    bad.colonies[0].modules[0] = crate::colony::ColonyModuleDef::LifeSupport {
        consumes_per_crew: vec![("electric_charge".into(), 0.1)],
        power_ec_per_s: 0.5,
    };
    let errs = validate_colonies(&bad, &sites, &res, &recipes);
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::InvalidColony { .. })),
        "{errs:?}"
    );
    // Zero power → no collision: an EC consumes entry alone is legal.
    bad.colonies[0].modules[0] = crate::colony::ColonyModuleDef::LifeSupport {
        consumes_per_crew: vec![("electric_charge".into(), 0.1)],
        power_ec_per_s: 0.0,
    };
    assert!(validate_colonies(&bad, &sites, &res, &recipes).is_empty());
}

// ---- Phase 9.2: module cross-references + version discipline ----

/// `ModuleDef::Converter.recipe` and `ModuleDef::Experiment.kind`
/// must resolve in `data/recipes`/`data/experiments` — an
/// unresolved recipe is a dead converter in `csp-sim`, an
/// unresolved kind can never run. (`part_errors` runs against the
/// empty registries, so every authored ref is unresolvable.)
#[test]
fn converter_recipe_and_experiment_kind_resolve() {
    let mut d = part_fixture();
    d.category = PartCategory::Utility;
    d.modules = vec![
        ModuleDef::Converter {
            recipe: "recipe.missing".into(),
            priority: 0,
            power_w: 0.0,
        },
        ModuleDef::Experiment {
            kind: "exp.missing".into(),
            data_units: 1.0,
            reusable: true,
            situations: crate::part::SituationMask(1),
        },
    ];
    let errs = part_errors(d);
    assert!(
        errs.iter().any(|e| matches!(
            e,
            ValidationError::UnknownRecipe { recipe, .. } if recipe == "recipe.missing"
        )),
        "{errs:?}"
    );
    assert!(
        errs.iter().any(|e| matches!(
            e,
            ValidationError::UnknownExperimentKind { experiment, .. } if experiment == "exp.missing"
        )),
        "{errs:?}"
    );

    // Positive half: a recipe the registry *does* carry is fine.
    let mut d = part_fixture();
    d.category = PartCategory::Utility;
    d.modules = vec![ModuleDef::Converter {
        recipe: "recipe.smelt".into(),
        priority: 0,
        power_w: 0.0,
    }];
    let (_, _, _, recipes) = colony_fixture();
    let (_, experiments) = empty_registries();
    let errs = validate_parts(&loaded_part(d), &res_fixture(), &recipes, &experiments);
    assert!(
        !errs
            .iter()
            .any(|e| matches!(e, ValidationError::UnknownRecipe { .. })),
        "{errs:?}"
    );
}

/// `ExperimentCap.body` must name a body in the system — a cap on
/// an unknown body is dead authored data (`cap_for`
/// exact-matches). The experiment version gate rides the same
/// file.
#[test]
fn experiment_cap_body_and_version_are_checked() {
    let mut loaded = crate::load::LoadedExperiments {
        experiments: vec![crate::load::LoadedExperiment {
            path: PathBuf::from("exp.test.ron"),
            def: crate::experiment::ExperimentDef {
                schema_version: crate::experiment::EXPERIMENT_SCHEMA_VERSION,
                id: "exp.test".into(),
                display_name: "exp.test.name".into(),
                transmit_ratio: 0.5,
                caps: vec![crate::experiment::ExperimentCap {
                    body: "kerbin".into(),
                    situations: crate::part::SituationMask(1),
                    cap_data_units: 1.0,
                }],
            },
        }],
    };
    let errs = validate_experiments(&loaded, &fixture());
    assert!(
        errs.iter().any(|e| matches!(
            e,
            ValidationError::BadExperiment { experiment, reason }
                if experiment == "exp.test" && *reason == "cap names a body not in the system"
        )),
        "{errs:?}"
    );

    loaded.experiments[0].def.caps[0].body = "planet".into();
    loaded.experiments[0].def.schema_version = 0;
    let errs = validate_experiments(&loaded, &fixture());
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::SchemaVersion { found: 0, .. })),
        "{errs:?}"
    );
}

/// An experiment with no `caps` validates green no longer —
/// `cap_for` returns `None` for every context, so the kind can
/// never pay out anywhere.
#[test]
fn experiment_with_no_caps_is_rejected() {
    let loaded = crate::load::LoadedExperiments {
        experiments: vec![crate::load::LoadedExperiment {
            path: PathBuf::from("exp.test.ron"),
            def: crate::experiment::ExperimentDef {
                schema_version: crate::experiment::EXPERIMENT_SCHEMA_VERSION,
                id: "exp.test".into(),
                display_name: "exp.test.name".into(),
                transmit_ratio: 0.5,
                caps: Vec::new(),
            },
        }],
    };
    let errs = validate_experiments(&loaded, &fixture());
    assert!(
        errs.iter().any(|e| matches!(
            e,
            ValidationError::BadExperiment { experiment, .. } if experiment == "exp.test"
        )),
        "{errs:?}"
    );
}

/// The `surface_resources` `ResourceId` keys resolve against
/// `resources.ron` — gated in [`validate_dataset`]'s
/// `check_surface_resources` (the resource file lives outside the
/// system directory, so `validate_system` itself cannot express
/// the check).
#[test]
fn unknown_surface_resource_is_rejected() {
    let mut sys = fixture();
    sys.bodies[1].def.surface_resources = vec![(
        "unobtainium".into(),
        crate::system::ConcentrationMapRef {
            png: "ore.png".to_string(),
        },
    )];
    let mut out = Vec::new();
    check_surface_resources(&sys, &res_fixture(), &mut out);
    assert!(
        out.iter()
            .any(|e| matches!(e, ValidationError::UnknownSurfaceResource { .. })),
        "{out:?}"
    );
    // The fixture's empty `surface_resources` reports nothing.
    let mut out = Vec::new();
    check_surface_resources(&fixture(), &res_fixture(), &mut out);
    assert!(out.is_empty(), "{out:?}");
}

/// Colony and recipe `display_name` keys get the same loc gate
/// every other LocKey-carrying file already had.
#[test]
fn colony_and_recipe_loc_keys_are_checked() {
    let loc = LocFile {
        schema_version: crate::loc::LOC_SCHEMA_VERSION,
        strings: BTreeMap::new(),
    };
    let colonies = crate::colony::ColonyFile {
        schema_version: crate::colony::COLONY_SCHEMA_VERSION,
        colonies: vec![crate::colony::ColonyDef {
            site: "site.pad".into(),
            display_name: "colony.pad.name".into(),
            modules: Vec::new(),
            stores: Vec::new(),
        }],
    };
    let errs = validate_colony_loc_keys(&colonies, &loc);
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::UnresolvedLocKey { key, .. } if key == "colony.pad.name")),
        "{errs:?}"
    );

    let recipes = crate::load::LoadedRecipes {
        recipes: vec![crate::load::LoadedRecipe {
            path: PathBuf::from("recipe.test.ron"),
            def: crate::recipe::RecipeDef {
                schema_version: crate::recipe::RECIPE_SCHEMA_VERSION,
                id: "recipe.test".into(),
                display_name: "recipe.test.name".into(),
                inputs: Vec::new(),
                outputs: Vec::new(),
            },
        }],
    };
    let errs = validate_recipe_loc_keys(&recipes, &loc);
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::UnresolvedLocKey { key, .. } if key == "recipe.test.name")),
        "{errs:?}"
    );
}

/// The legacy per-entry version (`SiteDef::schema_version`, now
/// optional): a declared value gets the same current-or-previous
/// band as the `SiteFile` wrapper; `0` — "not declared", the
/// current authoring form — needs nothing.
#[test]
fn site_entry_schema_version_is_checked() {
    let (_, mut sites, res, _) = colony_fixture();
    sites.sites[0].schema_version = 99;
    let errs = validate_sites(&sites, &fixture(), &res);
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::SchemaVersion { found: 99, .. })),
        "{errs:?}"
    );
    // Undeclared (0) is clean; so is a declared version in the band.
    let (_, mut sites, res, _) = colony_fixture();
    sites.sites[0].schema_version = 0;
    assert!(validate_sites(&sites, &fixture(), &res).is_empty());
    sites.sites[0].schema_version = crate::site::SITE_SCHEMA_VERSION;
    assert!(validate_sites(&sites, &fixture(), &res).is_empty());
}

/// C09-F2's loader-time guard: on a zero-synodic body (the
/// fixture's planet is `rotation_period_s: None` — locked to the
/// star, a permanently fixed sun) the site's Day/Night verdict is
/// decided ONCE by `cos H0·cos φ`, so a longitude authored within
/// `FIXED_SUN_TERMINATOR_MIN_COS` of the line is rejected — the
/// shipped rask-mines π/2 (Day hung on a +6.1e-17 residue) is
/// exactly the shape this blocks.
#[test]
fn zero_synodic_site_must_clear_the_terminator() {
    let (_, sites, res, _) = colony_fixture();
    // L0 = 0 in the fixture ⇒ h0 = lon − π; lon π/2 sits on the
    // line itself (|cos| = 6.1e-17 — the shipped bug, verbatim).
    let mut on_line = sites.clone();
    on_line.sites[0].body = "planet".into();
    on_line.sites[0].longitude_rad = core::f64::consts::FRAC_PI_2;
    let errs = validate_sites(&on_line, &fixture(), &res);
    assert!(
        errs.iter().any(|e| matches!(
            e,
            ValidationError::InvalidSite { reason, .. } if reason.contains("FIXED terminator")
        )),
        "{errs:?}"
    );
    // Off the line: the shipped fix's 2.4 rad (|cos| = 0.737) and
    // the antipode's permanent-night side (|cos| = 1.0 — dark is
    // a verdict, not an error) both carry real margins.
    for lon in [2.4, 0.0] {
        let mut ok = sites.clone();
        ok.sites[0].body = "planet".into();
        ok.sites[0].longitude_rad = lon;
        let errs = validate_sites(&ok, &fixture(), &res);
        assert!(errs.is_empty(), "lon {lon}: {errs:?}");
    }
    // A ROTATING body keeps cycling through day/night — grazing
    // the terminator is a phase, not a permanent verdict, so the
    // rule does not fire even at π/2.
    let mut sys = fixture();
    planet(&mut sys).rotation_period_s = Some(10_000.0);
    let errs = validate_sites(&on_line, &sys, &res);
    assert!(errs.is_empty(), "rotating body: {errs:?}");
}

/// `validate_contracts` runs the per-template schema_version gate.
#[test]
fn contract_schema_version_is_checked() {
    let contracts = crate::load::LoadedContracts {
        contracts: vec![crate::load::LoadedContract {
            path: PathBuf::from("contract.test.ron"),
            def: crate::contract::ContractTemplate {
                schema_version: 0,
                id: "contract.test".into(),
                display_name: "k".into(),
                description: "k".into(),
                kind: crate::contract::ContractKind::Rescue,
                generate: crate::contract::ContractGenerator {
                    weight: 1.0,
                    space: crate::contract::ContractParamSpace::Rescue {
                        bodies: vec!["planet".into()],
                    },
                },
                prerequisites: Vec::new(),
                rewards: crate::contract::ContractRewards {
                    advance_funds: 1.0,
                    completion_funds: 1.0,
                    failure_penalty_funds: 1.0,
                },
                deadline_days: None,
            },
        }],
    };
    let (_, experiments) = empty_registries();
    let tree = tree_fixture();
    let no_sites = SiteFile {
        schema_version: crate::site::SITE_SCHEMA_VERSION,
        sites: vec![],
    };
    let errs = validate_contracts(
        &contracts,
        &fixture(),
        &experiments,
        &tree,
        &res_fixture(),
        &no_sites,
    );
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::SchemaVersion { found: 0, .. })),
        "{errs:?}"
    );
}

/// `validate_loc` re-checks the `LocFile` schema_version the
/// loader gates.
#[test]
fn loc_schema_version_is_checked() {
    let loc = LocFile {
        schema_version: 0,
        strings: BTreeMap::new(),
    };
    assert!(matches!(
        validate_loc(&loc).as_slice(),
        [ValidationError::SchemaVersion { found: 0, .. }]
    ));
}

/// A `body.ron` file reference (`terrain.heightmap`, a
/// `surface_resources` map) that is not a tame one-segment name
/// reports `BadFileRef` — the same gate the loader runs before
/// `join`, repeated for hand-rolled `LoadedSystem`s.
#[test]
fn unsafe_body_file_refs_are_rejected() {
    let mut sys = fixture();
    // bodies[0] is the star; the planet carries the refs here.
    sys.bodies[1].def.terrain.heightmap = Some("../x.png".to_string());
    sys.bodies[1].def.surface_resources = vec![(
        "ore".into(),
        crate::system::ConcentrationMapRef {
            png: "sub/map.png".to_string(),
        },
    )];
    let errs = validate_system(&sys);
    assert!(
        errs.iter()
            .filter(|e| {
                matches!(e, ValidationError::BadFileRef { body, .. } if body == "planet")
            })
            .count()
            >= 2,
        "{errs:?}"
    );
}

// ---- Phase 9.3: LOW-batch gates ----

/// `SituationMask` bits 6–7 are dead authored data —
/// `situations()` drops them silently — so a mask setting them is
/// rejected on the module and on an experiment cap alike (an
/// all-spares mask is non-empty yet covers no situation).
#[test]
fn situation_mask_spare_bits_are_rejected() {
    let mut d = part_fixture();
    d.category = PartCategory::Utility;
    d.modules = vec![ModuleDef::Experiment {
        kind: "exp.test".into(),
        data_units: 1.0,
        reusable: true,
        situations: crate::part::SituationMask(0x40),
    }];
    let errs = part_errors(d);
    assert!(
        errs.iter().any(|e| matches!(
            e,
            ValidationError::BadModule {
                reason: "situation mask sets spare bits outside the six situations",
                ..
            }
        )),
        "{errs:?}"
    );

    let loaded = crate::load::LoadedExperiments {
        experiments: vec![crate::load::LoadedExperiment {
            path: PathBuf::from("exp.test.ron"),
            def: crate::experiment::ExperimentDef {
                schema_version: crate::experiment::EXPERIMENT_SCHEMA_VERSION,
                id: "exp.test".into(),
                display_name: "exp.test.name".into(),
                transmit_ratio: 0.5,
                caps: vec![crate::experiment::ExperimentCap {
                    body: "planet".into(),
                    situations: crate::part::SituationMask(0xC0),
                    cap_data_units: 1.0,
                }],
            },
        }],
    };
    let errs = validate_experiments(&loaded, &fixture());
    assert!(
        errs.iter().any(|e| matches!(
            e,
            ValidationError::BadExperiment {
                reason: "situation mask sets spare bits outside the six situations",
                ..
            }
        )),
        "{errs:?}"
    );
}

/// A `Delta`-effect track needs a positive finite base on every
/// family part — the same gate `compose_upgrade_levels` runs at
/// manufacture (`(base + Σ deltas) / base`). `Some(0.0)` used to
/// pass the resolve check alone.
#[test]
fn delta_track_nonpositive_base_is_rejected() {
    let mut t = tree_fixture();
    t.trees[0].def.tracks[0].stat_path = crate::part::StatPath::MassDryKg;
    t.trees[0].def.tracks[0].levels[0].multiplier_or_delta = UpgradeEffect::Delta(5.0);
    let mut zero_base = part_fixture();
    zero_base.mass_dry_kg = 0.0;
    let errs = tree_errors(&t, &loaded_part(zero_base.clone()));
    assert!(
        errs.iter().any(|e| matches!(
            e,
            ValidationError::BadUpgradeTrack {
                reason: "Delta track's stat_path base is not positive and finite on a family part",
                ..
            }
        )),
        "{errs:?}"
    );
    // A Multiplier track on the same zero base stays legal — the
    // manufacture gate only fires when a delta term exists.
    let mut t = tree_fixture();
    t.trees[0].def.tracks[0].stat_path = crate::part::StatPath::MassDryKg;
    assert!(
        !tree_errors(&t, &loaded_part(zero_base))
            .iter()
            .any(|e| matches!(e, ValidationError::BadUpgradeTrack { .. })),
        "multiplier track must not trip the delta-base gate"
    );
    // Positive base + Delta: the intended authoring, still green.
    let mut t = tree_fixture();
    t.trees[0].def.tracks[0].levels[0].multiplier_or_delta = UpgradeEffect::Delta(5.0);
    assert!(
        tree_errors(&t, &loaded_part(part_fixture())).is_empty(),
        "positive-base delta track must stay valid"
    );
}

/// Recipe failures stamp `InvalidRecipe`, not `InvalidColony` —
/// the recipe registry is not a colony file.
#[test]
fn recipe_errors_stamp_invalid_recipe() {
    let (_, _, res, mut recipes) = colony_fixture();
    recipes.recipes[0].def.outputs = vec![("unobtainium".into(), 1.0)];
    let errs = validate_recipes(&recipes, &res);
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::InvalidRecipe { .. })),
        "{errs:?}"
    );
    assert!(
        !errs
            .iter()
            .any(|e| matches!(e, ValidationError::InvalidColony { .. })),
        "{errs:?}"
    );
}

/// A `LoadedRecipe` builder for the recipe-graph tests — `id`
/// names file stem and def alike so the id↔stem check stays
/// green and only the rule under test fires.
fn recipe_fixture(
    id: &str,
    inputs: &[(&str, f64)],
    outputs: &[(&str, f64)],
) -> crate::load::LoadedRecipe {
    crate::load::LoadedRecipe {
        path: std::path::PathBuf::from(format!("{id}.ron")),
        def: crate::recipe::RecipeDef {
            schema_version: crate::recipe::RECIPE_SCHEMA_VERSION,
            id: id.into(),
            display_name: "recipe.test.name".into(),
            inputs: inputs.iter().map(|(r, q)| ((*r).into(), *q)).collect(),
            outputs: outputs.iter().map(|(r, q)| ((*r).into(), *q)).collect(),
        },
    }
}

/// C32-F2, the verifier's probe pair: `a 1.0 → b 0.5` +
/// `b 0.5 → a 1.2` — the cycle that minted +0.2 `a`/s through
/// the colony solver — is rejected twice over: `recipe.ba` is
/// net-positive on its own, and the pair is a directed cycle.
/// A self-referential row and a lone net-positive recipe fail
/// too; an acyclic net-negative chain stays green.
#[test]
fn recipe_graph_rejects_cycles_and_net_positive_yield() {
    let (_, _, mut res, _) = colony_fixture();
    for r in ["a", "b"] {
        res.resources.push(crate::resource::ResourceDef {
            id: r.into(),
            display_name: "resource.test".into(),
            mass_per_unit_kg: 1.0,
        });
    }
    // The probe pair — a directed 2-cycle, and `recipe.ba` is
    // independently net-positive.
    let cyclic = crate::load::LoadedRecipes {
        recipes: vec![
            recipe_fixture("recipe.ab", &[("a", 1.0)], &[("b", 0.5)]),
            recipe_fixture("recipe.ba", &[("b", 0.5)], &[("a", 1.2)]),
        ],
    };
    let errs = validate_recipes(&cyclic, &res);
    for id in ["recipe.ab", "recipe.ba"] {
        assert!(
            errs.iter().any(|e| matches!(
                e,
                ValidationError::InvalidRecipe { id: eid, reason }
                    if eid == id && reason.contains("cycle")
            )),
            "{id} must name the cycle: {errs:?}"
        );
    }
    // `recipe.ba`'s 1.2-out for 0.5-in trips the net-yield gate
    // as well — one recipe id, two reasons.
    assert!(
        errs.iter().any(|e| matches!(
            e,
            ValidationError::InvalidRecipe { id, reason }
                if id == "recipe.ba" && reason.contains("net-positive")
        )),
        "{errs:?}"
    );
    // A self-referential row is the degenerate cycle.
    let selfish = crate::load::LoadedRecipes {
        recipes: vec![recipe_fixture(
            "recipe.self",
            &[("a", 1.0)],
            &[("a", 0.5), ("b", 0.2)],
        )],
    };
    let errs = validate_recipes(&selfish, &res);
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::InvalidRecipe { .. })),
        "{errs:?}"
    );
    // A lone ACYCLIC net-positive recipe mints units the same
    // way — the yield gate, not the cycle gate, catches it.
    let mint = crate::load::LoadedRecipes {
        recipes: vec![recipe_fixture(
            "recipe.mint",
            &[("ore", 1.0)],
            &[("metal", 1.5)],
        )],
    };
    let errs = validate_recipes(&mint, &res);
    assert!(
        errs.iter().any(|e| matches!(
            e,
            ValidationError::InvalidRecipe { id, reason }
                if id == "recipe.mint" && reason.contains("net-positive")
        )),
        "{errs:?}"
    );
    // The sanctioned shape: an acyclic chain refining at a loss.
    let chained = crate::load::LoadedRecipes {
        recipes: vec![
            recipe_fixture("recipe.smelt", &[("ore", 1.0)], &[("metal", 0.6)]),
            recipe_fixture("recipe.cast", &[("metal", 1.0)], &[("a", 0.5)]),
        ],
    };
    assert!(
        validate_recipes(&chained, &res).is_empty(),
        "acyclic net-negative chain must stay green"
    );
}

/// `check_id` covers the remaining registry ids: sites, recipes,
/// experiments, and biome legend entries.
#[test]
fn registry_ids_are_well_formed_checked() {
    let (_, mut sites, res, _) = colony_fixture();
    sites.sites[0].id = "Bad Id".into();
    let errs = validate_sites(&sites, &fixture(), &res);
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::MalformedId { .. })),
        "site: {errs:?}"
    );

    let (_, _, res, mut recipes) = colony_fixture();
    recipes.recipes[0].def.id = "Bad Id".into();
    let errs = validate_recipes(&recipes, &res);
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::MalformedId { .. })),
        "recipe: {errs:?}"
    );

    let mut loaded = crate::load::LoadedExperiments {
        experiments: vec![crate::load::LoadedExperiment {
            path: PathBuf::from("Bad Id.ron"),
            def: crate::experiment::ExperimentDef {
                schema_version: crate::experiment::EXPERIMENT_SCHEMA_VERSION,
                id: "Bad Id".into(),
                display_name: "exp.test.name".into(),
                transmit_ratio: 0.5,
                caps: Vec::new(),
            },
        }],
    };
    let errs = validate_experiments(&loaded, &fixture());
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::MalformedId { .. })),
        "experiment: {errs:?}"
    );
    loaded.experiments.clear();

    let mut sys = fixture();
    with_biomes(&mut sys, 1, tiny_png(4, 4, 4, &[0; 16]));
    sys.bodies[1].biomes.as_mut().unwrap().legend.entries[0].id = "Bad Id".into();
    let errs = validate_system(&sys);
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::MalformedId { .. })),
        "biome: {errs:?}"
    );
}

/// The colony `Drill.min_concentration` bound is the part-side
/// `[0, 1]` — a `> 1` author permanently caps drill output.
#[test]
fn colony_drill_min_concentration_bound_matches_part_side() {
    let (mut file, sites, res, recipes) = colony_fixture();
    file.colonies[0].modules[0] = crate::colony::ColonyModuleDef::Drill {
        resource: "ore".into(),
        base_rate_per_s: 1.0,
        min_concentration: 1.5,
        power_ec_per_s: 1.0,
    };
    let errs = validate_colonies(&file, &sites, &res, &recipes);
    assert!(
        errs.iter()
            .any(|e| matches!(e, ValidationError::InvalidColony { .. })),
        "{errs:?}"
    );
    for edge in [0.0, 1.0] {
        file.colonies[0].modules[0] = crate::colony::ColonyModuleDef::Drill {
            resource: "ore".into(),
            base_rate_per_s: 1.0,
            min_concentration: edge,
            power_ec_per_s: 1.0,
        };
        assert!(
            validate_colonies(&file, &sites, &res, &recipes).is_empty(),
            "edge {edge} must stay legal"
        );
    }
}

/// §3.1 `Parachute`: sane bounds pass; every degenerate family is
/// flagged (non-positive ceilings, negative areas, deployed ≤
/// packed, negative ramp, a second module).
#[test]
fn parachute_module_bounds() {
    let good = |deployed_drag_area_m2| {
        let mut d = part_fixture();
        d.category = PartCategory::Utility;
        d.modules = vec![ModuleDef::Parachute {
            deploy_alt_m: 6_000.0,
            deploy_speed_mps: 260.0,
            packed_drag_area_m2: 0.4,
            deployed_drag_area_m2,
            deploy_time_s: 1.5,
        }];
        d
    };
    assert!(part_errors(good(120.0)).is_empty());
    // Zero ramp = instant deploy — legal.
    let mut d = good(120.0);
    let ModuleDef::Parachute { deploy_time_s, .. } = &mut d.modules[0] else {
        unreachable!()
    };
    *deploy_time_s = 0.0;
    assert!(part_errors(d).is_empty());
    // Deployed not exceeding packed is a no-op decelerator.
    let errs = part_errors(good(0.3));
    assert!(
        errs.iter().any(|e| matches!(
            e,
            ValidationError::BadModule {
                reason: "parachute deployed_drag_area_m2 must exceed packed_drag_area_m2",
                ..
            }
        )),
        "{errs:?}"
    );
    // Non-positive ceilings.
    for (alt, speed) in [(0.0, 260.0), (6_000.0, -1.0)] {
        let mut d = good(120.0);
        let ModuleDef::Parachute {
            deploy_alt_m,
            deploy_speed_mps,
            ..
        } = &mut d.modules[0]
        else {
            unreachable!()
        };
        *deploy_alt_m = alt;
        *deploy_speed_mps = speed;
        assert!(
            part_errors(d).iter().any(|e| matches!(
                e,
                ValidationError::BadModule { .. } | ValidationError::PartNonPositive { .. }
            )),
            "alt {alt} speed {speed} must be rejected"
        );
    }
    // Negative packed area / ramp.
    let mut d = good(120.0);
    let ModuleDef::Parachute {
        packed_drag_area_m2,
        deploy_time_s,
        ..
    } = &mut d.modules[0]
    else {
        unreachable!()
    };
    *packed_drag_area_m2 = -0.5;
    *deploy_time_s = -1.0;
    assert_eq!(
        part_errors(d)
            .iter()
            .filter(|e| matches!(e, ValidationError::BadModule { .. }))
            .count(),
        2
    );
    // Two decelerators on one part — ambiguous.
    let mut d = good(120.0);
    d.modules.push(d.modules[0].clone());
    let errs = part_errors(d);
    assert!(
        errs.iter().any(|e| matches!(
            e,
            ValidationError::BadModule {
                reason: "more than one parachute module",
                ..
            }
        )),
        "{errs:?}"
    );
}

/// §3.1 `Lab`: the authored lab's module shape passes (Utility
/// bucket, positive throughput/multiplier, seats for the
/// researchers); the degenerate fields are flagged.
#[test]
fn lab_module_bounds() {
    let lab_part = || {
        let mut d = part_fixture();
        d.id = "test.lab".into();
        d.category = PartCategory::Utility;
        d.modules = vec![
            ModuleDef::CrewSeat { slots: 2 },
            ModuleDef::Lab {
                throughput_data_per_s: 0.5,
                multiplier: 2.0,
                researcher_slots: 2,
            },
        ];
        d
    };
    assert!(part_errors(lab_part()).is_empty());
    // Non-positive throughput / multiplier.
    for (t, m) in [(0.0, 2.0), (-1.0, 2.0), (0.5, 0.0), (f64::NAN, 2.0)] {
        let mut d = lab_part();
        let ModuleDef::Lab {
            throughput_data_per_s,
            multiplier,
            ..
        } = &mut d.modules[1]
        else {
            unreachable!()
        };
        *throughput_data_per_s = t;
        *multiplier = m;
        assert!(
            part_errors(d).iter().any(|e| matches!(
                e,
                ValidationError::BadModule { .. }
                    | ValidationError::PartNonPositive { .. }
                    | ValidationError::PartNonFinite { .. }
            )),
            "throughput {t} multiplier {m} must be rejected"
        );
    }
    // A lab nobody can staff is dead content.
    let mut d = lab_part();
    let ModuleDef::Lab {
        researcher_slots, ..
    } = &mut d.modules[1]
    else {
        unreachable!()
    };
    *researcher_slots = 0;
    let errs = part_errors(d);
    assert!(
        errs.iter().any(|e| matches!(
            e,
            ValidationError::BadModule {
                reason: "lab with zero researcher slots",
                ..
            }
        )),
        "{errs:?}"
    );
}

/// §3.1 `DockPort`: a port naming a real attach node is a valid
/// part (and satisfies the Payload category rule); a port naming
/// no node, or two ports claiming one node, is rejected.
#[test]
fn dock_port_module_rules() {
    let port_part = |node: &str| {
        let mut d = part_fixture();
        d.id = "test.dockport".into();
        d.category = PartCategory::Payload;
        d.attach_nodes.push(AttachNode {
            id: "mount".into(),
            position_m: csp_core::DVec3::new(0.0, 0.0, -0.9),
            direction: -csp_core::DVec3::Z,
            diameter_m: 1.25,
        });
        d.modules = vec![ModuleDef::DockPort {
            size: crate::part::PortSize::Medium,
            node: node.into(),
        }];
        d
    };
    // "top" is the fixture's only authored node.
    assert!(part_errors(port_part("top")).is_empty());
    // A port face that names no attach node anchors nothing —
    // and the diagnostic names the offending node (C32-F8).
    let errs = part_errors(port_part("ghost"));
    assert!(
        errs.iter().any(|e| matches!(
            e,
            ValidationError::BadModuleNode {
                node,
                reason: "dock_port node names no attach node",
                ..
            } if node == "ghost"
        )),
        "{errs:?}"
    );
    // Two ports, one face — ambiguous authored data, again named.
    let mut d = port_part("top");
    d.modules.push(d.modules[0].clone());
    let errs = part_errors(d);
    assert!(
        errs.iter().any(|e| matches!(
            e,
            ValidationError::BadModuleNode {
                node,
                reason: "two dock ports claim the same attach node",
                ..
            } if node == "top"
        )),
        "{errs:?}"
    );
}
