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
/// is ambiguous — that pinned spec is the linked version. Members are
/// consulted in [`PIN_PREFERENCE`] order — a first-found-wins scan is
/// file-order dependent, so a divergent pin in an earlier block could
/// fingerprint a version the determinism domain never linked.
/// Otherwise the highest resolved version wins (a single version is
/// trivially its own max).
fn locked_version(lock: &str, pkg: &str) -> Option<String> {
    let spec = format!("\"{pkg} ");
    let mut versions: Vec<String> = Vec::new();
    // Every member's pinned spec — (member name, pinned version).
    let mut pins: Vec<(String, String)> = Vec::new();
    let mut named = false; // previous line was `name = "pkg"`
    let mut member = false; // inside a workspace (`source`-less) package
    let mut member_name = String::new(); // its `name =` line
    let mut in_deps = false; // inside its `dependencies = [ … ]`
    for line in lock.lines().map(str::trim) {
        if line == "[[package]]" {
            named = false;
            member = true;
            member_name.clear();
            in_deps = false;
            continue;
        }
        if line.starts_with('[') {
            // A non-package table ([metadata], patches) — every piece
            // of block-scoped state resets; foreign `version =`/`"pkg
            // X.Y.Z"` lines must not leak into versions or pins.
            named = false;
            member = false;
            member_name.clear();
            in_deps = false;
            continue;
        }
        if let Some(rest) = line
            .strip_prefix("name = \"")
            .and_then(|r| r.strip_suffix('"'))
        {
            member_name = rest.to_string();
            named = rest == pkg;
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
        } else if in_deps && let Some(rest) = line.trim_end_matches(',').strip_prefix(spec.as_str())
        {
            // `"pkg X.Y.Z"` or `"pkg X.Y.Z (registry+…)"` — the
            // version token is first.
            if let Some(v) = rest.trim_end_matches('"').split_whitespace().next() {
                pins.push((member_name.clone(), v.to_string()));
            }
        }
    }
    // The determinism member's pin is authoritative — it is the build
    // the fingerprint describes. The remaining members fall back to a
    // name-sorted pick so resolution stays deterministic regardless of
    // lockfile block order.
    for preferred in PIN_PREFERENCE {
        if let Some((_, v)) = pins.iter().find(|(name, _)| name == preferred) {
            return Some(v.clone());
        }
    }
    pins.sort();
    pins.dedup();
    pins.into_iter()
        .next()
        .map(|(_, v)| v)
        .or_else(|| versions.iter().max_by(|a, b| semver_cmp(a, b)).cloned())
}

/// Members whose dep spec pins the linked version, in preference
/// order — the §2.1 determinism domain the fingerprint describes.
const PIN_PREFERENCE: &[&str] = &["csp-core", "csp-data", "csp-sim", "csp-phys"];

/// Semver ordering for `"X.Y.Z[-pre][+meta]"` — the numeric core,
/// then pre-release precedence (a release sorts ABOVE every one of
/// its pre-releases: `1.0.0-alpha.1 < 1.0.0`, which the old
/// numeric-runs key got backwards).
fn semver_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let (a_core, a_pre) = split_semver(a);
    let (b_core, b_pre) = split_semver(b);
    a_core
        .cmp(&b_core)
        .then_with(|| prerelease_cmp(a_pre, b_pre))
}

/// Split into the `(major, minor, patch)` numeric triple and the
/// pre-release string (`""` for a release). Build metadata (`+…`)
/// carries no precedence and is dropped.
fn split_semver(v: &str) -> ([u64; 3], &str) {
    let v = v.split('+').next().unwrap_or(v);
    let (core, pre) = v.split_once('-').map_or((v, ""), |(c, p)| (c, p));
    let mut nums = [0u64; 3];
    for (n, part) in nums.iter_mut().zip(core.split('.')) {
        *n = part.parse().unwrap_or(0);
    }
    (nums, pre)
}

/// Semver §11 pre-release precedence: dot-separated identifiers,
/// numeric identifiers below alphanumeric ones, numeric compared
/// numerically, alphanumeric lexically, fewer identifiers below more.
fn prerelease_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a.is_empty(), b.is_empty()) {
        (true, true) => return Ordering::Equal,
        (true, false) => return Ordering::Greater,
        (false, true) => return Ordering::Less,
        (false, false) => {}
    }
    let mut ai = a.split('.');
    let mut bi = b.split('.');
    loop {
        let ord = match (ai.next(), bi.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) => match (x.parse::<u64>(), y.parse::<u64>()) {
                (Ok(xn), Ok(yn)) => xn.cmp(&yn),
                (Ok(_), Err(_)) => Ordering::Less,
                (Err(_), Ok(_)) => Ordering::Greater,
                (Err(_), Err(_)) => x.cmp(y),
            },
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
}
