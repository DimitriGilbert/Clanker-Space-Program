//! Repo lint tasks (PLAN §8.2): `cargo xtask lint-deps`, `lint-attribution`.
//! Replaces the old fragile `cargo tree | grep` one-liners: fails loudly on
//! tooling errors, uses explicit per-crate allow/deny lists — see the
//! `lint_deps`/`lint_attr` modules for the rule engines (pure and
//! unit-tested against violating fixtures).
//!
//! §8.4 golden workflow: `cargo xtask bless-goldens [--reason "<text>"]`.

mod lint_attr;
mod lint_deps;

use std::path::{Path, PathBuf};
use std::process::{Command, exit};

use lint_attr::lint_attribution;
use lint_deps::lint_deps;

/// A golden re-recorded in memory but not yet written. Pass 1 of §8.4's
/// bless stages everything up front so the refusals (dirty tree, schema
/// version, self-check) all run before a single byte is written — a
/// *refused* bless never half-writes the tree. (A write-side IO failure
/// mid-pass can still leave some dirs rewritten and others not; staging
/// narrows that window, it does not eliminate it.)
struct Staged {
    dir: PathBuf,
    name: String,
    golden: csp_bench::replay::RecordedGolden,
    /// `None` when the dir has fixture data but no committed artifacts —
    /// a new golden being blessed into existence.
    old_expected: Option<[u8; 32]>,
    old_schema_version: Option<u32>,
}

/// §8.4 bless-goldens: regenerate every registered scenario's `replay.bin`,
/// `expected-hash.txt`, and `fingerprint.json`.
///
/// Bootstrapping a NEW golden: commit the fixture dir
/// (`data/test-fixtures/<name>/` data files) and the scenario recipe
/// first; `bless-goldens` then writes the artifacts (the dir need not
/// contain `replay.bin` yet — it is created). Equivalently,
/// `csp-replay record <name> <dir>` writes them directly and the commit
/// carries them; bless afterwards only refreshes fingerprints. Either
/// way the committed diff is exactly the artifact files.
///
/// Every staged artifact self-verifies before anything is written:
/// `csp_bench::replay::record` replays its own output and refuses a
/// recording that cannot reproduce its recorded final hash.
///
/// Two hard refusals:
///
/// - **dirty tree** — `git status --porcelain` must be empty, so the
///   blessed commit contains exactly the regeneration;
/// - **hash changed without a `HASH_SCHEMA_VERSION` bump** — if the
///   re-recorded final hash differs from the committed one while
///   `csp_core::HASH_SCHEMA_VERSION` is unchanged, blessing would hide a
///   determinism regression. The only override is `--reason "<text>"`, the
///   documented behavioural change §8.4 names; it is echoed into the
///   summary for the commit message. A *new* golden has no prior hash —
///   there is nothing to refuse on.
fn bless_goldens(args: &[String]) {
    let reason = match args {
        [] => None,
        [flag, text] if flag == "--reason" && !text.is_empty() => Some(text.clone()),
        _ => {
            eprintln!("usage: cargo xtask bless-goldens [--reason \"<text>\"]");
            exit(2);
        }
    };
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    refuse_on_dirty_tree(&root);
    let staged = stage_goldens(&root);
    check_schema_version_refusal(&staged, reason.as_deref());
    write_goldens(&staged, reason.as_deref());
}

// See `tree` — tooling failure is loud failure here (§8.2).
#[allow(clippy::expect_used)]
fn refuse_on_dirty_tree(root: &Path) {
    let out = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(root)
        .output()
        .expect("git status failed to run");
    if !out.status.success() {
        eprintln!("bless-goldens: git status failed");
        exit(2);
    }
    if !out.stdout.is_empty() {
        eprintln!("bless-goldens: refusing on a dirty tree (§8.4) — commit or stash first");
        exit(1);
    }
}

// See `tree` — tooling failure is loud failure here (§8.2).
#[allow(clippy::expect_used)]
fn stage_goldens(root: &Path) -> Vec<Staged> {
    let fixtures = root.join("data/test-fixtures");
    // A committed golden whose scenario recipe no longer exists cannot be
    // re-recorded — refuse rather than silently leaving it stale.
    let mut orphans = Vec::new();
    for e in std::fs::read_dir(&fixtures)
        .expect("data/test-fixtures missing")
        .flatten()
    {
        let dir = e.path();
        if dir.is_dir() && dir.join(csp_bench::replay::REPLAY_FILE).exists() {
            let name = dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if csp_bench::scenarios::find(&name).is_none() {
                orphans.push(name);
            }
        }
    }
    if !orphans.is_empty() {
        eprintln!(
            "bless-goldens: committed goldens without a scenario recipe: {}",
            orphans.join(", ")
        );
        exit(1);
    }
    // Iterate the recipe registry, not the directory listing: a scenario
    // whose fixture dir exists but has no artifacts yet is blessed into
    // existence here — see `bless_goldens`' bootstrap note.
    let mut staged = Vec::new();
    for recipe in csp_bench::scenarios::SCENARIOS {
        let dir = fixtures.join(recipe.name);
        let name = recipe.name.to_string();
        // record() self-verifies: the artifact must reproduce its own
        // final hash before anything below may write it.
        let golden = csp_bench::replay::record(recipe, &dir).unwrap_or_else(|e| {
            eprintln!("bless-goldens: recording '{name}' failed: {e}");
            exit(2);
        });
        let (old_expected, old_schema_version) =
            if dir.join(csp_bench::replay::REPLAY_FILE).exists() {
                let (old_artifact, old_expected) = csp_bench::replay::load_golden(&dir)
                    .unwrap_or_else(|e| {
                        eprintln!("bless-goldens: loading '{}' failed: {e}", dir.display());
                        exit(2);
                    });
                (
                    Some(old_expected),
                    Some(old_artifact.fingerprint.hash_schema_version),
                )
            } else {
                (None, None)
            };
        staged.push(Staged {
            dir,
            name,
            golden,
            old_expected,
            old_schema_version,
        });
    }
    if staged.is_empty() {
        eprintln!("bless-goldens: no scenario recipes are registered");
        exit(1);
    }
    staged
}

/// §8.4 refusal: any changed hash without a `HASH_SCHEMA_VERSION` bump and
/// without `--reason` aborts before anything is written. A new golden
/// (no committed artifacts yet) has no prior hash — nothing to refuse on.
fn check_schema_version_refusal(staged: &[Staged], reason: Option<&str>) {
    for s in staged {
        let Some(old_expected) = s.old_expected else {
            continue;
        };
        let changed = s.golden.final_hash != old_expected;
        let version_bumped = s
            .old_schema_version
            .is_some_and(|old| s.golden.artifact.fingerprint.hash_schema_version > old);
        if changed && !version_bumped && reason.is_none() {
            eprintln!(
                "bless-goldens: REFUSED — '{}' hash changed ({} -> {}) without a \
                 HASH_SCHEMA_VERSION bump (§8.4). Bump it in the same commit, or \
                 pass --reason \"<text>\" for a documented behavioural change.",
                s.name,
                csp_bench::replay::hex(&old_expected),
                csp_bench::replay::hex(&s.golden.final_hash)
            );
            exit(1);
        }
    }
}

// See `tree` — tooling failure is loud failure here (§8.2).
#[allow(clippy::expect_used)]
fn write_goldens(staged: &[Staged], reason: Option<&str>) {
    for s in staged {
        csp_bench::replay::write_golden(&s.dir, &s.golden).unwrap_or_else(|e| {
            eprintln!("bless-goldens: writing '{}' failed: {e}", s.dir.display());
            exit(2);
        });
        match s.old_expected {
            None => println!(
                "bless-goldens: {} — new golden blessed in ({})",
                s.name,
                csp_bench::replay::hex(&s.golden.final_hash)
            ),
            Some(old) if s.golden.final_hash != old => println!(
                "bless-goldens: {} — reblessed {} -> {} (schema {} -> {}){}",
                s.name,
                csp_bench::replay::hex(&old),
                csp_bench::replay::hex(&s.golden.final_hash),
                s.old_schema_version.unwrap_or(0),
                s.golden.artifact.fingerprint.hash_schema_version,
                reason.map(|r| format!(", reason: {r}")).unwrap_or_default()
            ),
            Some(_) => println!(
                "bless-goldens: {} — hash unchanged, fingerprint refreshed",
                s.name
            ),
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("lint-deps") => lint_deps(),
        Some("lint-attribution") => lint_attribution(),
        Some("bless-goldens") => bless_goldens(args.get(1..).unwrap_or(&[])),
        _ => {
            eprintln!("usage: cargo xtask <lint-deps|lint-attribution|bless-goldens>");
            exit(2);
        }
    }
}
