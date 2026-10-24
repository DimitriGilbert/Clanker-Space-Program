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
/// Hard refusals:
///
/// - **dirty tree** — `git status --porcelain` must be empty, so the
///   blessed commit contains exactly the regeneration;
/// - **partial artifact set** — a fixture dir holding SOME of the
///   `ARTIFACT_FILES` (e.g. `expected-hash.txt` without `replay.bin`)
///   is ambiguous: it cannot load as an existing golden, but treating
///   it as brand-new would skip the hash-change refusal. The dir must
///   be made all-or-nothing first;
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
    // "Is this dir a golden?" keys on ANY artifact file, not just
    // replay.bin — a partial set (expected-hash.txt without replay.bin,
    // say) is still a committed golden, not a blank fixture dir.
    let has_artifacts = |dir: &Path| {
        csp_bench::replay::ARTIFACT_FILES
            .iter()
            .filter(|f| dir.join(f).exists())
            .count()
    };
    // A committed golden whose scenario recipe no longer exists cannot be
    // re-recorded — refuse rather than silently leaving it stale.
    let mut orphans = Vec::new();
    for e in std::fs::read_dir(&fixtures)
        .expect("data/test-fixtures missing")
        .flatten()
    {
        let dir = e.path();
        if dir.is_dir() && has_artifacts(&dir) > 0 {
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
        let (old_expected, old_schema_version) = match has_artifacts(&dir) {
            n if n == csp_bench::replay::ARTIFACT_FILES.len() => {
                let (old_artifact, old_expected) = csp_bench::replay::load_golden(&dir)
                    .unwrap_or_else(|e| {
                        eprintln!("bless-goldens: loading '{}' failed: {e}", dir.display());
                        exit(2);
                    });
                (
                    Some(old_expected),
                    Some(old_artifact.fingerprint.hash_schema_version),
                )
            }
            0 => (None, None),
            n => {
                // A partial artifact set is not a blank slate: keying
                // "new" on replay.bin alone would let a dir holding
                // only expected-hash.txt skip the hash-change refusal
                // and have its artifacts overwritten unchecked.
                let present: Vec<&str> = csp_bench::replay::ARTIFACT_FILES
                    .iter()
                    .copied()
                    .filter(|f| dir.join(f).exists())
                    .collect();
                eprintln!(
                    "bless-goldens: refusing — '{}' holds a partial golden \
                     ({n}/{} artifacts: {}). Restore or delete the artifact files \
                     so the dir is all-or-nothing.",
                    dir.display(),
                    csp_bench::replay::ARTIFACT_FILES.len(),
                    present.join(", ")
                );
                exit(2);
            }
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

/// M10-C / §6.3: package the web demo into `apps/demo/dist/`.
///
/// 1. `cargo build -p csp-app --lib --release --target wasm32-unknown-unknown`
///    — the §8.2 wasm compile gate, run for real.
/// 2. `wasm-bindgen --target web` over the cdylib into a **staging**
///    dir that only renames to `apps/demo/dist` on success — a failed
///    or missing bindgen must not leave a half-populated `dist/`
///    behind (the old flow wrote `index.html` into `dist/` before
///    bindgen even ran). `wasm-bindgen-cli` must match the crate
///    version (workspace pin 0.2.129; the CLI itself refuses a
///    mismatched .wasm). Absent → the exact commands are printed and
///    the task exits non-zero: §8.2's "tooling failure is loud
///    failure", never a silent half-artifact.
/// 3. `apps/demo/index.html` is staged beside the bundle.
fn build_web() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let status = Command::new("cargo")
        .args([
            "build",
            "-p",
            "csp-app",
            "--lib",
            "--release",
            "--target",
            "wasm32-unknown-unknown",
            "--locked",
        ])
        .current_dir(&root)
        .status()
        .unwrap_or_else(|e| {
            eprintln!("build-web: failed to run cargo: {e}");
            exit(2);
        });
    if !status.success() {
        eprintln!("build-web: wasm build failed");
        exit(status.code().unwrap_or(2));
    }
    let wasm = root.join("target/wasm32-unknown-unknown/release/csp_app.wasm");
    let dist = root.join("apps/demo/dist");
    let stage = root.join("apps/demo/.dist-staging");
    if stage.exists()
        && let Err(e) = std::fs::remove_dir_all(&stage)
    {
        eprintln!("build-web: cannot clear {}: {e}", stage.display());
        exit(2);
    }
    if let Err(e) = std::fs::create_dir_all(&stage) {
        eprintln!("build-web: cannot create {}: {e}", stage.display());
        exit(2);
    }
    if let Err(e) = std::fs::copy(root.join("apps/demo/index.html"), stage.join("index.html")) {
        build_web_fail(&stage, 2, &format!("cannot copy index.html: {e}"));
    }
    let bindgen = Command::new("wasm-bindgen")
        .args(["--target", "web", "--out-dir"])
        .arg(&stage)
        .arg("--out-name")
        .arg("csp_app")
        .arg(&wasm)
        .current_dir(&root)
        .status();
    match bindgen {
        Ok(s) if s.success() => {
            // All artifacts exist — swap dist/ in one rename so the
            // output is never observed half-written.
            if let Ok(md) = std::fs::symlink_metadata(&dist) {
                let removed = if md.is_dir() {
                    std::fs::remove_dir_all(&dist)
                } else {
                    std::fs::remove_file(&dist)
                };
                if let Err(e) = removed {
                    build_web_fail(
                        &stage,
                        2,
                        &format!("cannot remove old {}: {e}", dist.display()),
                    );
                }
            }
            if let Err(e) = std::fs::rename(&stage, &dist) {
                build_web_fail(
                    &stage,
                    2,
                    &format!("cannot move {} into place: {e}", stage.display()),
                );
            }
            println!(
                "build-web: artifact at {} — serve it with any static file server, e.g.\n  \
                 python3 -m http.server -d apps/demo/dist",
                dist.display()
            );
        }
        Ok(s) => {
            let code = s.code().unwrap_or(2);
            build_web_fail(&stage, code, &format!("wasm-bindgen failed ({s})"));
        }
        Err(e) => {
            eprintln!(
                "build-web: wasm-bindgen not found ({e}). The wasm cdylib is built at\n  \
                 {}\nfinish the packaging with:\n  \
                 cargo install wasm-bindgen-cli --version 0.2.129 --locked\n  \
                 wasm-bindgen --target web --out-dir apps/demo/dist --out-name csp_app \\\n    \
                 target/wasm32-unknown-unknown/release/csp_app.wasm\n  \
                 cp apps/demo/index.html apps/demo/dist/",
                wasm.display()
            );
            build_web_fail(&stage, 2, "wasm-bindgen unavailable");
        }
    }
}

/// Clear the staging dir and exit — a bindgen failure must leave
/// neither a half-populated `dist/` nor a stale `.dist-staging/`.
fn build_web_fail(stage: &Path, code: i32, msg: &str) -> ! {
    let _ = std::fs::remove_dir_all(stage);
    eprintln!("build-web: {msg}");
    exit(code);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("lint-deps") => lint_deps(),
        Some("lint-attribution") => lint_attribution(),
        Some("bless-goldens") => bless_goldens(args.get(1..).unwrap_or(&[])),
        Some("build-web") => build_web(),
        _ => {
            eprintln!("usage: cargo xtask <lint-deps|lint-attribution|bless-goldens|build-web>");
            exit(2);
        }
    }
}
