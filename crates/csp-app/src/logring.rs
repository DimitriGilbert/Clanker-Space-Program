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
    let max = inner.filter();
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
        self.inner.enabled(metadata)
    }

    fn log(&self, record: &log::Record<'_>) {
        self.inner.log(record);
        if self.inner.enabled(record.metadata()) {
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
