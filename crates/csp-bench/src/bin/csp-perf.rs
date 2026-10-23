//! `csp-perf` (M10-D / PLAN §5.3): run the §5.3 budget suite and write
//! the perf-log artifact.
//!
//! ```text
//! csp-perf [--out <path>] [--machine <name>]
//! ```
//!
//! Every measurable CPU row is benchmarked on the reference scene the
//! §5.3 table names; the fps rows are recorded `HardwareGated` (§8.2:
//! hosted runners have no GPU — they are REF-A/REF-B gates). The log
//! lands at `--out`, defaulting to
//! `docs/perf/perf-log-<machine>-<profile>.ron`. Exit codes: 0 = every
//! measurable row inside budget, 1 = a measured row failed, 2 = usage
//! or harness error.

use std::path::PathBuf;
use std::process::ExitCode;

use csp_bench::perf::{self, PerfStatus};

fn main() -> ExitCode {
    let mut out: Option<PathBuf> = None;
    let mut machine_name: Option<String> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--out" => match args.next() {
                Some(p) => out = Some(PathBuf::from(p)),
                None => return usage(),
            },
            "--machine" => match args.next() {
                Some(m) => machine_name = Some(m),
                None => return usage(),
            },
            _ => return usage(),
        }
    }
    let mut machine = perf::detect_machine();
    if let Some(name) = machine_name {
        machine.name = name;
    }
    let log = perf::run_perf_log(Some(machine));
    println!(
        "perf log — {} ({} {}, {}, rustc {})",
        log.machine.name, log.machine.os, log.machine.arch, log.machine.profile, log.machine.rustc
    );
    for e in &log.entries {
        let status = match e.status {
            PerfStatus::Pass => "PASS",
            PerfStatus::Fail => "FAIL",
            PerfStatus::HardwareGated => "GATED",
        };
        let measured = e
            .measured_value
            .map(|v| format!("{v:.3}"))
            .unwrap_or_else(|| "—".to_string());
        println!(
            "{status:6} {:32} budget ≤ {:>10.3} {:<12} measured {measured}",
            e.id, e.budget_value, e.unit
        );
        if let Some(d) = &e.detail {
            println!("       {}", shorten(d));
        }
    }
    let path = out.unwrap_or_else(|| perf::default_log_path(&log.machine));
    match perf::write_perf_log(&log, &path) {
        Ok(()) => println!("wrote {}", path.display()),
        Err(e) => {
            eprintln!("csp-perf: writing {} failed: {e}", path.display());
            return ExitCode::from(2);
        }
    }
    if log.all_passed() {
        ExitCode::SUCCESS
    } else {
        eprintln!("csp-perf: at least one measurable row is over budget");
        ExitCode::FAILURE
    }
}

/// Trim a detail line for the summary table.
fn shorten(s: &str) -> String {
    const MAX: usize = 110;
    if s.len() <= MAX {
        s.to_string()
    } else {
        format!("{}…", &s[..MAX])
    }
}

fn usage() -> ExitCode {
    eprintln!("usage: csp-perf [--out <path>] [--machine <name>]");
    ExitCode::from(2)
}
