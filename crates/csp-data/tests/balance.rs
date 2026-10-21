//! `data/balance.ron` loads green — the §8.3 tuning file's file-shape gate.

#![forbid(unsafe_code)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap_or_else(|_| Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
}

/// The authored balance file parses, versions correctly, and carries the
/// §2.6.5 event budget the sim mirrors.
#[test]
fn balance_ron_loads_and_carries_the_event_budget() {
    let b = csp_data::load_balance(&repo_root().join("data/balance.ron")).unwrap();
    assert_eq!(b.schema_version, csp_data::BALANCE_SCHEMA_VERSION);
    assert_eq!(b.advance_to_max_events_per_frame, 2_000);
    assert!(b.bubble_radius_m > 0.0);
}
