//! `csp-eval` (PLAN §2.12.1, M9d) — the scenario runner: a
//! `scenario.ron` in, an [`EvalReport`](csp_bench::scenario::EvalReport)
//! out — the plan's "`→ exit code + final state hash + score`".
//!
//! ```text
//! csp-eval <scenario.ron> [--fixtures-root <dir>] [--transcript <out.json>]
//! csp-eval --suite <dir>    [--fixtures-root <dir>]
//! ```
//!
//! - `<scenario.ron>`: one scenario file (`csp_bench::scenario`).
//! - `--suite <dir>`: run every `*.ron` in `<dir>` instead (sorted
//!   order); exit code is the worst verdict across the suite.
//! - `--fixtures-root`: where `WorldSpec::Fixture{name}` resolves
//!   (default `./data/test-fixtures` — the repo convention).
//! - `--transcript`: write the lockstep transcript (the "agent runs
//!   are reproducible only as logs" artifact, §2.12.1) as JSON —
//!   single-scenario mode only.
//!
//! Exit codes: 0 = every goal held, 1 = eval verdict fail (unmet
//! goal or agent fault), 2 = usage/harness error. The report JSON
//! always lands on stdout.

use std::path::PathBuf;
use std::process::ExitCode;

use csp_bench::scenario::{self, EvalError, EvalReport};

const USAGE: &str = "usage: csp-eval <scenario.ron | --suite <dir>> [--fixtures-root <dir>] [--transcript <out.json>]";

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("csp-eval: {e}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<ExitCode, EvalError> {
    let mut scenario_path: Option<PathBuf> = None;
    let mut suite_dir: Option<PathBuf> = None;
    let mut fixtures_root = PathBuf::from("data/test-fixtures");
    let mut transcript_out: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--suite" => {
                suite_dir =
                    Some(PathBuf::from(args.next().ok_or_else(|| {
                        EvalError::Invalid("--suite needs a dir".into())
                    })?));
            }
            "--fixtures-root" => {
                fixtures_root = PathBuf::from(
                    args.next()
                        .ok_or_else(|| EvalError::Invalid("--fixtures-root needs a dir".into()))?,
                );
            }
            "--transcript" => {
                transcript_out =
                    Some(PathBuf::from(args.next().ok_or_else(|| {
                        EvalError::Invalid("--transcript needs a path".into())
                    })?));
            }
            other if scenario_path.is_none() && !other.starts_with("--") => {
                scenario_path = Some(PathBuf::from(other));
            }
            other => return Err(EvalError::Invalid(format!("{USAGE}\nunknown arg {other}"))),
        }
    }

    if let Some(dir) = suite_dir {
        let entries = csp_bench::eval::run_suite(&dir, &fixtures_root)?;
        let mut all_pass = true;
        for entry in &entries {
            match &entry.outcome {
                Ok(report) => {
                    print_report(report);
                    all_pass &= report.pass;
                }
                Err(e) => {
                    eprintln!("{}: {e}", entry.path.display());
                    all_pass = false;
                }
            }
        }
        return Ok(if all_pass {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        });
    }

    let Some(path) = scenario_path else {
        eprintln!("{USAGE}");
        return Ok(ExitCode::from(2));
    };
    let file = scenario::load_scenario(&path)?;
    let (report, transcript) = scenario::run_scenario_full(&file, &fixtures_root)?;
    print_report(&report);
    if let Some(out) = transcript_out {
        let json = serde_json::to_string_pretty(&transcript)
            .map_err(|e| EvalError::Invalid(format!("transcript encode: {e}")))?;
        std::fs::write(&out, json).map_err(EvalError::Io)?;
    }
    Ok(if report.pass {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn print_report(report: &EvalReport) {
    match serde_json::to_string_pretty(report) {
        Ok(json) => println!("{json}"),
        Err(e) => eprintln!("csp-eval: report serialize failed: {e}"),
    }
}
