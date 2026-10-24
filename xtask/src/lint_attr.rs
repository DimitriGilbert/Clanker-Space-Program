//! `cargo xtask lint-attribution` — the PLAN §3.7 asset-attribution gate
//! (§8.2 row "xtask lint-attribution").
//!
//! §3.7: "`xtask lint-attribution` fails if any file under `data/` or
//! `assets/` with a non-authored extension lacks a matching line, or if any
//! line's license is not on the allowlist." Line format:
//! `path | source | license | url`.
//!
//! The authored/non-authored split is explicit, declared inside
//! `data/ATTRIBUTION.md`'s own comment header so the file documents its
//! convention:
//!
//! ```text
//! # authored-extensions: .ron .md ...
//! # authored-names: .gitkeep LICENSE ATTRIBUTION.md
//! ```
//!
//! Matching is by filename SUFFIX — `Path::extension` cannot express
//! `.ron.disabled` (its extension is `disabled`) or extensionless names
//! like `LICENSE`/`.gitkeep` (the old blanket `.ron`/`.md` exemption hid
//! that bug).
//!
//! [`check_attribution`] is pure over `(file_text, file_listing)` so
//! fixture trees can prove the rules fire.

use std::path::Path;
use std::process::exit;

/// §3.7 asset-license allowlist — consistent with the §1.2 proprietary
/// stance and the deny.toml code-license list: NC and share-alike licenses
/// (CC-BY-NC, CC-BY-SA, GPL) are categorically unusable, so plain CC-BY is
/// the only copyleft-adjacent entry (attribution is given by the row
/// itself). This is a name set, not cargo-deny's SPDX evaluation.
const ALLOWED_ASSET_LICENSES: &[&str] = &[
    "CC0-1.0",
    "public-domain",
    "MIT",
    "Apache-2.0",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "Zlib",
    "BSL-1.0",
    "OFL-1.1",
    "Unlicense",
    "CC-BY-3.0",
    "CC-BY-4.0",
];

/// Header markers the lint recognizes inside ATTRIBUTION.md comments.
const EXT_MARKER: &str = "authored-extensions:";
const NAME_MARKER: &str = "authored-names:";

/// One parsed `path | source | license | url` row.
#[derive(Debug)]
pub struct AttrEntry {
    /// Repo-relative path (field 1), e.g. `data/parts/x.png`.
    pub path: String,
    /// Field 3 — validated against [`ALLOWED_ASSET_LICENSES`].
    pub license: String,
}

/// The parsed ATTRIBUTION.md.
#[derive(Debug, Default)]
pub struct AttrFile {
    /// Filename suffixes that mark a file as own-authored.
    pub authored_exts: Vec<String>,
    /// Exact filenames that mark a file as own-authored.
    pub authored_names: Vec<String>,
    /// Attribution rows.
    pub entries: Vec<AttrEntry>,
}

/// A single attribution-rule violation.
#[derive(Debug)]
pub struct Violation {
    /// Which rule fired.
    pub rule: &'static str,
    /// Offending file or line.
    pub detail: String,
}

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.rule, self.detail)
    }
}

/// Parse ATTRIBUTION.md's text: comment markers populate the authored sets,
/// non-comment lines must be 4-field rows. Returns `Err` on a malformed
/// row (row syntax errors are hard failures, not collected violations —
/// the file would be ambiguous to maintain).
pub fn parse_attribution(text: &str) -> Result<AttrFile, String> {
    let mut out = AttrFile::default();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix('#') {
            let rest = rest.trim();
            if let Some(list) = rest.strip_prefix(EXT_MARKER) {
                out.authored_exts
                    .extend(list.split_whitespace().map(str::to_string));
            } else if let Some(list) = rest.strip_prefix(NAME_MARKER) {
                out.authored_names
                    .extend(list.split_whitespace().map(str::to_string));
            }
            continue;
        }
        let parts: Vec<&str> = line.split('|').map(str::trim).collect();
        if parts.len() != 4 || parts.iter().any(|p| p.is_empty()) {
            return Err(format!(
                "ATTRIBUTION.md line {}: need 'path | source | license | url', \
                 got {line:?}",
                n + 1
            ));
        }
        out.entries.push(AttrEntry {
            path: parts[0].to_string(),
            license: parts[2].to_string(),
        });
    }
    Ok(out)
}

/// Whether `file_name` (basename) is own-authored per the declared markers.
/// `ATTRIBUTION.md` is always authored — it is the ledger itself.
fn is_authored(attr: &AttrFile, file_name: &str) -> bool {
    file_name == "ATTRIBUTION.md"
        || attr.authored_names.iter().any(|n| n == file_name)
        || attr
            .authored_exts
            .iter()
            .any(|e| file_name.ends_with(e.as_str()))
}

/// The pure rule engine: `files` is the recursive repo-relative listing
/// under `data/` (and `assets/` when it exists), e.g.
/// `data/parts/tank.png`.
pub fn check_attribution(text: &str, files: &[String]) -> Vec<Violation> {
    let mut out = Vec::new();
    let attr = match parse_attribution(text) {
        Ok(a) => a,
        Err(e) => {
            out.push(Violation {
                rule: "bad-row",
                detail: e,
            });
            return out;
        }
    };
    if attr.authored_exts.is_empty() && attr.authored_names.is_empty() {
        out.push(Violation {
            rule: "no-authored-markers",
            detail: "ATTRIBUTION.md declares no '# authored-extensions:' / \
                     '# authored-names:' markers — the authored-file \
                     convention must be explicit (§3.7)"
                .to_string(),
        });
    }
    // Rows: allowlisted license, in-scope path, non-stale.
    for e in &attr.entries {
        if !ALLOWED_ASSET_LICENSES.contains(&e.license.as_str()) {
            out.push(Violation {
                rule: "license-not-allowlisted",
                detail: format!(
                    "{} has license {:?} — not on the §3.7 allowlist",
                    e.path, e.license
                ),
            });
        }
        if !e.path.starts_with("data/") && !e.path.starts_with("assets/") {
            out.push(Violation {
                rule: "entry-out-of-scope",
                detail: format!("attribution row for {:?} outside data//assets/", e.path),
            });
        } else if !files.iter().any(|f| f == &e.path) {
            out.push(Violation {
                rule: "stale-entry",
                detail: format!("attribution row for missing file {:?}", e.path),
            });
        }
    }
    // Files: every non-authored file needs a row.
    for f in files {
        let name = f.rsplit('/').next().unwrap_or(f.as_str());
        if !is_authored(&attr, name) && !attr.entries.iter().any(|e| &e.path == f) {
            out.push(Violation {
                rule: "missing-attribution",
                detail: format!("{f} has no attribution row and no authored marker"),
            });
        }
    }
    out
}

/// Recursively collect repo-relative (`data/...`) paths of every file
/// under `dir` — all subdirs, no extension filtering.
fn collect_files(dir: &Path, prefix: &str, out: &mut Vec<String>) {
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    for e in read.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if p.is_dir() {
            collect_files(&p, &format!("{prefix}/{name}"), out);
        } else {
            out.push(format!("{prefix}/{name}"));
        }
    }
}

/// `cargo xtask lint-attribution`.
// See lint_deps::tree — tooling failure is loud failure here (§8.2).
#[allow(clippy::expect_used)]
pub fn lint_attribution() {
    // Anchor to the manifest dir like the bless-goldens/gen_biome_maps
    // siblings — CWD-relative paths would panic (or silently lint the
    // wrong tree) when invoked outside the repo root.
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let attr = std::fs::read_to_string(root.join("data/ATTRIBUTION.md"))
        .expect("data/ATTRIBUTION.md missing");
    let mut files = Vec::new();
    collect_files(&root.join("data"), "data", &mut files);
    // §3.7 covers assets/ too; it does not exist yet — walk it when it does.
    if root.join("assets").is_dir() {
        collect_files(&root.join("assets"), "assets", &mut files);
    }
    let violations = check_attribution(&attr, &files);
    if violations.is_empty() {
        println!("lint-attribution: OK");
    } else {
        for v in &violations {
            eprintln!("VIOLATION: {v}");
        }
        eprintln!("lint-attribution: {} violation(s)", violations.len());
        exit(1);
    }
}

// Test code may unwrap — it is the failure path either way.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    // `.ron.disabled` is listed explicitly: the authored check is a
    // filename-SUFFIX match, so a compound suffix must be named in full
    // (the old `Path::extension` lookup could only ever see `disabled`).
    const HEADER: &str = "# ledger\n\
                          # authored-extensions: .ron .md .ron.disabled\n\
                          # authored-names: .gitkeep LICENSE\n";

    #[test]
    fn authored_files_need_no_row() {
        let files = vec![
            "data/ATTRIBUTION.md".to_string(),
            "data/balance.ron".to_string(),
            "data/LICENSE".to_string(),
            "data/parts/.gitkeep".to_string(),
        ];
        assert!(check_attribution(HEADER, &files).is_empty());
    }

    #[test]
    fn non_authored_file_needs_an_allowlisted_row() {
        let files = vec!["data/parts/tank.png".to_string()];
        // Missing entirely.
        assert!(
            check_attribution(HEADER, &files)
                .iter()
                .any(|v| v.rule == "missing-attribution")
        );
        // Present but a banned license.
        let bad = format!("{HEADER}data/parts/tank.png | kenney | CC-BY-NC-4.0 | https://x\n");
        let v = check_attribution(&bad, &files);
        assert!(v.iter().any(|x| x.rule == "license-not-allowlisted"));
        // Present, allowed license — passes.
        let ok = format!("{HEADER}data/parts/tank.png | kenney | CC0-1.0 | https://x\n");
        assert!(check_attribution(&ok, &files).is_empty());
    }

    #[test]
    fn compound_suffix_and_stale_rows() {
        // `.ron.disabled` is authored by SUFFIX — `Path::extension` would
        // call it `.disabled` and demand a row.
        let files = vec!["data/balance.ron.disabled".to_string()];
        assert!(check_attribution(HEADER, &files).is_empty());
        // A row for a file that no longer exists is stale.
        let stale = format!("{HEADER}data/gone.png | x | CC0-1.0 | https://x\n");
        assert!(
            check_attribution(&stale, &files)
                .iter()
                .any(|v| v.rule == "stale-entry")
        );
    }

    #[test]
    fn missing_markers_and_bad_rows_fail() {
        // No authored markers at all — the convention must be explicit.
        let files = vec!["data/balance.ron".to_string()];
        assert!(
            check_attribution("# nothing declared\n", &files)
                .iter()
                .any(|v| v.rule == "no-authored-markers")
        );
        // Malformed row → parse failure surfaces as a violation.
        let bad = format!("{HEADER}data/x.png | only-two-fields\n");
        assert!(
            check_attribution(&bad, &files)
                .iter()
                .any(|v| v.rule == "bad-row")
        );
    }
}
