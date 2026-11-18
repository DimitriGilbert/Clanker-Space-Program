//! M5/§5.2-R2 terrain-authoring gates: the `TerrainDef` additions
//! (`amplitude_frac`, `relief`) exercised over `EmbeddedSource` —
//! v1-file compat (both fields read their serde defaults), explicit
//! authoring of the new fields, the closed profile set, and the
//! amplitude validation band on the loader→validator round trip.

#![forbid(unsafe_code)]
// Tests signal failure by panicking — §2.13's no-panic rule is for the
// shipping fallible paths, so unwrap/expect are allowed in test code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use csp_core::terrain::{AMPLITUDE_FRAC, ReliefProfile};
use csp_data::{EmbeddedSource, LoadError, ValidationError, load_system_from, validate_system};

/// A one-body system whose `body.ron` carries the given
/// `schema_version` and `terrain` block verbatim.
fn body_with(schema_version: u32, terrain: &str) -> EmbeddedSource {
    let body = format!(
        r#"(schema_version: {schema_version}, id: "a", display_name: "k", parent: None,
            radius_m: 1.0, mu_m3_per_s2: 1.0, soi_m: None, rotation_period_s: Some(1.0),
            tilt_rad: 0.0, orbit: None, atmosphere: None, ocean: None,
            terrain: ({terrain}),
            biome_map: None, surface_resources: [], insolation_w_m2: None)"#
    );
    EmbeddedSource::new(Box::leak(
        vec![
            (
                "sys/system.ron",
                br#"(schema_version: 1, id: "sys", star: "a", bodies: ["a"], epoch_note: "k")"#
                    as &[u8],
            ),
            (
                "sys/a/body.ron",
                Box::leak(body.into_bytes().into_boxed_slice()),
            ),
        ]
        .into_boxed_slice(),
    ))
}

/// Compat (M5, §5.2-R2): a v1 `body.ron` authored before the fields
/// existed loads unchanged and reads the legacy defaults — the
/// amplitude is bit-identical to `csp_core::terrain::AMPLITUDE_FRAC`
/// (imported, not duplicated) and the profile is `Legacy`, so every
/// existing world bakes the exact pre-overhaul field.
#[test]
fn v1_body_without_terrain_additions_reads_legacy_defaults() {
    let src = body_with(1, "noise_seed: 7, heightmap: None");
    let sys = load_system_from(&src, Path::new("sys")).unwrap();
    let t = &sys.bodies[0].def.terrain;
    assert_eq!(t.amplitude_frac.to_bits(), AMPLITUDE_FRAC.to_bits());
    assert_eq!(t.relief, ReliefProfile::Legacy);
    assert!(validate_system(&sys).is_empty());
}

/// A v2 file authors the additions explicitly — a different amplitude
/// and the Mountain profile survive the round trip and validate green.
#[test]
fn v2_body_authoring_the_terrain_additions_loads() {
    let src = body_with(
        2,
        "noise_seed: 7, heightmap: None, amplitude_frac: 0.001, relief: Mountain",
    );
    let sys = load_system_from(&src, Path::new("sys")).unwrap();
    let t = &sys.bodies[0].def.terrain;
    assert_eq!(t.amplitude_frac, 0.001);
    assert_eq!(t.relief, ReliefProfile::Mountain);
    assert!(validate_system(&sys).is_empty());
}

/// A v1 file may author the new fields too — serde defaults are about
/// OMISSION, not the version tag, and the band's only v1→v2
/// distinction is which fields the file is expected to know.
#[test]
fn v1_body_authoring_the_terrain_additions_loads() {
    let src = body_with(
        1,
        "noise_seed: 7, heightmap: None, amplitude_frac: 0.002, relief: Mountain",
    );
    let sys = load_system_from(&src, Path::new("sys")).unwrap();
    let t = &sys.bodies[0].def.terrain;
    assert_eq!(t.amplitude_frac, 0.002);
    assert_eq!(t.relief, ReliefProfile::Mountain);
    assert!(validate_system(&sys).is_empty());
}

/// The profile set is closed: an unknown variant is refused at parse
/// time (serde's unknown-variant error — the same gate every closed
/// data enum here relies on, e.g. `Situation`), never silently read
/// as a default.
#[test]
fn unknown_relief_variant_is_refused_at_parse() {
    let src = body_with(2, "noise_seed: 7, heightmap: None, relief: Volcano");
    let err = load_system_from(&src, Path::new("sys")).unwrap_err();
    assert!(matches!(err, LoadError::Ron { .. }), "{err:?}");
}

/// The amplitude band on the loader→validator round trip: above the
/// 0.02 ceiling and below 0 are rejected with the dedicated variant,
/// NaN with the `NonFinite` stamp, and 0.0 (a flat body) stays green.
#[test]
fn terrain_amplitude_band_round_trips_into_validation() {
    for (amp, expect_non_finite) in [
        ("0.03", false),
        ("0.2", false),
        ("-0.004", false),
        ("NaN", true),
    ] {
        let terrain = format!("noise_seed: 7, heightmap: None, amplitude_frac: {amp}");
        let src = body_with(2, &terrain);
        let sys = load_system_from(&src, Path::new("sys")).unwrap();
        let errs = validate_system(&sys);
        let flagged = errs.iter().any(|e| match e {
            ValidationError::TerrainAmplitudeOutOfRange { .. } => !expect_non_finite,
            ValidationError::NonFinite {
                field: "terrain.amplitude_frac",
                ..
            } => expect_non_finite,
            _ => false,
        });
        assert!(flagged, "amplitude_frac={amp}: {errs:?}");
    }
    let src = body_with(2, "noise_seed: 7, heightmap: None, amplitude_frac: 0.0");
    let sys = load_system_from(&src, Path::new("sys")).unwrap();
    assert!(validate_system(&sys).is_empty());
}
