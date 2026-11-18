//! Loader-side gates (C31-F2/F3/F4): the authored-name guard's Windows
//! cases, the manifest-vs-tree orphan sweep, and the per-file-type
//! version bands — exercised over `EmbeddedSource` so no filesystem
//! fixture is needed.

#![forbid(unsafe_code)]
// Tests signal failure by panicking — §2.13's no-panic rule is for the
// shipping fallible paths, so unwrap/expect are allowed in test code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use csp_data::{
    EmbeddedSource, LoadError, ValidationError, load_loc_from, load_system_from, validate_system,
};

/// A one-body system on the embedded source: `system.ron` manifest
/// plus `<body>/body.ron`, with whatever extra entries `extra` adds.
fn system_source(body_names: &[&str], extra: &[(&'static str, &'static [u8])]) -> EmbeddedSource {
    let bodies = body_names
        .iter()
        .map(|b| format!("\"{b}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let mut files: Vec<(&'static str, &'static [u8])> = vec![(
        "sys/system.ron",
        Box::leak(
            format!(
                r#"(schema_version: 1, id: "sys", star: "{}", bodies: [{bodies}], epoch_note: "k")"#,
                body_names[0]
            )
            .into_bytes()
            .into_boxed_slice(),
        ),
    )];
    for b in body_names {
        files.push((
            Box::leak(format!("sys/{b}/body.ron").into_boxed_str()),
            Box::leak(
                format!(
                    r#"(schema_version: 1, id: "{b}", display_name: "k", parent: None,
                    radius_m: 1.0, mu_m3_per_s2: 1.0, soi_m: None, rotation_period_s: Some(1.0),
                    tilt_rad: 0.0, orbit: None, atmosphere: None, ocean: None,
                    terrain: (noise_seed: 0, heightmap: None),
                    biome_map: None, surface_resources: [], insolation_w_m2: None)"#
                )
                .into_bytes()
                .into_boxed_slice(),
            ),
        ));
    }
    files.extend_from_slice(extra);
    EmbeddedSource::new(Box::leak(files.into_boxed_slice()))
}

/// C31-F4: an authored `system.bodies` name that is drive-prefixed or
/// a DOS device name is refused before the join — on Windows
/// `dir.join("C:x")` drops the base entirely (drive-relative escape),
/// and `con`/`nul`/`com1` resolve to devices in every directory. The
/// guard applies on every platform so the data set stays portable.
#[test]
fn windows_unsafe_names_are_rejected() {
    for bad in [
        "C:x", "d:dir", "con", "CON", "nul.png", "com1", "lpt9", "aux",
    ] {
        let src = system_source(&[bad], &[]);
        let err = load_system_from(&src, Path::new("sys")).unwrap_err();
        assert!(
            matches!(err, LoadError::UnsafeName { .. }),
            "{bad:?} -> {err:?}"
        );
    }
    // The near-misses stay legal: a colon-free single segment, a
    // device-ish prefix, and a 5-char COM name are tame names.
    let src = system_source(&["com", "com10", "auxiliary", "a.b"], &[]);
    let sys = load_system_from(&src, Path::new("sys")).unwrap();
    assert_eq!(sys.bodies.len(), 4);
}

/// C31-F3: content the manifest never references surfaces on
/// `LoadedSystem::orphans` and fails `validate_system` — an unlisted
/// body directory (a body that loads nowhere) and a stray file inside
/// a listed body's dir (the `biomes.png` beside a `biome_map: None`
/// body is the classic trap).
#[test]
fn orphan_content_is_flagged() {
    let src = system_source(
        &["a"],
        &[
            ("sys/ghost/body.ron", b"" as &[u8]), // unlisted body dir
            ("sys/a/biomes.png", b"" as &[u8]),   // stray map beside biome_map: None
            ("sys/a/art-src", b"" as &[u8]),      // unreferenced file
            ("sys/sites.ron", b"" as &[u8]),      // legit system-scope file — NOT an orphan
        ],
    );
    let sys = load_system_from(&src, Path::new("sys")).unwrap();
    let mut orphan_strs: Vec<String> = sys
        .orphans
        .iter()
        .map(|p| p.display().to_string())
        .collect();
    orphan_strs.sort();
    assert_eq!(
        orphan_strs,
        ["sys/a/art-src", "sys/a/biomes.png", "sys/ghost"]
    );
    let errs = validate_system(&sys);
    let flagged: Vec<&String> = errs
        .iter()
        .filter_map(|e| match e {
            ValidationError::OrphanContent { path } => Some(path),
            _ => None,
        })
        .collect();
    assert_eq!(flagged.len(), 3, "{errs:?}");
}

/// The version-band report (C31-F2): `check_version` now carries the
/// whole accepted band — `sites.ron`'s [2, 3] renders both versions,
/// an exact-match file renders the single version.
#[test]
fn version_errors_report_the_accepted_band() {
    // sites.ron band: v1 is two back — error names v2 AND v3.
    let src = EmbeddedSource::new(&[("sites.ron", br#"(schema_version: 1, sites: [])"# as &[u8])]);
    let err = csp_data::load_sites_from(&src, Path::new("sites.ron")).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("v2") && msg.contains("v3"), "{msg}");
    assert!(
        matches!(err, LoadError::UnsupportedSchemaVersion { found: 1, .. }),
        "{err:?}"
    );

    // loc/en.ron is exact-match v1 — the band report carries just v1.
    let src = EmbeddedSource::new(&[("en.ron", br#"(schema_version: 7, strings: {})"# as &[u8])]);
    let err = load_loc_from(&src, Path::new("en.ron")).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("v1") && !msg.contains("v2"), "{msg}");
}

/// C31-F2's constant split: `body.ron`, `biomes.ron`, and `en.ron`
/// each carry their OWN version constant — a `body.ron` bump refuses
/// only body files, never the manifest/legend/loc siblings that used
/// to share `system::SCHEMA_VERSION`. (body.ron now carries M5's
/// `[1, 2]` band, so the refused probe is v3 — two back.)
#[test]
fn version_namespaces_are_per_file_type() {
    // A v3 body.ron inside an otherwise-clean system is refused with
    // the BODY band, not the system file's.
    let src = EmbeddedSource::new(&[
        (
            "sys/system.ron",
            br#"(schema_version: 1, id: "sys", star: "a", bodies: ["a"], epoch_note: "k")"#
                as &[u8],
        ),
        (
            "sys/a/body.ron",
            br#"(schema_version: 3, id: "a", display_name: "k", parent: None,
                radius_m: 1.0, mu_m3_per_s2: 1.0, soi_m: None, rotation_period_s: Some(1.0),
                tilt_rad: 0.0, orbit: None, atmosphere: None, ocean: None,
                terrain: (noise_seed: 0, heightmap: None),
                biome_map: None, surface_resources: [], insolation_w_m2: None)"#
                as &[u8],
        ),
    ]);
    let err = load_system_from(&src, Path::new("sys")).unwrap_err();
    assert!(
        matches!(err, LoadError::UnsupportedSchemaVersion { found: 3, .. }),
        "{err:?}"
    );
}
