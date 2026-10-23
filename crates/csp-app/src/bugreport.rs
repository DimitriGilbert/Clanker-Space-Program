//! The §2.14 bug-report bundle (M10) — what a user ships to the
//! developer after a `Fatal` or a panic, and what `--bug-report`
//! writes on demand.
//!
//! One bundle = one directory under `$config_dir/csp/bug-reports/`:
//!
//! | file | contents |
//! |---|---|
//! | `save.cspsave` | the §7.7 versioned save (the world at the halt) |
//! | `commands.bin` | the §7.6 command log (bincode) |
//! | `fingerprint.json` | the §7.1 determinism fingerprint |
//! | `error.txt` | the halt reason + sim tick |
//! | `system.txt` | OS/arch/adapter report |
//! | `log.txt` | the last [`crate::logring::TAIL_LINES`] log lines |
//! | `README.txt` | "send this folder" instructions |
//!
//! The writer is deliberately dumb: its input is already-collected
//! data ([`BugReportInput`]) so the panic path — which cannot reach
//! the sim — writes the same shape with `None` fields for what it
//! cannot see. Native only: the bundle is a filesystem artifact.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use crate::settings_io::config_dir;

/// The bundle parent: `$config_dir/csp/bug-reports/` (§7.8's sidecar
/// class — outside the save and the hash).
pub fn bug_report_root() -> PathBuf {
    config_dir().join("bug-reports")
}

/// Everything a bundle carries, pre-collected by the caller — the sim
/// side (`save`, `command_log`, `tick`) comes from `csp-app`'s frame
/// loop; the panic hook fills only what it can reach.
#[derive(Debug)]
pub struct BugReportInput {
    /// One-line classification: `"fatal sim error"`, `"panic"`, or
    /// `"manual report"`.
    pub title: &'static str,
    /// The halt reason / panic text / operator note.
    pub error: String,
    /// The sim tick at the halt, when a sim was involved.
    pub tick: Option<u64>,
    /// The encoded `CSPSAVE` bytes, or the reason the snapshot was
    /// refused (a post-`Fatal` queue can make `snapshot()` refuse —
    /// the refusal itself is diagnostic and lands in `error.txt`).
    pub save: Result<Vec<u8>, String>,
    /// The encoded command log, or its encode failure.
    pub command_log: Result<Vec<u8>, String>,
    /// The §3.1 data-set hash of the live `data/`, when computable.
    pub data_set_hash: Option<[u8; 32]>,
    /// The GPU adapter name, when the session had one.
    pub gpu_adapter: Option<String>,
    /// The wgpu backend label, when the session had one.
    pub gpu_backend: Option<String>,
    /// The last [`crate::logring::TAIL_LINES`] log lines.
    pub log_tail: Vec<String>,
}

/// Failures of the bundle writer — reported, never panicked on: the
/// crash path must survive a full disk and a missing config dir.
#[derive(Debug)]
pub enum BundleError {
    /// The bundle directory could not be created or a file write failed.
    Io {
        /// The path that failed.
        path: PathBuf,
        /// The underlying error.
        source: io::Error,
    },
}

impl fmt::Display for BundleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(f, "io at {}: {source}", path.display()),
        }
    }
}

impl std::error::Error for BundleError {}

/// A wall-clock bundle-dir stamp — `secs-nanos-seq` keeps names sortable
/// by wall time and collision-free across rapid reports. Seconds alone
/// collide on a double-fault inside one second (a panic mid-`Fatal`
/// handling, or two `--bug-report` runs); the nanosecond field plus a
/// process counter make each name unique. Wall time is the app domain's
/// to use (§2.1 binds the sim, not the shell).
fn stamp() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!(
        "{}-{:09}-{:04}",
        d.as_secs(),
        d.subsec_nanos(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    )
}

/// Write a complete bundle under `root/<stamp>-<slug>/` and return its
/// path. Every artifact is attempted — a single failed write never
/// orphans the rest of the bundle; the first failure is returned after
/// all files are attempted so the report stays best-effort.
pub fn write_bug_report(
    root: &Path,
    slug: &str,
    input: &BugReportInput,
) -> Result<PathBuf, BundleError> {
    let dir = root.join(format!("{}-{slug}", stamp()));
    write_bundle_dir(&dir, input).map(|()| dir)
}

/// The directory-taking half of [`write_bug_report`] — what tests drive
/// (deterministic dir names) and what [`write_bug_report`] calls after
/// stamping its own name.
pub fn write_bundle_dir(dir: &Path, input: &BugReportInput) -> Result<(), BundleError> {
    let mut first_err: Option<BundleError> = None;
    let mut write = |name: &str, bytes: &[u8]| {
        // Every artifact is attempted — a failed write never skips the
        // rest; only the first failure is kept for the return value.
        if let Err(e) = std::fs::write(dir.join(name), bytes)
            && first_err.is_none()
        {
            first_err = Some(BundleError::Io {
                path: dir.join(name),
                source: e,
            });
        }
    };
    std::fs::create_dir_all(dir).map_err(|source| BundleError::Io {
        path: dir.to_path_buf(),
        source,
    })?;
    write(
        "README.txt",
        b"Bug report bundle for Clanker Space Program.\n\
          Send this whole folder to the developer. It contains the save\n\
          (save.cspsave), the deterministic command log (commands.bin),\n\
          the determinism fingerprint (fingerprint.json), the halt\n\
          reason (error.txt), system info (system.txt), and the last\n\
          500 log lines (log.txt).\n",
    );
    let mut error_text = format!("{}\nerror: {}\n", input.title, input.error);
    if let Some(tick) = input.tick {
        error_text.push_str(&format!("sim tick: {tick}\n"));
    }
    if let Err(reason) = &input.save {
        error_text.push_str(&format!("snapshot refused: {reason}\n"));
    }
    if let Err(reason) = &input.command_log {
        error_text.push_str(&format!("command-log encode failed: {reason}\n"));
    }
    write("error.txt", error_text.as_bytes());
    write("system.txt", system_info(input).as_bytes());
    write("fingerprint.json", fingerprint(input).as_bytes());
    if let Ok(save) = &input.save {
        write("save.cspsave", save);
    }
    if let Ok(log) = &input.command_log {
        write("commands.bin", log);
    }
    write("log.txt", (input.log_tail.join("\n") + "\n").as_bytes());
    match first_err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// The panic path: no sim access, so the bundle is the error text +
/// system info + the log tail — the same shape, the sim files absent.
/// Best-effort by contract (it runs inside the panic hook): returns the
/// dir on success, `None` on any failure.
pub fn write_panic_bundle(root: &Path, panic_text: &str, log_tail: Vec<String>) -> Option<PathBuf> {
    let input = BugReportInput {
        title: "panic",
        error: panic_text.to_string(),
        tick: None,
        save: Err("no sim access inside the panic hook".into()),
        command_log: Err("no sim access inside the panic hook".into()),
        data_set_hash: None,
        gpu_adapter: None,
        gpu_backend: None,
        log_tail,
    };
    write_bug_report(root, "panic", &input).ok()
}

/// `system.txt` — the OS/arch/adapter report. Std-only fields (there
/// is no runtime rustc query; the toolchain version lives in the
/// §8.4 fingerprint sidecar which a build.rs supplies — noted here
/// honestly rather than faked).
fn system_info(input: &BugReportInput) -> String {
    let mut s = format!(
        "os: {}\narch: {}\nfamily: {}\ngame_version: {}\n",
        std::env::consts::OS,
        std::env::consts::ARCH,
        std::env::consts::FAMILY,
        env!("CARGO_PKG_VERSION"),
    );
    if let Some(a) = &input.gpu_adapter {
        s.push_str(&format!("gpu_adapter: {a}\n"));
    }
    if let Some(b) = &input.gpu_backend {
        s.push_str(&format!("gpu_backend: {b}\n"));
    }
    s
}

/// `fingerprint.json` — the §7.1-style version stamp so a red report is
/// diagnosable: every version that can change a hash without a sim bug,
/// plus the live data-set hash (§7.7's drift key).
fn fingerprint(input: &BugReportInput) -> String {
    let hash_hex = input
        .data_set_hash
        .map(|h| h.iter().map(|b| format!("{b:02x}")).collect::<String>());
    serde_json::to_string_pretty(&serde_json::json!({
        "game_version": env!("CARGO_PKG_VERSION"),
        "save_version": csp_view::save::SAVE_VERSION_CURRENT,
        "hash_schema_version": csp_core::HASH_SCHEMA_VERSION,
        "kdf_version": csp_core::KDF_VERSION,
        "data_hash_version": csp_view::DATA_HASH_VERSION,
        "data_set_hash": hash_hex,
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
    }))
    .unwrap_or_else(|_| "{}".into())
}
