//! `csp-replay` (PLAN §2.1): the golden-replay CLI over `csp_bench::replay`.
//!
//! ```text
//! csp-replay list                        known scenario recipes
//! csp-replay record <scenario> <dir>     drive the recipe; write the golden
//!           [--force]                    artifacts (replay.bin,
//!                                        expected-hash.txt, fingerprint.json)
//!                                        `--force` authorizes overwriting an
//!                                        occupied dir whose committed hash
//!                                        differs (§8.4 — without it a
//!                                        mismatched write is refused)
//! csp-replay verify <dir>                replay the artifact; compare the
//!                                        reconstructed hash against
//!                                        expected-hash.txt
//! ```
//!
//! Exit codes: 0 = verified/written, 1 = golden hash mismatch, 2 = usage or
//! harness error. Native-only — it walks the filesystem freely, but the
//! reconstruction itself stays inside `csp-sim`'s public surface (§2.1).

use std::path::Path;
use std::process::ExitCode;

use csp_bench::replay;
use csp_bench::scenarios;

const USAGE: &str = "usage: csp-replay <list | record <scenario> <dir> [--force] | verify <dir>>";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(code) => ExitCode::from(code),
        // §8.3 verdict mapping: a hash mismatch is the golden's red (1);
        // every other ReplayError is a harness failure (2).
        Err(e) => {
            eprintln!("csp-replay: {e}");
            if matches!(e, replay::ReplayError::HashMismatch { .. }) {
                ExitCode::FAILURE
            } else {
                ExitCode::from(2)
            }
        }
    }
}

fn run(args: &[String]) -> Result<u8, replay::ReplayError> {
    let rest = args.get(1..).unwrap_or(&[]);
    match args.first().map(String::as_str) {
        Some("list") if rest.is_empty() => {
            for recipe in scenarios::SCENARIOS {
                println!("{}", recipe.name);
            }
            Ok(0)
        }
        Some("record") => {
            let (scenario, dir, force) = match rest {
                [scenario, dir] => (scenario.as_str(), dir.as_str(), false),
                [scenario, dir, flag] if flag == "--force" => {
                    (scenario.as_str(), dir.as_str(), true)
                }
                _ => return usage(),
            };
            let Some(recipe) = scenarios::find(scenario) else {
                eprintln!("csp-replay: unknown scenario '{scenario}' (see `csp-replay list`)");
                return Ok(2);
            };
            let dir = Path::new(dir);
            let golden = replay::record(recipe, dir)?;
            // §8.4's anti-laundering refusal lives in the library:
            // `write_golden` refuses a hash-changing overwrite of an
            // occupied dir; `--force` is the explicit authorization.
            if force {
                replay::write_golden_overwrite(dir, &golden)?;
            } else {
                replay::write_golden(dir, &golden)?;
            }
            println!(
                "recorded {}: {} commands, final hash {}",
                recipe.name,
                golden.artifact.command_log.len(),
                replay::hex(&golden.final_hash)
            );
            Ok(0)
        }
        Some("verify") => {
            let [dir] = rest else {
                return usage();
            };
            verify_cmd(Path::new(dir))
        }
        _ => usage(),
    }
}

/// `verify <dir>`: the strict [`replay::verify_golden`] path — doctored
/// artifacts, fixture drift, and the replay verdict are all hard errors;
/// toolchain/dep fingerprint drift stays advisory notes (§7.1).
fn verify_cmd(dir: &Path) -> Result<u8, replay::ReplayError> {
    match replay::verify_golden(dir) {
        Ok(v) => {
            println!("{}: OK ({})", dir.display(), replay::hex(&v.hash));
            for w in &v.warnings {
                println!("note: fingerprint drift — {w}");
            }
            Ok(0)
        }
        Err(e @ replay::ReplayError::HashMismatch { .. }) => {
            eprintln!("csp-replay: {e}");
            // §7.1 context for a red verdict — best-effort: the verdict
            // is already decided, so a reload failure adds nothing.
            if let Ok((artifact, _)) = replay::load_golden(dir)
                && let Ok(fixture_hash) = replay::fixture_hash(dir)
            {
                for w in replay::fingerprint_warnings(&artifact.fingerprint, fixture_hash) {
                    eprintln!("note: fingerprint drift — {w}");
                }
            }
            Ok(1)
        }
        Err(e) => Err(e),
    }
}

fn usage() -> Result<u8, replay::ReplayError> {
    eprintln!("{USAGE}");
    Ok(2)
}
