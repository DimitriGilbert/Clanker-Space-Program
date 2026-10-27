//! The §2.14 log tail — a `log::Log` that forwards every record to
//! `env_logger` *and* keeps the last [`TAIL_LINES`] formatted lines in a
//! ring buffer, so the bug-report bundle can ship "the last 500 log
//! lines" without the sim ever knowing a logger exists.
//!
//! Native only: the wasm build keeps `console_log` (its log sink is the
//! dev console, and the bundle is a filesystem artifact).

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

/// §2.14's "last 500 log lines".
pub const TAIL_LINES: usize = 500;

/// The ring's own floor: the bundle wants `info`-and-above regardless
/// of console verbosity — a `RUST_LOG=warn` session must not ship an
/// empty `log.txt`. [`init`] raises the process `max_level` to admit
/// both channels; [`TeeLog`] re-gates them separately (console on the
/// env_logger filter, the ring on this level).
const TAIL_LEVEL: log::LevelFilter = log::LevelFilter::Info;

/// The process-wide tail — `OnceLock` so [`init`] is idempotent-safe:
/// a second `init` (a test binary is one process) reuses the buffer.
static TAIL: OnceLock<Mutex<VecDeque<String>>> = OnceLock::new();

/// The home-dir prefix scrubbed out of ring-captured lines — `log.txt`
/// ships in bug-report bundles to strangers, so `/home/<user>` /
/// `%USERPROFILE%` never leaves the machine inside one (C25-F3). The
/// console copy keeps the real path; only the tail is redacted.
/// `OnceLock`: the prefix can't change mid-process, and `log` must
/// stay cheap on every record.
static HOME_PREFIX: OnceLock<Option<String>> = OnceLock::new();

/// The resolved home-dir prefix to scrub, or `None` when there is
/// nothing safe to scrub (no `HOME`/`USERPROFILE`, or a trivially
/// short prefix like `/` that would redact every separator).
fn home_prefix() -> Option<&'static str> {
    HOME_PREFIX
        .get_or_init(|| {
            #[cfg(target_os = "windows")]
            let home = std::env::var_os("USERPROFILE");
            #[cfg(not(target_os = "windows"))]
            let home = std::env::var_os("HOME");
            home.and_then(|h| {
                let mut s = h.to_string_lossy().into_owned();
                // A trailing separator would turn `~/x` redaction into
                // `~x` — strip it so the prefix always ends at the dir.
                while s.len() > 1 && s.ends_with(['/', '\\']) {
                    s.pop();
                }
                if s.len() > 1 { Some(s) } else { None }
            })
        })
        .as_deref()
}

/// Install the tee logger: `env_logger`'s configuration (RUST_LOG,
/// formatting) forwards records onward, and each record's formatted
/// line also lands in the ring buffer. Fails soft — logging is a
/// diagnostic channel, never a startup refusal.
pub fn init() {
    let _ = TAIL.get_or_init(|| Mutex::new(VecDeque::with_capacity(TAIL_LINES)));
    let inner =
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).build();
    // The `log` macros short-circuit at `max_level` before a record
    // ever reaches the logger, so the process max must admit BOTH
    // channels — the console's env filter and the ring's floor — or a
    // quiet console (`RUST_LOG=warn`) would starve the tail.
    let max = inner.filter().max(TAIL_LEVEL);
    if log::set_boxed_logger(Box::new(TeeLog { inner })).is_ok() {
        log::set_max_level(max);
    }
}

/// A copy of the last [`TAIL_LINES`] formatted lines, oldest first.
pub fn tail() -> Vec<String> {
    TAIL.get()
        .and_then(|m| m.lock().ok().map(|t| t.iter().cloned().collect()))
        .unwrap_or_default()
}

/// Push one formatted line into the ring, evicting the oldest at the
/// cap. The home-dir prefix is scrubbed here — one choke point, so
/// every path-embedding log site (settings warns, bundle errors)
/// stays simple while `log.txt` never carries `/home/<user>` into a
/// shipped bundle (C25-F3).
fn push(line: String) {
    let line = match home_prefix() {
        Some(home) => line.replace(home, "~"),
        None => line,
    };
    if let Some(m) = TAIL.get()
        && let Ok(mut tail) = m.lock()
    {
        if tail.len() >= TAIL_LINES {
            tail.pop_front();
        }
        tail.push_back(line);
    }
}

/// The forwarding logger.
struct TeeLog {
    inner: env_logger::Logger,
}

impl log::Log for TeeLog {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        // Either channel may want the record — `enabled` ORs them so
        // `log_enabled!`-style callers can't pre-empt the tail floor.
        self.inner.enabled(metadata) || metadata.level() <= TAIL_LEVEL
    }

    fn log(&self, record: &log::Record<'_>) {
        // The console half keeps env_logger's filter (`Logger::log`
        // re-gates internally); the ring half keeps its own floor.
        self.inner.log(record);
        if record.level() <= TAIL_LEVEL {
            push(format!(
                "{:<5} {}: {}",
                record.level(),
                record.target(),
                record.args()
            ));
        }
    }

    fn flush(&self) {
        self.inner.flush();
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // test assertions
mod tests {
    use super::*;

    /// C25-F3: a log line embedding `$HOME` lands in the ring with
    /// the prefix scrubbed to `~` — `log.txt` never ships the user's
    /// absolute home path inside a bug-report bundle.
    #[test]
    fn tail_lines_redact_the_home_dir() {
        init();
        let Some(home) = home_prefix().map(|h| h.to_string()) else {
            return; // no HOME on this host — nothing to leak
        };
        push(format!(
            "WARN settings: cannot read {home}/.config/csp/settings.ron"
        ));
        let last = tail().last().cloned().unwrap_or_default();
        assert!(
            last.contains("~/.config/csp/settings.ron"),
            "the home prefix must render as ~: {last}"
        );
        assert!(!last.contains(&home), "the absolute path leaked: {last}");
    }
}
