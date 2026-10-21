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

/// The `version =` line of `pkg`'s `[[package]]` block. Cargo.lock lists
/// `name` before `version` in each block; the first match wins (a duplicate
/// version would be a cargo-deny finding, not a fingerprint concern).
fn locked_version(lock: &str, pkg: &str) -> Option<String> {
    let needle = format!("name = \"{pkg}\"");
    let mut found = false;
    for line in lock.lines().map(str::trim) {
        if line == "[[package]]" {
            found = false;
        } else if line == needle {
            found = true;
        } else if found && let Some(rest) = line.strip_prefix("version = \"") {
            return rest.strip_suffix('"').map(str::to_string);
        }
    }
    None
}
