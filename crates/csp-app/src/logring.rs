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

/// Push one formatted line into the ring, evicting the oldest at the cap.
fn push(line: String) {
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
