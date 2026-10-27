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

/// Split `src` into two byte-aligned shadows, both the same length as
/// `src` with newlines preserved (so `lines()` stays index-aligned):
///
/// - `code` keeps compilable bytes and blanks comments plus the
///   contents of string/char literals to spaces.
/// - `comments` keeps `// …`, `//! …`, `/// …`, and `/* … */` text and
///   blanks everything else.
///
/// The lint consumes both: issuer search runs on `code` so a
/// `SimCommand::X` inside a doc comment or a string literal is a
/// *mention*, not a construction; the enum parser runs brace
/// accounting on `code` (a `{`/`}` inside a comment must not move the
/// depth counter) and the `ui-unreachable:` hunt on `comments` (the
/// same text inside a string is not an annotation).
fn split_code_and_comments(src: &str) -> (String, String) {
    let b = src.as_bytes();
    let n = b.len();
    let mut code = vec![b' '; n];
    let mut comments = vec![b' '; n];
    let mut i = 0;
    while i < n {
        match b[i] {
            b'\n' => {
                code[i] = b'\n';
                comments[i] = b'\n';
                i += 1;
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                // Line comment (`//`, `///`, `//!`) to end of line.
                while i < n && b[i] != b'\n' {
                    comments[i] = b[i];
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                // Block comment — Rust allows them to nest.
                let mut depth = 0usize;
                while i < n {
                    if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
                        comments[i] = b[i];
                        comments[i + 1] = b[i + 1];
                        depth += 1;
                        i += 2;
                    } else if b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
                        comments[i] = b[i];
                        comments[i + 1] = b[i + 1];
                        depth -= 1;
                        i += 2;
                        if depth == 0 {
                            break;
                        }
                    } else if b[i] == b'\n' {
                        code[i] = b'\n';
                        comments[i] = b'\n';
                        i += 1;
                    } else {
                        comments[i] = b[i];
                        i += 1;
                    }
                }
            }
            b'"' | b'b' | b'r' => match string_literal_start(b, i) {
                Some((quote, hashes, raw)) => {
                    // The literal's bytes stay blank in both shadows;
                    // only its extent is scanned.
                    i = string_literal_end(b, quote + 1, hashes, raw, &mut code, &mut comments);
                }
                // An identifier char (`b`, `r`, `bravo`, `r#ident`)
                // that does not open a literal is plain code.
                None => {
                    code[i] = b[i];
                    i += 1;
                }
            },
            b'\'' => {
                // `'x'` / `'\n'` / `'\u{…}'` literals vs `'a` lifetimes:
                // a char literal closes with a second `'` a few bytes
                // later on the same line; a lifetime never does. A
                // `'}'` literal must not reach the enum depth counter.
                let close = if b.get(i + 1) == Some(&b'\\') {
                    // The escaped char occupies i+2 (it may itself be a
                    // quote: `'\''`), so the closer sits at i+3+.
                    closing_quote(b, i + 3, i + 32)
                } else {
                    // 1–4-byte char: the closer lands at i+2..=i+5.
                    closing_quote(b, i + 2, i + 6)
                };
                match close {
                    Some(k) => i = k + 1,
                    None => {
                        code[i] = b[i];
                        i += 1;
                    }
                }
            }
            _ => {
                code[i] = b[i];
                i += 1;
            }
        }
    }
    (
        String::from_utf8_lossy(&code).into_owned(),
        String::from_utf8_lossy(&comments).into_owned(),
    )
}

/// If a string literal opens at `b[i]` — `"…"`, `b"…"`, `r"…"`,
/// `r#"…"#`, `br"…"`, `br#"…"#` — returns `(index of the opening
/// quote, # count, is-raw)`. Identifier chars that do not lead to a
/// literal (`r#ident`, a variable named `b`) return `None`.
fn string_literal_start(b: &[u8], i: usize) -> Option<(usize, usize, bool)> {
    let mut j = i;
    let mut raw = false;
    if b.get(j) == Some(&b'b') {
        j += 1;
        if b.get(j) == Some(&b'r') {
            raw = true;
            j += 1;
        }
    } else if b.get(j) == Some(&b'r') {
        raw = true;
        j += 1;
    }
    let mut hashes = 0;
    while b.get(j + hashes) == Some(&b'#') {
        hashes += 1;
    }
    if b.get(j + hashes) == Some(&b'"') {
        Some((j + hashes, hashes, raw))
    } else {
        None
    }
}

/// Scan a string literal's body starting at `from` (one past the
/// opening quote). Cooked literals end at the first unescaped `"`; raw
/// literals at `"` followed by `hashes` × `#`. Newlines inside the
/// literal are mirrored into both shadows so line alignment survives a
/// multi-line string. Returns the index one past the literal.
fn string_literal_end(
    b: &[u8],
    mut i: usize,
    hashes: usize,
    raw: bool,
    code: &mut [u8],
    comments: &mut [u8],
) -> usize {
    while i < b.len() {
        if b[i] == b'\n' {
            code[i] = b'\n';
            comments[i] = b'\n';
            i += 1;
        } else if !raw && b[i] == b'\\' {
            // `\<newline>` is a legal line continuation inside a Rust
            // string — keep its newline mirrored.
            if b.get(i + 1) == Some(&b'\n') {
                code[i + 1] = b'\n';
                comments[i + 1] = b'\n';
            }
            i += 2;
        } else if b[i] == b'"' {
            i += 1;
            if raw {
                let closes = (0..hashes).all(|k| b.get(i + k) == Some(&b'#'));
                if closes {
                    return i + hashes;
                }
            } else {
                return i;
            }
        } else {
            i += 1;
        }
    }
    i
}

/// First `'` at `from .. min(to, len)` that is not past a newline —
/// the closer of a char literal; `None` means the opening `'` was a
/// lifetime and stays code.
fn closing_quote(b: &[u8], from: usize, to: usize) -> Option<usize> {
    let end = to.min(b.len());
    let mut k = from;
    while k < end && b[k] != b'\n' {
        if b[k] == b'\'' {
            return Some(k);
        }
        k += 1;
    }
    None
}

/// Parse `pub enum SimCommand` out of `csp-sim/src/lib.rs`: variant
/// name plus whether a `ui-unreachable:` marker binds to it.
///
/// Marker binding (the convention the annotations are written in):
/// a marker on its own line binds to the NEXT variant row; a marker in
/// a variant row's trailing comment binds to THAT row; a marker inside
/// a variant's payload (`d0 != 1`) annotates a field, not a command,
/// and is ignored — it must not leak onto the following variant.
///
/// Top-level-only parsing: the enum body is flat `Variant {…} | Variant`
/// rows at one indent level, so a depth counter over braces in the code
/// shadow separates the variant rows from their struct payloads.
fn sim_command_variants(lib: &str) -> Vec<(String, Option<String>)> {
    let Some(start) = lib.find("pub enum SimCommand") else {
        return Vec::new();
    };
    let Some(body_start) = lib[start..].find('{').map(|i| start + i + 1) else {
        return Vec::new();
    };
    let (code, comments) = split_code_and_comments(&lib[body_start..]);
    let mut depth: i32 = 1;
    let mut out = Vec::new();
    let mut pending_annotation: Option<String> = None;
    for (cline, mline) in code.lines().zip(comments.lines()) {
        let marker = mline
            .find("ui-unreachable:")
            .map(|p| mline[p + "ui-unreachable:".len()..].trim().to_string());
        let d0 = depth;
        depth += cline.matches('{').count() as i32 - cline.matches('}').count() as i32;
        if d0 != 1 {
            // Inside a variant's payload — a marker here is a field
            // note, not an opt-out; dropping it keeps it off the next
            // variant row.
            continue;
        }
        let trimmed = cline.trim();
        // A variant row: `Name {`, `Name(`, or `Name,`. Its own
        // trailing marker wins; otherwise it claims the pending one —
        // which is always consumed so a stale marker can never skip a
        // variant and exempt the one after.
        let name: String = trimmed
            .chars()
            .take_while(|c| c.is_ascii_uppercase() || c.is_alphanumeric() || *c == '_')
            .collect();
        if name.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
            let pending = pending_annotation.take();
            out.push((name, marker.or(pending)));
        } else if let Some(m) = marker {
            pending_annotation = Some(m);
        }
        if depth <= 0 {
            break; // enum body closed (checked after row detection so a
            // `LastVariant, }` line still registers its variant)
        }
    }
    out
}

/// All `*.rs` files under `dir`, reduced to their code shadows: test
/// code is cut at `#[cfg(test)]` (the workspace's tests live in
/// `mod tests` blocks at file end or `tests/` dirs, never scanned
/// anyway) and comments/string literals are blanked, so a
/// `SimCommand::X` that only appears in prose or a string cannot
/// satisfy the issuer gate.
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
            let (code, _) = split_code_and_comments(&text);
            // Cutting the shadow — not the raw text — keeps a
            // `"#[cfg(test)]"` string or a `// #[cfg(test)]` note from
            // truncating real code.
            let cut = code.find("#[cfg(test)]").unwrap_or(code.len());
            out.push((path.display().to_string(), code[..cut].to_string()));
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
    unreachable_variants(&variants, &sources)
}

/// The per-variant verdict loop, split from `check_reach` so tests can
/// drive it with synthetic variants and source shadows. `sources` must
/// already be code shadows (see `production_sources`).
fn unreachable_variants(
    variants: &[(String, Option<String>)],
    sources: &[(String, String)],
) -> Vec<String> {
    let mut failures = Vec::new();
    for (name, annotation) in variants {
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

    /// C35-F3 class (a): a `ui-unreachable:` in a variant row's
    /// TRAILING comment must annotate that row, not erase it and
    /// exempt the next one. The old parser consumed the marker line
    /// before row detection — `Foo` vanished from the checked set and
    /// `Bar` inherited the opt-out, so removing both issuers stayed
    /// green.
    #[test]
    fn trailing_marker_binds_to_its_own_row() {
        let lib = r#"
pub enum SimCommand {
    Foo, // ui-unreachable: reserved for scripts
    Bar,
    Baz,
}
"#;
        let v = sim_command_variants(lib);
        assert_eq!(
            v,
            vec![
                ("Foo".to_string(), Some("reserved for scripts".to_string())),
                ("Bar".to_string(), None),
                ("Baz".to_string(), None),
            ]
        );
    }

    /// C35-F3 class (a), second half: a marker inside a variant's
    /// payload is a field note, not an opt-out — it must not leak onto
    /// the next variant row (the old marker check ran before the
    /// `d0 != 1` payload guard).
    #[test]
    fn marker_inside_payload_does_not_leak() {
        let lib = r#"
pub enum SimCommand {
    Foo {
        // ui-unreachable: field remark, not an annotation
        x: u32,
    },
    Bar,
}
"#;
        let v = sim_command_variants(lib);
        assert_eq!(
            v,
            vec![("Foo".to_string(), None), ("Bar".to_string(), None)]
        );
    }

    /// Braces inside comments and literals must not move the enum
    /// depth counter — a `{` in a comment would otherwise swallow the
    /// following rows into a phantom payload.
    #[test]
    fn comment_and_literal_braces_do_not_move_depth() {
        let lib = r#"
pub enum SimCommand {
    Foo, // opens a phantom block {
    Bar, /* closes } and opens { again */
    Baz,
}
"#;
        let v = sim_command_variants(lib);
        assert_eq!(
            v,
            vec![
                ("Foo".to_string(), None),
                ("Bar".to_string(), None),
                ("Baz".to_string(), None),
            ]
        );
    }

    /// C35-F3 class (b): a `SimCommand::X` inside a doc comment or a
    /// string literal is a mention, not a construction — the old
    /// `text.contains` over raw source counted it as an issuer. This
    /// is the previously silently-green case that now reds.
    #[test]
    fn comment_and_string_mentions_are_not_issuers() {
        let src = r#"
//! Emits `SimCommand::DocOnly` when the flag flips.
const LABEL: &str = "SimCommand::DocOnly";
pub fn issue() -> SimCommand {
    SimCommand::Real { field: 1 }
}
"#;
        let (code, _) = split_code_and_comments(src);
        assert!(code.contains("SimCommand::Real"));
        assert!(!code.contains("SimCommand::DocOnly"));

        let variants = vec![("DocOnly".to_string(), None), ("Real".to_string(), None)];
        let sources = vec![("synthetic.rs".to_string(), code)];
        let failures = unreachable_variants(&variants, &sources);
        assert_eq!(failures.len(), 1, "failures: {failures:?}");
        assert!(failures[0].contains("DocOnly"));
    }

    /// The code/comment split itself: line comments, nested block
    /// comments, raw strings, char literals, and lifetimes land in the
    /// right shadow with line alignment preserved.
    #[test]
    fn split_code_and_comments_shadows() {
        let src = "let s = r#\"a // not a comment\"#; // real // comment\n\
                   /* block /* nested */ still comment */ let c = '}';\n\
                   fn f<'a>(x: &'a str) -> &'a str { x }\n";
        let (code, comments) = split_code_and_comments(src);
        // Comments land in the comment shadow only.
        assert!(comments.contains("real // comment"));
        assert!(comments.contains("still comment"));
        assert!(!code.contains("real // comment"));
        assert!(!code.contains("still comment"));
        // String and char-literal contents are blanked out of code —
        // the `'}'` literal must not look like a brace (the only `{`/`}`
        // left are the fn body's real pair).
        assert!(!code.contains("a // not a comment"));
        assert!(code.contains("let c ="));
        assert_eq!(code.matches('}').count(), 1);
        // Lifetimes and code survive in the code shadow.
        assert!(code.contains("fn f<'a>(x: &'a str) -> &'a str { x }"));
        // Line alignment is preserved for both shadows.
        assert_eq!(src.lines().count(), code.lines().count());
        assert_eq!(src.lines().count(), comments.lines().count());
    }

    /// The real repo pass: every variant is issued or opted out.
    /// (Also proves the parser sees the real enum, not a fixture.)
    #[test]
    fn every_sim_command_reachable_or_annotated() {
        let failures = check_reach(&workspace_root());
        assert!(failures.is_empty(), "unreachable variants: {failures:?}");
    }
}
