//! `csp-perf` (M10-D / PLAN §5.3): run the §5.3 budget suite and write
//! the perf-log artifact.
//!
//! ```text
//! csp-perf [--out <path>] [--machine <name>] [--baseline <path>]
//! ```
//!
//! Every measurable CPU row is benchmarked on the reference scene the
//! §5.3 table names; the fps rows are recorded `HardwareGated` (§8.2:
//! hosted runners have no GPU — they are REF-A/REF-B gates). The log
//! lands at `--out`, defaulting to
//! `docs/perf/perf-log-<machine>-<profile>.ron`.
//!
//! `--baseline <path>` diffs the fresh log against a committed perf
//! log (the release CI job's gate): a row more than
//! [`perf::REGRESSION_TOLERANCE`] slower than the committed
//! measurement, a §5.3 Pass→Fail crossing, or an unmeasured
//! measurable row is a **regression** and fails the run. `--out`
//! resolving to the same file as `--baseline` is refused outright —
//! the write would clobber the baseline and the "diff" would compare
//! the fresh log against itself.
//!
//! Exit codes: 0 = every measurable row inside budget and no
//! regression, 1 = a measured row failed or regressed, 2 = usage or
//! harness error (including the `--out` == `--baseline` refusal).

use std::path::PathBuf;
use std::process::ExitCode;

use csp_bench::perf::{self, PerfStatus};

fn main() -> ExitCode {
    let mut out: Option<PathBuf> = None;
    let mut machine_name: Option<String> = None;
    let mut baseline_path: Option<PathBuf> = None;
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
            "--baseline" => match args.next() {
                Some(p) => baseline_path = Some(PathBuf::from(p)),
                None => return usage(),
            },
            _ => return usage(),
        }
    }
    run(out, machine_name, baseline_path)
}

fn run(
    out: Option<PathBuf>,
    machine_name: Option<String>,
    baseline_path: Option<PathBuf>,
) -> ExitCode {
    let mut machine = perf::detect_machine();
    if let Some(name) = machine_name {
        machine.name = name;
    }
    let path = out.unwrap_or_else(|| perf::default_log_path(&machine));
    // `--out` defaults to the committed per-machine log — exactly the
    // file `--baseline` names when diffing against it. Writing first
    // would clobber the baseline and then "diff" the fresh log
    // against itself (a vacuous green that also dirties the tree), so
    // the same-file case is refused outright.
    if let Some(base_path) = &baseline_path
        && same_file(&path, base_path)
    {
        eprintln!(
            "csp-perf: refusing to overwrite the baseline — pass a different --out \
             ({} and {} resolve to the same file)",
            path.display(),
            base_path.display()
        );
        return ExitCode::from(2);
    }
    // Load the baseline before the suite runs: an unreadable baseline
    // fails fast instead of burning a full measurement pass, and the
    // parsed log the diff below uses can never be the bytes `--out`
    // is about to write — even for a same-file pair the guard could
    // not prove equal (hardlinks, dangling symlink targets).
    let baseline = match &baseline_path {
        Some(base_path) => match perf::load_perf_log(base_path) {
            Ok(b) => Some(b),
            Err(e) => {
                eprintln!("csp-perf: baseline {} unreadable: {e}", base_path.display());
                return ExitCode::from(2);
            }
        },
        None => None,
    };
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
    match perf::write_perf_log(&log, &path) {
        Ok(()) => println!("wrote {}", path.display()),
        Err(e) => {
            eprintln!("csp-perf: writing {} failed: {e}", path.display());
            return ExitCode::from(2);
        }
    }
    // The release-CI gate: diff the fresh log against the last
    // committed log — already parsed above, so this compares against
    // the committed bytes even though the write already landed. With
    // `--baseline` the diff is the verdict: rows already red in the
    // committed log stay reported but the gate checks *movement* (a
    // permanently-red gate is a gate nobody keeps). Without it the
    // verdict is the §5.3 budget itself (`all_passed`) — the
    // REF-machine check.
    let diff_mode = baseline.is_some();
    let regressions = match (&baseline, &baseline_path) {
        (Some(b), Some(base_path)) => baseline_diff(&log, b, base_path),
        _ => Vec::new(),
    };
    if !log.all_passed() {
        eprintln!("csp-perf: at least one measurable row is over budget");
    }
    if !regressions.is_empty() {
        eprintln!("csp-perf: {} regression(s) vs baseline", regressions.len());
    }
    let verdict_ok = if diff_mode {
        regressions.is_empty()
    } else {
        log.all_passed()
    };
    if verdict_ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// The `--baseline` half: report the regression diff against the
/// already-loaded committed log (`base_path` is for messages only —
/// the bytes were read before `--out` was written).
fn baseline_diff(
    log: &perf::PerfLog,
    baseline: &perf::PerfLog,
    base_path: &std::path::Path,
) -> Vec<perf::PerfRegression> {
    if baseline.machine.name != log.machine.name {
        println!(
            "note: baseline was measured on '{}' — the diff is cross-machine",
            baseline.machine.name
        );
    }
    let regressions = perf::regression_diff(log, baseline);
    if regressions.is_empty() {
        println!(
            "regression diff vs {}: no row regressed >{:.0}%",
            base_path.display(),
            perf::REGRESSION_TOLERANCE * 100.0
        );
    } else {
        eprintln!("regressions vs {}:", base_path.display());
        for r in &regressions {
            let b = r
                .baseline
                .map(|v| format!("{v:.3}"))
                .unwrap_or_else(|| "—".to_string());
            let m = r
                .measured
                .map(|v| format!("{v:.3}"))
                .unwrap_or_else(|| "—".to_string());
            eprintln!("  REGRESSED {:32} {b} -> {m}: {}", r.id, r.reason);
        }
    }
    regressions
}

/// Canonical-path equality for the `--out`/`--baseline` guard.
/// `canonicalize` wins when the file exists (resolves symlinks and
/// `..`); a not-yet-written `--out` falls back to a normalized
/// absolute path so `./log.ron` / `a/../log.ron` spellings still
/// match the baseline they would clobber.
fn same_file(a: &std::path::Path, b: &std::path::Path) -> bool {
    fn resolve(p: &std::path::Path) -> PathBuf {
        if let Ok(c) = p.canonicalize() {
            return c;
        }
        let abs = std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
        let mut n = PathBuf::new();
        for c in abs.components() {
            match c {
                std::path::Component::CurDir => {}
                std::path::Component::ParentDir => {
                    n.pop();
                }
                other => n.push(other.as_os_str()),
            }
        }
        n
    }
    resolve(a) == resolve(b)
}

/// Trim a detail line for the summary table. The byte cut is floored
/// to a char boundary — detail strings carry non-ASCII (`≤`, `—`, `§`,
/// `×`, `⁵`), so a raw `&s[..MAX]` can panic mid-char.
fn shorten(s: &str) -> String {
    const MAX: usize = 110;
    if s.len() <= MAX {
        s.to_string()
    } else {
        let end = s
            .char_indices()
            .map(|(i, _)| i)
            .take_while(|&i| i <= MAX)
            .last()
            .unwrap_or(0);
        format!("{}…", &s[..end])
    }
}

fn usage() -> ExitCode {
    eprintln!("usage: csp-perf [--out <path>] [--machine <name>] [--baseline <path>]");
    ExitCode::from(2)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::path::Path;

    /// The guard's whole job: spelling variants of one file must
    /// compare equal, different files must not.
    #[test]
    fn same_file_catches_spelling_variants() {
        let dir = std::env::temp_dir().join(format!("csp-perf-same-file-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let real = dir.join("log.ron");
        std::fs::write(&real, b"x").unwrap();
        // Existing file → canonicalize resolves `.`/`..` and symlinks.
        assert!(same_file(&real, &dir.join("./log.ron")));
        assert!(same_file(&real, &dir.join("sub/../log.ron")));
        assert!(!same_file(&real, &dir.join("other.ron")));
    }

    /// `--out` usually does not exist yet — the absolute+normalize
    /// fallback must still prove `x` == `./x` == `a/../x`.
    #[test]
    fn same_file_falls_back_for_unwritten_out() {
        let missing = Path::new("csp-perf-never-written-a/../csp-perf-never-written.ron");
        let plain = Path::new("csp-perf-never-written.ron");
        let dotted = Path::new("./csp-perf-never-written.ron");
        assert!(same_file(missing, plain));
        assert!(same_file(missing, dotted));
        assert!(!same_file(
            plain,
            Path::new("csp-perf-never-written-other.ron")
        ));
    }
}
