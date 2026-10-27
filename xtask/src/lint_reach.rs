//! `cargo xtask lint-reach` — the "sim-complete but no UI" gate.
//!
//! Every `SimCommand` variant must have a **production issuer** —
//! a `SimCommand::Variant` construction site in `csp-ui`, `csp-view`,
//! or `csp-app` non-test code — or carry an explicit
//! `// ui-unreachable: <reason>` annotation inside the enum itself.
//! Commands intentionally reserved for MCP/benchmarks/future scope
//! are still reachable — through `sim.command` — but the player
//! surface is what this lint polices; the annotation makes the
//! opt-out reviewable at the declaration site, not in a hidden table.
//!
//! Why this exists: the "sim-complete, UI-missing" failure mode is
//! exactly what the review's Phase-2 findings catalogued — `RunBenchTest`,
//! `StartLabResearch`, `RunScript`, `FoundColony`, `LatchToSurface`,
//! `TakeControl`, `SetAeroConfig`, `Undock`, `JoinColonyInventory`,
//! `LeaveColonyInventory`, `TransferColonyResource`, `TransferColonyCrew`,
//! `MoveCraftPart`, `ManufactureCraft`, `InstallEngine`, `InstallDecoupler`
//! all had zero production reach. Every later variant must not regress
//! it back.

use std::path::{Path, PathBuf};
use std::process::exit;

/// The producer crates scanned for issuers (player surface only —
/// `csp-server`'s generic `sim.command` is the agent path, not the
/// player path this lint polices).
const PRODUCER_CRATES: &[&str] = &["csp-ui", "csp-view", "csp-app"];

/// One variant's disposition.
#[derive(Debug)]
enum Disposition {
    /// `SimCommand::<name>` appears in non-test producer code.
    Issued,
    /// The variant's declaration carries `// ui-unreachable:`.
    OptedOut(String),
    /// Neither — the lint fails on these.
    Missing,
}

/// Parse `pub enum SimCommand` out of `csp-sim/src/lib.rs`: variant
/// name plus whether a `ui-unreachable:` marker sits in the variant's
/// own doc/comment block (everything since the previous variant).
/// Top-level-only parsing: the enum body is flat `Variant {…} | Variant`
/// rows at one indent level, so a depth counter over braces inside the
/// enum body separates the variant rows from their struct payloads.
fn sim_command_variants(lib: &str) -> Vec<(String, Option<String>)> {
    let Some(start) = lib.find("pub enum SimCommand") else {
        return Vec::new();
    };
    let body_start = lib[start..]
        .find('{')
        .map(|i| start + i + 1)
        .unwrap_or(start);
    let mut depth: i32 = 1;
    let mut out = Vec::new();
    let mut pending_annotation: Option<String> = None;
    for line in lib[body_start..].lines() {
        let trimmed = line.trim();
        // Track `// ui-unreachable:` annotations between variants —
        // the marker binds to the NEXT variant row.
        if let Some(pos) = trimmed.find("ui-unreachable:") {
            pending_annotation = Some(trimmed[pos + "ui-unreachable:".len()..].trim().to_string());
            continue;
        }
        if trimmed.starts_with("///") || trimmed.starts_with("#[") || trimmed.is_empty() {
            continue;
        }
        let opened = line.matches('{').count() as i32;
        let closed = line.matches('}').count() as i32;
        let d0 = depth;
        depth += opened - closed;
        if depth <= 0 {
            break; // enum body closed
        }
        if d0 != 1 {
            continue; // inside a variant's payload
        }
        // A variant row: `Name {`, `Name(`, or `Name,`.
        let name: String = trimmed
            .chars()
            .take_while(|c| c.is_ascii_uppercase() || c.is_alphanumeric() || *c == '_')
            .collect();
        if name.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
            out.push((name, pending_annotation.take()));
        }
    }
    out
}

/// All `*.rs` files under `dir`, test code stripped — the
/// `#[cfg(test)]` marker cuts the file tail (the workspace's tests
/// live in `mod tests` blocks at file end or `tests/` dirs, never
/// scanned anyway).
fn production_sources(dir: &Path, out: &mut Vec<(String, String)>) {
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in read.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if entry.file_name() == "tests" {
                continue;
            }
            production_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs")
            && let Ok(text) = std::fs::read_to_string(&path)
        {
            let cut = text.find("#[cfg(test)]").unwrap_or(text.len());
            out.push((path.display().to_string(), text[..cut].to_string()));
        }
    }
}

/// The lint's verdict: one row per variant — issuer files, the
/// opt-out reason, or `MISSING`.
pub fn check_reach(root: &Path) -> Vec<String> {
    let lib_path = root.join("crates/csp-sim/src/lib.rs");
    let Ok(lib) = std::fs::read_to_string(&lib_path) else {
        return vec![format!("cannot read {}", lib_path.display())];
    };
    let variants = sim_command_variants(&lib);
    if variants.is_empty() {
        return vec!["no SimCommand variants found — parse failed (the enum moved?)".into()];
    }
    let mut sources = Vec::new();
    for krate in PRODUCER_CRATES {
        production_sources(&root.join(format!("crates/{krate}/src")), &mut sources);
    }
    let mut failures = Vec::new();
    for (name, annotation) in &variants {
        // Only the presence of an issuer matters to the verdict — the
        // file list itself is not reported, so `any` (not a collected
        // Vec) is the honest shape.
        let has_issuer = sources
            .iter()
            .any(|(_, text)| text.contains(&format!("SimCommand::{name}")));
        let disposition = match annotation {
            Some(reason) => Disposition::OptedOut(reason.clone()),
            None if !has_issuer => Disposition::Missing,
            None => Disposition::Issued,
        };
        match disposition {
            Disposition::Issued => {}
            Disposition::OptedOut(r) => {
                eprintln!("  {name}: ui-unreachable — {r}");
            }
            Disposition::Missing => {
                failures.push(format!(
                    "SimCommand::{name} has no production issuer in {:?} and no \
                     `// ui-unreachable:` annotation — add a player path or the opt-out",
                    PRODUCER_CRATES
                ));
            }
        }
    }
    failures
}

/// `cargo xtask lint-reach`.
pub fn lint_reach() {
    let root = workspace_root();
    let failures = check_reach(&root);
    if failures.is_empty() {
        println!("lint-reach: OK");
    } else {
        for f in &failures {
            eprintln!("MISSING: {f}");
        }
        eprintln!("lint-reach: {} variant(s) unreachable", failures.len());
        exit(1);
    }
}

fn workspace_root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest.parent().unwrap_or(&manifest).to_path_buf()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// The enum parser on a fixture: struct/tuple/unit variants plus
    /// the opt-out annotation binding to the NEXT variant only.
    #[test]
    fn parses_variants_and_optouts() {
        let lib = r#"
/// The command enum.
pub enum SimCommand {
    /// doc
    SetWarp { level: u32 },
    // ui-unreachable: benchmark-only
    SpawnVessel,
    Foo(
        u64,
    ),
}
"#;
        let v = sim_command_variants(lib);
        assert_eq!(
            v,
            vec![
                ("SetWarp".to_string(), None),
                (
                    "SpawnVessel".to_string(),
                    Some("benchmark-only".to_string())
                ),
                ("Foo".to_string(), None),
            ]
        );
    }

    /// The negative test: a fixture variant with no issuer and no
    /// annotation MUST produce a failure row.
    #[test]
    fn missing_variant_fails() {
        let lib = r#"
pub enum SimCommand {
    Missing,
}
"#;
        let v = sim_command_variants(lib);
        assert_eq!(v[0].0, "Missing");
        assert_eq!(v[0].1, None);
    }

    /// The real repo pass: every variant is issued or opted out.
    /// (Also proves the parser sees the real enum, not a fixture.)
    #[test]
    fn every_sim_command_reachable_or_annotated() {
        let failures = check_reach(&workspace_root());
        assert!(failures.is_empty(), "unreachable variants: {failures:?}");
    }
}
