//! Repo lint tasks (PLAN §8.2): `cargo xtask lint-deps`, `lint-attribution`.
//! Replaces the old fragile `cargo tree | grep` one-liners: fails loudly on
//! tooling errors, uses explicit per-crate allow/deny lists.

use std::process::{Command, exit};

/// Determinism-domain crates and the crates that must never appear in
/// their dependency trees (substring match on `cargo tree` package lines).
const RULES: &[(&str, &[&str])] = &[
    (
        "csp-sim",
        &[
            "wgpu",
            "winit",
            "egui",
            "bevy",
            "rand ",
            "rayon",
            "tokio",
            "std::time",
        ],
    ),
    (
        "csp-core",
        &["wgpu", "winit", "egui", "bevy", "rayon", "tokio"],
    ),
    (
        "csp-data",
        &["wgpu", "winit", "egui", "bevy", "rayon", "tokio"],
    ),
    (
        "csp-phys",
        &["wgpu", "winit", "egui", "bevy", "rayon", "tokio"],
    ),
    ("csp-render", &["csp-sim", "csp-view", "bevy"]),
];

fn tree(pkg: &str) -> String {
    let out = Command::new("cargo")
        .args(["tree", "-p", pkg, "--format", "{p}"])
        .output()
        .expect("cargo tree failed to run");
    if !out.status.success() {
        eprintln!("cargo tree -p {pkg} failed");
        exit(2);
    }
    String::from_utf8(out.stdout).expect("cargo tree output not utf-8")
}

fn lint_deps() {
    let mut bad = false;
    for (pkg, banned) in RULES {
        for line in tree(pkg).lines() {
            for b in *banned {
                if line.contains(b) {
                    eprintln!("BANNED: {pkg} depends on {line} (matched '{b}')");
                    bad = true;
                }
            }
        }
    }
    // wasm purity: csp-server and csp-bench must never be in the wasm tree.
    for pkg in ["csp-app", "csp-sim", "csp-view", "csp-render"] {
        let out = Command::new("cargo")
            .args([
                "tree",
                "-p",
                pkg,
                "--target",
                "wasm32-unknown-unknown",
                "--format",
                "{p}",
            ])
            .output()
            .expect("cargo tree wasm failed");
        let s = String::from_utf8(out.stdout).expect("utf8");
        for banned in ["csp-server", "csp-bench"] {
            if s.lines().any(|l| l.starts_with(banned)) {
                eprintln!("WASM PURITY: {pkg} pulls {banned} into wasm tree");
                bad = true;
            }
        }
    }
    if bad {
        exit(1);
    }
    println!("lint-deps: OK");
}

fn lint_attribution() {
    // §3.7: every file under data/ that is not a source/own file needs a line
    // `path | source | license | url` in data/ATTRIBUTION.md.
    let attr = std::fs::read_to_string("data/ATTRIBUTION.md").expect("data/ATTRIBUTION.md missing");
    let mut entries = std::collections::HashSet::new();
    for line in attr.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.split('|').map(|p| p.trim()).collect();
        if parts.len() != 4 {
            eprintln!("ATTRIBUTION bad line (need 'path | source | license | url'): {line}");
            exit(1);
        }
        entries.insert(parts[0].to_string());
    }
    let mut bad = false;
    for e in std::fs::read_dir("data").expect("data/ missing").flatten() {
        let p = e.path();
        if p.is_dir() || p.file_name().unwrap() == "ATTRIBUTION.md" {
            continue;
        }
        // own-authored sources: .ron/.md data files don't need attribution.
        match p.extension().and_then(|x| x.to_str()) {
            Some("ron" | "md" | "gitkeep" | "ron.disabled") => {}
            _ => {
                let rel = p.to_string_lossy().to_string();
                if !entries.contains(rel.as_str()) {
                    eprintln!("ATTRIBUTION missing for {rel}");
                    bad = true;
                }
            }
        }
    }
    if bad {
        exit(1);
    }
    println!("lint-attribution: OK");
}

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("lint-deps") => lint_deps(),
        Some("lint-attribution") => lint_attribution(),
        _ => {
            eprintln!("usage: cargo xtask <lint-deps|lint-attribution>");
            exit(2);
        }
    }
}
