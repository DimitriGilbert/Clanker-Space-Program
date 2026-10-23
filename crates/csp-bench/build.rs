//! Build script for the §7.1 determinism fingerprint.
//!
//! The fingerprint records *what built this binary* — rustc version, target
//! triple, and the exact resolved versions of the determinism-domain deps.
//! None of that is queryable at runtime, so it is captured here into `CSP_*`
//! env vars that `csp_bench::replay::fingerprint` reads via `option_env!`.
//!
//! Dep versions come from the workspace `Cargo.lock` — the only
//! authoritative record of what was actually resolved — parsed with a plain
//! line scan rather than a TOML dependency for six fields.

use std::env;
use std::path::PathBuf;
use std::process::Command;

/// The deps §7.1 names in the fingerprint: crate name → env var suffix the
/// library reads. Absent from the lockfile (parry/rapier until csp-phys
/// lands, M1+) emits an empty value, which the library reports as `None`.
const TRACKED_DEPS: &[(&str, &str)] = &[
    ("parry3d-f64", "CSP_DEP_VERSION_PARRY3D_F64"),
    ("rapier3d-f64", "CSP_DEP_VERSION_RAPIER3D_F64"),
    ("libm", "CSP_DEP_VERSION_LIBM"),
    ("rand_chacha", "CSP_DEP_VERSION_RAND_CHACHA"),
    ("blake3", "CSP_DEP_VERSION_BLAKE3"),
];

fn main() {
    // The compiler driving this build — honored via $RUSTC so cross-builds
    // fingerprint the right toolchain.
    let rustc = env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let rustc_version = Command::new(rustc)
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=CSP_RUSTC_VERSION={rustc_version}");

    let target = env::var("TARGET").unwrap_or_else(|_| "unknown".to_string());
    println!("cargo:rustc-env=CSP_TARGET_TRIPLE={target}");

    // crates/csp-bench -> workspace root.
    let lock = env::var_os("CARGO_MANIFEST_DIR").map_or_else(
        || PathBuf::from("../../Cargo.lock"),
        |dir| PathBuf::from(dir).join("../../Cargo.lock"),
    );
    println!("cargo:rerun-if-changed={}", lock.display());
    let lock_text = std::fs::read_to_string(&lock).unwrap_or_default();
    for &(name, var) in TRACKED_DEPS {
        let version = locked_version(&lock_text, name).unwrap_or_default();
        println!("cargo:rustc-env={var}={version}");
    }
}

/// The version of `pkg` the determinism domain actually links.
///
/// Cargo.lock lists `name` before `version` in each `[[package]]`
/// block, but a *first-match* scan is wrong whenever `pkg` resolves to
/// two versions — a transitive duplicate (e.g. tungstenite's
/// `rand_chacha 0.9.0`) can sort before the workspace's real pick
/// (csp-core's pinned `0.10.0`), and the fingerprint then reports a
/// version nothing in the domain compiled against — a false drift
/// warning at verify time.
///
/// Resolution order: when a workspace member (a `[[package]]` block
/// with no `source =` line) names `pkg` in its `dependencies` as a
/// versioned spec — cargo writes `"pkg X.Y.Z"` exactly when the name
/// is ambiguous — that pinned spec is the linked version. Otherwise
/// the highest resolved version wins (a single version is trivially
/// its own max).
fn locked_version(lock: &str, pkg: &str) -> Option<String> {
    let needle = format!("name = \"{pkg}\"");
    let spec = format!("\"{pkg} ");
    let mut versions: Vec<String> = Vec::new();
    let mut pinned: Option<String> = None;
    let mut named = false; // previous line was `name = "pkg"`
    let mut member = false; // inside a workspace (`source`-less) package
    let mut in_deps = false; // inside its `dependencies = [ … ]`
    for line in lock.lines().map(str::trim) {
        if line == "[[package]]" {
            named = false;
            member = true;
            in_deps = false;
            continue;
        }
        if line.starts_with('[') {
            // A non-package table ([metadata], patches) — member and
            // deps state are block-scoped; foreign dep specs must not
            // leak into the pin.
            member = false;
            in_deps = false;
            continue;
        }
        if line == needle {
            named = true;
        } else if named
            && let Some(rest) = line.strip_prefix("version = \"")
            && let Some(v) = rest.strip_suffix('"')
        {
            versions.push(v.to_string());
            named = false;
        }
        if line.starts_with("source =") {
            member = false;
        } else if member && line.starts_with("dependencies =") {
            in_deps = true;
        } else if in_deps && line.starts_with(']') {
            in_deps = false;
        } else if in_deps
            && pinned.is_none()
            && let Some(rest) = line.trim_end_matches(',').strip_prefix(spec.as_str())
        {
            // `"pkg X.Y.Z"` or `"pkg X.Y.Z (registry+…)"` — the
            // version token is first.
            pinned = rest
                .trim_end_matches('"')
                .split_whitespace()
                .next()
                .map(str::to_string);
        }
    }
    pinned.or_else(|| versions.iter().max_by_key(|v| semver_key(v)).cloned())
}

/// A numeric ordering key for `"X.Y.Z[-pre][+meta]"` — split on
/// non-digits and compare the numeric runs lexicographically. Good
/// enough to pick the max out of a duplicate-version pair.
fn semver_key(v: &str) -> Vec<u64> {
    v.split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty())
        .map(|s| s.parse().unwrap_or(0))
        .collect()
}
