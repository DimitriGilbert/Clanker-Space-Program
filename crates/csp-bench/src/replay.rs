//! The §7.6 replay artifact: recording, replay reconstruction, and golden
//! file I/O — plus the §7.1 determinism fingerprint that sits beside every
//! golden.
//!
//! Artifact shape (§7.6 + `final_tick`): `{ fingerprint, save_seed,
//! data_fixture_hash, initial snapshot, final_tick, Vec<(CommandKey,
//! SimCommand)> }`, bincode-encoded with a fixed configuration.
//! **Not a save format** — replays are version-scoped by T3 and carry no
//! migration promise (§7.2, §7.7).
//!
//! Golden layout (§8.3): `data/test-fixtures/<scenario>/` holds the frozen
//! data files plus three harness artifacts —
//! `replay.bin` (the bincode artifact), `expected-hash.txt` (the recorded
//! final state hash, lowercase hex per §7.1's golden-file convention), and
//! `fingerprint.json` (the §7.1 sidecar). Artifact files are *outputs* of
//! recording, so the fixture hash excludes them — hashing them would be
//! self-referential.

use std::fmt;
use std::path::{Path, PathBuf};

use csp_core::{HASH_SCHEMA_VERSION, KDF_VERSION, SimError};
use csp_data::DATA_HASH_VERSION;
use csp_sim::orbit::PatchedConics;
use csp_sim::{CommandKey, SimCommand, SourceClass, World, WorldSnapshot};
use serde::{Deserialize, Serialize};

use crate::scenarios::ScenarioRecipe;

/// §7.6 wire format. Fixed for the life of the artifacts — replays are
/// version-scoped by T3, so the configuration is chosen once and
/// documented, never negotiated. `standard` is bincode 2.x's canonical
/// default (varint lengths, little-endian fixed-width ints); it reports
/// bytes read rather than rejecting trailing bytes itself — [`decode`]
/// performs that check explicitly.
fn bincode_config() -> bincode::config::Configuration {
    bincode::config::standard()
}

/// The bincode artifact inside a golden directory (§7.6).
pub const REPLAY_FILE: &str = "replay.bin";
/// The recorded final state hash, lowercase hex + newline (§7.1).
pub const EXPECTED_HASH_FILE: &str = "expected-hash.txt";
/// The §7.1 determinism fingerprint sidecar.
pub const FINGERPRINT_FILE: &str = "fingerprint.json";
/// Golden artifacts are recording outputs, not fixture data —
/// [`fixture_hash`] skips exactly these names, and only at the
/// fixture dir's top level: a *nested* file that happens to share a
/// name is fixture data and must hash like any other. Public so
/// bless-goldens keys "is this dir a golden?" on the full set — a
/// *partial* artifact set (e.g. `expected-hash.txt` without
/// `replay.bin`) must never read as brand-new.
pub const ARTIFACT_FILES: &[&str] = &[REPLAY_FILE, EXPECTED_HASH_FILE, FINGERPRINT_FILE];

/// The §7.1 determinism fingerprint — every field that can legitimately
/// change a golden hash without a sim bug, so a red build is diagnosable
/// instead of "something changed".
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fingerprint {
    /// `rustc --version` of the compiler that built this binary, captured
    /// by `build.rs` (there is no supported runtime query).
    pub rustc_version: String,
    /// The target triple this binary was built for (build.rs `$TARGET`).
    pub target_triple: String,
    /// Exact resolved versions from `Cargo.lock`; `None` = absent from the
    /// build tree (parry/rapier land with csp-phys — absent is honest today).
    pub parry3d_f64: Option<String>,
    /// See [`Self::parry3d_f64`].
    pub rapier3d_f64: Option<String>,
    /// See [`Self::parry3d_f64`].
    pub libm: Option<String>,
    /// See [`Self::parry3d_f64`].
    pub rand_chacha: Option<String>,
    /// See [`Self::parry3d_f64`].
    pub blake3: Option<String>,
    /// `csp_core::HASH_SCHEMA_VERSION` (§7.1 rule 1).
    pub hash_schema_version: u32,
    /// `csp_core::KDF_VERSION` (§2.7).
    pub kdf_version: u8,
    /// `csp_data::DATA_HASH_VERSION` (§3.1).
    pub data_hash_version: u32,
    /// §3.1 hash of the scenario's frozen fixture dir, golden artifacts
    /// excluded. Lowercase hex inside `fingerprint.json` (via the
    /// `FingerprintSidecar` view — inside `replay.bin` this is the raw
    /// 32 bytes, not ASCII hex).
    pub data_fixture_hash: [u8; 32],
}

/// The `fingerprint.json` sidecar shape — field-identical to
/// [`Fingerprint`] but with the fixture hash hex-rendered for human
/// diffing (§7.1 wants the fingerprint to *explain* red builds). A
/// distinct serde type because the same [`Fingerprint`] also rides inside
/// `replay.bin`, where ASCII hex would bloat the binary wire for zero
/// diagnostic gain.
#[derive(Serialize, Deserialize)]
struct FingerprintSidecar {
    rustc_version: String,
    target_triple: String,
    parry3d_f64: Option<String>,
    rapier3d_f64: Option<String>,
    libm: Option<String>,
    rand_chacha: Option<String>,
    blake3: Option<String>,
    hash_schema_version: u32,
    kdf_version: u8,
    data_hash_version: u32,
    #[serde(with = "hex_hash")]
    data_fixture_hash: [u8; 32],
}

impl From<&Fingerprint> for FingerprintSidecar {
    fn from(f: &Fingerprint) -> Self {
        Self {
            rustc_version: f.rustc_version.clone(),
            target_triple: f.target_triple.clone(),
            parry3d_f64: f.parry3d_f64.clone(),
            rapier3d_f64: f.rapier3d_f64.clone(),
            libm: f.libm.clone(),
            rand_chacha: f.rand_chacha.clone(),
            blake3: f.blake3.clone(),
            hash_schema_version: f.hash_schema_version,
            kdf_version: f.kdf_version,
            data_hash_version: f.data_hash_version,
            data_fixture_hash: f.data_fixture_hash,
        }
    }
}

impl From<FingerprintSidecar> for Fingerprint {
    fn from(s: FingerprintSidecar) -> Self {
        Self {
            rustc_version: s.rustc_version,
            target_triple: s.target_triple,
            parry3d_f64: s.parry3d_f64,
            rapier3d_f64: s.rapier3d_f64,
            libm: s.libm,
            rand_chacha: s.rand_chacha,
            blake3: s.blake3,
            hash_schema_version: s.hash_schema_version,
            kdf_version: s.kdf_version,
            data_hash_version: s.data_hash_version,
            data_fixture_hash: s.data_fixture_hash,
        }
    }
}

/// The §7.6 replay artifact. The recorded final state hash is deliberately
/// NOT a field — the artifact is an input description; the expectation
/// lives in [`EXPECTED_HASH_FILE`] beside it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Replay {
    /// The build/data environment this was recorded under (§7.1).
    pub fingerprint: Fingerprint,
    /// The §2.7 save seed the run was recorded under. A cross-check,
    /// not an input: `record` stamps the seed into the world *before*
    /// the initial snapshot, so [`World::restore`] already carries it —
    /// [`replay`] verifies the field agrees with the snapshot and a
    /// doctored/spliced value is [`ReplayError::MalformedGolden`].
    pub save_seed: u64,
    /// §3.1 hash of the frozen fixture the run loaded (§8.3). Raw bytes on
    /// the binary wire — the hex rendering lives only in `fingerprint.json`
    /// (via the `FingerprintSidecar` view).
    pub data_fixture_hash: [u8; 32],
    /// The world's §7.2 serialized set before the run — what
    /// [`World::restore`] replays from.
    pub initial_snapshot: WorldSnapshot,
    /// The clock's tick at the end of the recorded run. The log's
    /// `AdvanceTo` commands drive the reconstruction forward, but this
    /// field *pins the end*: [`replay`] finishes by checking the restored
    /// clock against it ([`ReplayError::ClockMismatch`]), so a missing or
    /// extra advance is a named failure, not a laundered hash diff.
    pub final_tick: u64,
    /// Every applied command in strictly increasing `CommandKey` order —
    /// the replay program (§2.2.1).
    pub command_log: Vec<(CommandKey, SimCommand)>,
    /// The world's construction config at record time (§7.3 — the
    /// data-derived orbit model plus the physics environment and
    /// dominant body, when present). The artifact carries it so the
    /// reconstruction is self-contained: [`replay`] rebuilds the same
    /// construction from the artifact rather than re-running the
    /// recipe's data loads.
    pub config: csp_sim::WorldConfig,
}

/// A fresh recording: the artifact plus the final hash the run produced.
/// The hash stays out of the artifact per §7.6; it is written to
/// [`EXPECTED_HASH_FILE`].
#[derive(Clone, Debug)]
pub struct RecordedGolden {
    /// The §7.6 artifact.
    pub artifact: Replay,
    /// `state_hash` of the world after the recorded run.
    pub final_hash: [u8; 32],
}

/// Failures of the replay harness. These are tooling verdicts, not
/// sim-state failures — deliberately NOT [`SimError`]s (§2.13's four
/// classes describe the sim's own failure contract; a stale stamp or a
/// hash mismatch is not one of them).
#[derive(Debug)]
pub enum ReplayError {
    /// File-system failure while reading a fixture or writing a golden.
    Io {
        /// The path that failed.
        path: PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
    /// bincode encode of the artifact failed.
    Encode(bincode::error::EncodeError),
    /// bincode decode failed — truncated or foreign artifact bytes.
    Decode(bincode::error::DecodeError),
    /// A fingerprint sidecar is not the JSON this harness writes.
    Json(serde_json::Error),
    /// The sim itself failed during record or replay (§2.13).
    Sim(SimError),
    /// A recipe's `initial` failed to load its fixture data (§8.3's
    /// frozen input).
    Load(csp_data::LoadError),
    /// A symlink sits inside the fixture dir — the hash walks the
    /// committed file tree only; a link's target could change outside
    /// it. Mirrors `csp_data::DataHashError::Symlink`.
    Symlink {
        /// The link.
        path: PathBuf,
    },
    /// A fixture file name is not UTF-8 — the hash keys files by path
    /// bytes, and a lossy conversion could collide two distinct names.
    /// Mirrors `csp_data::DataHashError::NonUtf8Name`.
    NonUtf8Name {
        /// The file.
        path: PathBuf,
    },
    /// A golden file exists but is not in the documented format.
    MalformedGolden(&'static str),
    /// A log entry is stamped at a tick the restored clock has already
    /// passed — a corrupt or foreign artifact; the clock cannot retreat
    /// (§2.3), so the entry could never reproduce its stamp.
    StampBehindClock {
        /// The entry's recorded tick.
        tick: u64,
        /// The restored clock at the point the entry was reached.
        clock: u64,
    },
    /// The replayed drain produced different `(CommandKey, SimCommand)`
    /// entries than the log records — stamping diverged; determinism is
    /// already broken before the hash comparison.
    StampMismatch {
        /// Index into the log of the first divergent entry — equal to
        /// `command_log.len()` when the divergence is a spurious entry
        /// produced past the log's end.
        index: usize,
    },
    /// [`replay`] produced a final hash that differs from the recorded
    /// expectation — the golden's verdict.
    HashMismatch {
        /// `expected-hash.txt`.
        expected: [u8; 32],
        /// What the reconstruction produced.
        actual: [u8; 32],
    },
    /// The reconstruction ended at a different tick than the artifact
    /// recorded. `Replay::final_tick` pins the run's terminal advance:
    /// a missing or extra `AdvanceTo` fails here by name rather than
    /// surfacing laundered through [`Self::HashMismatch`].
    ClockMismatch {
        /// `Replay::final_tick` — where the recorded run ended.
        expected: u64,
        /// Where the replayed clock actually stopped.
        actual: u64,
    },
    /// A `step_frame` call inside [`replay`]'s partial-resumption loop
    /// made zero progress toward its target. The §2.6.5 budget can split
    /// an advance across calls but never stall one — zero progress is a
    /// harness/sim bug, not a normal partial.
    AdvanceStalled {
        /// The tick `step_frame` was asked to reach.
        target: u64,
        /// The tick the clock remained at.
        reached: u64,
    },
    /// A freshly recorded artifact is unfit to write — either its own
    /// replay failed to reproduce the run, or the log it captured is
    /// empty (a vacuous golden [`verify_golden`] would refuse as
    /// malformed). [`record`] (and therefore `xtask bless-goldens`,
    /// which records through it) checks this before anything may be
    /// written: a recorder-contract violation or a harness bug,
    /// refused here rather than discovered by CI later.
    SelfCheckFailed(String),
    /// The on-disk fixture dir no longer hashes to the recorded
    /// `data_fixture_hash`. The §8.3 fixture freeze is a hard failure in
    /// [`verify_golden`] — unlike toolchain/dep fingerprint drift, which
    /// stays advisory per §7.1.
    FixtureDrift {
        /// `Replay::data_fixture_hash` — the hash at record time.
        recorded: [u8; 32],
        /// What the on-disk fixture dir hashes to now.
        actual: [u8; 32],
    },
}

impl fmt::Display for ReplayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(f, "io at {}: {source}", path.display()),
            Self::Encode(e) => write!(f, "bincode encode: {e}"),
            Self::Decode(e) => write!(f, "bincode decode: {e}"),
            Self::Json(e) => write!(f, "fingerprint.json: {e}"),
            Self::Sim(e) => write!(f, "sim: {e}"),
            Self::Load(e) => write!(f, "fixture data: {e}"),
            Self::Symlink { path } => {
                write!(f, "symlink in the fixture dir at {}", path.display())
            }
            Self::NonUtf8Name { path } => {
                write!(f, "non-UTF-8 file name at {}", path.display())
            }
            Self::MalformedGolden(r) => write!(f, "malformed golden: {r}"),
            Self::StampBehindClock { tick, clock } => write!(
                f,
                "log entry stamped at tick {tick} but the restored clock already stands at {clock}"
            ),
            Self::StampMismatch { index } => write!(
                f,
                "replayed command at log index {index} stamped differently than recorded"
            ),
            Self::HashMismatch { expected, actual } => write!(
                f,
                "state hash mismatch: expected {}, replayed {}",
                hex(expected),
                hex(actual)
            ),
            Self::ClockMismatch { expected, actual } => write!(
                f,
                "clock mismatch: recorded run ended at tick {expected}, replay ended at {actual}"
            ),
            Self::AdvanceStalled { target, reached } => write!(
                f,
                "step_frame made zero progress toward tick {target} (clock held at {reached})"
            ),
            Self::SelfCheckFailed(detail) => {
                write!(f, "record self-check failed — refusing to write: {detail}")
            }
            Self::FixtureDrift { recorded, actual } => write!(
                f,
                "fixture drift: recorded fixture hash {}, on-disk fixture hashes {}",
                hex(recorded),
                hex(actual)
            ),
        }
    }
}

impl std::error::Error for ReplayError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Encode(e) => Some(e),
            Self::Decode(e) => Some(e),
            Self::Json(e) => Some(e),
            Self::Sim(e) => Some(e),
            Self::Load(e) => Some(e),
            _ => None,
        }
    }
}

impl From<SimError> for ReplayError {
    fn from(e: SimError) -> Self {
        Self::Sim(e)
    }
}

impl From<csp_data::LoadError> for ReplayError {
    fn from(e: csp_data::LoadError) -> Self {
        Self::Load(e)
    }
}

/// The §7.1 fingerprint for this build. Toolchain identity and dep versions
/// are captured by `build.rs` into `CSP_*` env vars — the fingerprint
/// records what *built* the binary, which is the only honest source.
/// `option_env!` degrades to `unknown`/`None` rather than failing builds in
/// contexts where the build script's vars are absent.
pub fn fingerprint(data_fixture_hash: [u8; 32]) -> Fingerprint {
    fn dep(v: Option<&'static str>) -> Option<String> {
        v.filter(|s| !s.is_empty()).map(str::to_string)
    }
    Fingerprint {
        rustc_version: option_env!("CSP_RUSTC_VERSION")
            .unwrap_or("unknown")
            .to_string(),
        target_triple: option_env!("CSP_TARGET_TRIPLE")
            .unwrap_or("unknown")
            .to_string(),
        parry3d_f64: dep(option_env!("CSP_DEP_VERSION_PARRY3D_F64")),
        rapier3d_f64: dep(option_env!("CSP_DEP_VERSION_RAPIER3D_F64")),
        libm: dep(option_env!("CSP_DEP_VERSION_LIBM")),
        rand_chacha: dep(option_env!("CSP_DEP_VERSION_RAND_CHACHA")),
        blake3: dep(option_env!("CSP_DEP_VERSION_BLAKE3")),
        hash_schema_version: HASH_SCHEMA_VERSION,
        kdf_version: KDF_VERSION,
        data_hash_version: DATA_HASH_VERSION,
        data_fixture_hash,
    }
}

/// One line per fingerprint field that differs between the recorded golden
/// and this build — the §7.1 diagnostic ("without it, every dependency bump
/// is an unexplained red build"). `current_fixture_hash` re-hashes the
/// fixture dir so a changed fixture is itself a reported diff.
pub fn fingerprint_warnings(recorded: &Fingerprint, current_fixture_hash: [u8; 32]) -> Vec<String> {
    let current = fingerprint(current_fixture_hash);
    let mut out = Vec::new();
    let mut diff = |field: &str, was: String, now: String| {
        out.push(format!("{field}: recorded {was}, current {now}"));
    };
    let opt = |v: &Option<String>| v.as_deref().unwrap_or("absent").to_string();
    if recorded.rustc_version != current.rustc_version {
        diff(
            "rustc_version",
            recorded.rustc_version.clone(),
            current.rustc_version.clone(),
        );
    }
    if recorded.target_triple != current.target_triple {
        diff(
            "target_triple",
            recorded.target_triple.clone(),
            current.target_triple.clone(),
        );
    }
    for (name, was, now) in [
        ("parry3d-f64", &recorded.parry3d_f64, &current.parry3d_f64),
        (
            "rapier3d-f64",
            &recorded.rapier3d_f64,
            &current.rapier3d_f64,
        ),
        ("libm", &recorded.libm, &current.libm),
        ("rand_chacha", &recorded.rand_chacha, &current.rand_chacha),
        ("blake3", &recorded.blake3, &current.blake3),
    ] {
        if was != now {
            diff(name, opt(was), opt(now));
        }
    }
    for (name, was, now) in [
        (
            "HASH_SCHEMA_VERSION",
            u64::from(recorded.hash_schema_version),
            u64::from(current.hash_schema_version),
        ),
        (
            "KDF_VERSION",
            u64::from(recorded.kdf_version),
            u64::from(current.kdf_version),
        ),
        (
            "DATA_HASH_VERSION",
            u64::from(recorded.data_hash_version),
            u64::from(current.data_hash_version),
        ),
    ] {
        if was != now {
            diff(name, was.to_string(), now.to_string());
        }
    }
    if recorded.data_fixture_hash != current.data_fixture_hash {
        diff(
            "data_fixture_hash",
            hex(&recorded.data_fixture_hash),
            hex(&current.data_fixture_hash),
        );
    }
    out
}

/// §3.1's data-set hash applied to a frozen fixture dir (§8.3): BLAKE3-256
/// over `path_bytes || 0x00 || u64-LE len || canonicalized_file_bytes`
/// for every file, in ascending path order — the length prefix keeps the
/// frame injective (the retired `path || 0x00 || content` framing was
/// not; see `csp_data::datahash`). Canonicalization mirrors §3.1 — LF
/// line endings, trailing whitespace stripped — for UTF-8 files; a
/// non-UTF-8 file hashes raw (its bytes are already canonical — the
/// scheme is defined for text data, and fixture sets are text).
///
/// Golden artifact files (`replay.bin`, `expected-hash.txt`,
/// `fingerprint.json`) are outputs of recording, not inputs, and are
/// excluded at the dir's top level — including them would be
/// self-referential. A *nested* file sharing one of those names is
/// ordinary fixture data and hashes like any other.
/// Changing this scheme bumps `csp_data::DATA_HASH_VERSION` (v2: the
/// length-prefix above — keep this implementation byte-identical to
/// `csp_data::datahash::data_set_hash`).
pub fn fixture_hash(dir: &Path) -> Result<[u8; 32], ReplayError> {
    let mut h = blake3::Hasher::new();
    for rel in fixture_files(dir)? {
        let bytes = read_file(&dir.join(&rel))?;
        feed_file(&mut h, &rel, &canonicalize(&bytes));
    }
    Ok(*h.finalize().as_bytes())
}

/// One file's contribution to the §3.1 stream — byte-identical to
/// `csp_data::datahash`'s per-file frame: `path || 0x00 || u64-LE len ||
/// content`.
fn feed_file(h: &mut blake3::Hasher, rel: &str, content: &[u8]) {
    h.update(rel.as_bytes());
    h.update(&[0]);
    h.update(&(content.len() as u64).to_le_bytes());
    h.update(content);
}

/// Every non-artifact file under `dir` as a sorted list of `/`-separated
/// relative paths — ascending path order per §3.1, independent of the
/// filesystem's own enumeration order. The walk mirrors
/// `csp_data::datahash`'s `collect` entry-for-entry: `file_type()` is
/// queried *without* following symlinks (a link is refused, not
/// traversed — `path.is_dir()` would follow it into a cycle or out of
/// the hashed tree) and file names must be UTF-8, because the hash
/// stream keys entries by their relative-path bytes and a lossy
/// conversion could collide two distinct names.
fn fixture_files(dir: &Path) -> Result<Vec<String>, ReplayError> {
    let mut out = Vec::new();
    let mut stack = vec![(dir.to_path_buf(), String::new())];
    while let Some((d, prefix)) = stack.pop() {
        let entries = std::fs::read_dir(&d).map_err(|source| ReplayError::Io {
            path: d.clone(),
            source,
        })?;
        for entry in entries {
            let entry = entry.map_err(|source| ReplayError::Io {
                path: d.clone(),
                source,
            })?;
            let ft = entry.file_type().map_err(|source| ReplayError::Io {
                path: entry.path(),
                source,
            })?;
            if ft.is_symlink() {
                return Err(ReplayError::Symlink { path: entry.path() });
            }
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| ReplayError::NonUtf8Name { path: entry.path() })?;
            let rel = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            if ft.is_dir() {
                stack.push((entry.path(), rel));
                continue;
            }
            // Golden artifacts are recording outputs, not fixture data —
            // excluded at the top level only (an empty `prefix`); a
            // nested file sharing an artifact's name is fixture data.
            if prefix.is_empty() && ARTIFACT_FILES.contains(&name.as_str()) {
                continue;
            }
            out.push(rel);
        }
    }
    out.sort();
    Ok(out)
}

/// §3.1 canonicalization: LF line endings, trailing whitespace stripped —
/// applied to UTF-8 text; non-UTF-8 bytes hash verbatim.
fn canonicalize(bytes: &[u8]) -> Vec<u8> {
    match std::str::from_utf8(bytes) {
        Ok(text) => text
            .lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n")
            .into_bytes(),
        Err(_) => bytes.to_vec(),
    }
}

/// Run a recipe and capture the §7.6 artifact: snapshot before driving,
/// command log and terminal tick after. `fixture_dir` is the scenario's
/// frozen data set — in the canonical layout it is the golden dir itself
/// (§8.3).
///
/// The artifact is **self-verified before it is returned**: [`replay`]
/// must reconstruct the recorded final hash from it, or the recipe
/// violated the recorder contract (see [`crate::scenarios`]) / the harness
/// is broken — either way the answer is [`ReplayError::SelfCheckFailed`]
/// and nothing is fit to write. An **empty command log** is refused the
/// same way — a vacuous golden that [`verify_golden`] would reject as
/// [`ReplayError::MalformedGolden`]. `xtask bless-goldens` records through
/// this function, so the check gates both writers.
pub fn record(recipe: &ScenarioRecipe, fixture_dir: &Path) -> Result<RecordedGolden, ReplayError> {
    let data_fixture_hash = fixture_hash(fixture_dir)?;
    let mut world = (recipe.initial)(fixture_dir)?;
    // The seed selects the contract generator's draws — stamp it
    // before the baseline snapshot so the artifact's initial state
    // carries it.
    world.set_save_seed(recipe.save_seed);
    let initial_snapshot = world.snapshot()?;
    let config = world.config();
    (recipe.drive)(&mut world)?;
    let final_hash = world.state_hash()?;
    let command_log = world.command_log().to_vec();
    // An empty log writes a vacuous golden — one `verify_golden`
    // refuses as `MalformedGolden`. Refuse it here instead: the
    // record path must never produce an artifact the verify path
    // rejects on sight.
    if command_log.is_empty() {
        return Err(ReplayError::SelfCheckFailed(format!(
            "scenario '{}' produced an empty command log — a golden \
             with nothing to replay is vacuous",
            recipe.name
        )));
    }
    let artifact = Replay {
        fingerprint: fingerprint(data_fixture_hash),
        save_seed: recipe.save_seed,
        data_fixture_hash,
        initial_snapshot,
        final_tick: world.clock().ticks(),
        command_log,
        config,
    };
    match replay(&artifact) {
        Ok(actual) if actual == final_hash => {}
        Ok(actual) => {
            return Err(ReplayError::SelfCheckFailed(format!(
                "scenario '{}' recorded hash {} but its own artifact replays {}",
                recipe.name,
                hex(&final_hash),
                hex(&actual)
            )));
        }
        Err(e) => {
            return Err(ReplayError::SelfCheckFailed(format!(
                "scenario '{}' artifact fails its own replay: {e}",
                recipe.name
            )));
        }
    }
    Ok(RecordedGolden {
        artifact,
        final_hash,
    })
}

/// Feed an artifact's command log back through a world restored from its
/// initial snapshot; return the resulting state hash — the §7.6
/// reconstruction, and the actual determinism check.
///
/// Discipline (§2.2.1): recorded `CommandKey`s are ground truth, but the
/// tick is stamped at *dequeue*, so a command must sit in the queue while
/// the clock stands at its recorded tick. The log is processed in
/// same-tick groups — a tick's commands all drained in one boundary pass
/// during recording, so they are enqueued together here:
///
/// 1. advance the restored clock to the group tick — the queue is
///    *externally* empty (every earlier group already drained) but may
///    hold not-yet-due `Script`/`Autopilot` entries the sim's own
///    control ticks issued along the way — those are expected, they
///    drain at their recorded ticks;
/// 2. enqueue the group's externally-sourced entries under their
///    recorded sources — within one source, log order is production
///    order, so the restored `source_seq` counters reproduce the
///    recorded `seq`s. **`Script`/`Autopilot`-class entries are NOT
///    enqueued** (see "sim-derived entries" below);
/// 3. advance past the drain — the boundary that stamps the recorded
///    `tick`, plus whatever target the group's `AdvanceTo`s raise.
///
/// Sim-derived entries (§2.10): the restored world re-runs the same
/// script `on_tick`s and autopilot ticks the recording ran, and each
/// re-issues its commands into `pending` with the same one-tick
/// `not_before` latency — they drain and log themselves. Two
/// consequences for the driver:
///
/// - Re-enqueueing a recorded `Script`/`Autopilot` entry would stamp a
///   *second* copy, so those entries are never enqueued — they are
///   expectations, and the produced-vs-log comparison still verifies
///   the sim reproduced them entry-for-entry.
/// - A warp advance produces a whole span of them at once: the
///   covering `advance_to` logs every sim-derived entry up to the
///   target, so `produced` legitimately runs ahead of the current
///   group, and later groups can sit at ticks the clock already
///   passed. A group that is *entirely* sim-derived at `tick < clock`
///   is therefore compare-only (its entries must already sit in
///   `produced`); a non-sim entry behind the clock remains
///   [`ReplayError::StampBehindClock`], because an external stamp
///   requires the recorder's clock to have stood at that tick —
///   something a warp span never does.
///
/// The documented ambiguity this leaves: an entry stamped
/// `Script`/`Autopilot` that was enqueued *externally* at record time
/// (a driver forging the sim-internal classes — `World::enqueue` does
/// not police the class it is handed) is filtered out here too and
/// never re-issued, so `record`'s self-check refuses such a recipe
/// rather than writing an unreplayable golden.
///
/// The `step_frame` targets are read back out of the log, never computed
/// from anything outside it — the recorder contract (see
/// [`crate::scenarios`]) is that a frame's `step_frame` target equals its
/// drain tick + 1 and every further advance rides a logged `AdvanceTo`
/// command (§2.6.3). So a group's effective target is reconstructed as
/// `max(tick + 1, the group's AdvanceTo args)` — exactly the target the
/// recording frame ran to.
///
/// [`AdvanceReport::partial`](csp_sim::AdvanceReport::partial) is honored,
/// not ignored (§2.6.5): each advance loops `step_frame` until the target
/// is reached, and a call that makes zero progress fails
/// [`ReplayError::AdvanceStalled`].
///
/// After each drain the produced log entries are compared against the
/// recording: divergence is [`ReplayError::StampMismatch`]. The run is
/// then pinned at its recorded end — [`Replay::final_tick`]: a
/// reconstruction that stops anywhere else fails
/// [`ReplayError::ClockMismatch`], before the hash comparison.
pub fn replay(artifact: &Replay) -> Result<[u8; 32], ReplayError> {
    // The construction config is §7.3 data-derived state the artifact
    // carries verbatim — the orbit model + physics environment the run
    // was recorded against.
    let mut world = World::restore(
        PatchedConics::new(artifact.config.orbits.clone()),
        artifact.config.physics.clone(),
        artifact.initial_snapshot.clone(),
    )?;
    // §7.3 data-derived config like the orbit model — the artifact's
    // recorded `experiments` table is what the science commands
    // validated against at record time.
    world.set_experiments(artifact.config.experiments.clone());
    // M7-B: the merged tech tree rides the same channel — a recorded
    // `tech_tree` is what the run's research commands validated
    // against; `None` means the record ran tree-less.
    if let Some(tree) = &artifact.config.tech_tree {
        world.set_tech_tree(tree.clone());
    }
    // M7-C: the site table rides the same channel — a recorded run's
    // `RecoverVessel` resolves its site against it; without it the
    // command rejects and the log diverges.
    world.set_sites(artifact.config.sites.clone());
    world.set_contracts(artifact.config.contracts.clone());
    // M8-A: the per-body colony environments ride the same channel —
    // a recorded run's `FoundColony`/terminator math resolves
    // rotation and insolation against it.
    world.set_colony_env(artifact.config.colony_env.clone());
    // M8-B: the recipe table rides the same channel — a recorded
    // run's `FoundColony`/`ManufactureCraft` resolves `RecipeId`s
    // against it.
    world.set_recipes(artifact.config.recipes.clone());
    // M9a §2.10: the script host + autopilot driver are the same
    // construction-config channel — a recorded run whose snapshot
    // carried scripts/autopilots replays their `on_tick` calls; the
    // per-save op budget is serialized sim state and restores with
    // the snapshot (the engine install forwards it).
    world.set_script_engine(Box::new(csp_script::RhaiHost::new()));
    world.set_autopilot_driver(Box::new(csp_ai::AscentAutopilot::new()));
    // §2.7: `Replay::save_seed` is a cross-check — `restore` already
    // took the snapshot's seed (the record stamped it pre-snapshot), so
    // a disagreement is a doctored or spliced artifact, not an input to
    // apply.
    if world.save_seed() != artifact.save_seed {
        return Err(ReplayError::MalformedGolden(
            "replay.save_seed disagrees with the initial snapshot's save_seed",
        ));
    }
    let log = &artifact.command_log;
    // §2.10 producers re-derive their own log entries (see the fn doc):
    // `Script`/`Autopilot`-class entries are expectations, not inputs.
    let is_sim_derived =
        |class: SourceClass| matches!(class, SourceClass::Script | SourceClass::Autopilot);
    let mut i = 0;
    while i < log.len() {
        let group_tick = log[i].0.tick;
        let mut j = i;
        while j < log.len() && log[j].0.tick == group_tick {
            j += 1;
        }
        let group = &log[i..j];
        let clock = world.clock().ticks();
        if group_tick < clock {
            // A covering warp advance already ran this tick's drain —
            // only sim-derived entries can legitimately sit here
            // (their producing control ticks re-issued them en route).
            // An external entry behind the clock can never reproduce
            // its stamp: the recorder's clock had to stand at the tick
            // to log it, and a warp span never does.
            if group.iter().any(|(k, _)| !is_sim_derived(k.source_class)) {
                return Err(ReplayError::StampBehindClock {
                    tick: group_tick,
                    clock,
                });
            }
        } else {
            advance_to(&mut world, group_tick)?;
            // The drain frame's step target was drain tick + 1 at
            // record time; logged AdvanceTo commands extend it, so the
            // effective target is their max — read back from the log
            // itself. A sim-derived AdvanceTo raised the recorded
            // frame's target too — the scan covers the whole group
            // regardless of which entries get enqueued.
            let mut target = group_tick
                .checked_add(1)
                .ok_or(SimError::Fatal("replay log stamps u64::MAX tick"))?;
            for (key, command) in group {
                if let SimCommand::AdvanceTo(t) = command {
                    target = target.max(*t);
                }
                if !is_sim_derived(key.source_class) {
                    world.enqueue(key.source_class, key.source_id, command.clone());
                }
            }
            advance_to(&mut world, target)?;
        }
        // `produced` may run ahead of `j` — a warp advance logs a
        // whole span of sim-derived entries at once — so the check is
        // the group's window, not the produced length.
        let produced = world.command_log();
        if produced.len() < j || produced[i..j] != log[i..j] {
            // Report the first divergent entry, not the group's start:
            // the earliest position where the drain's output differs
            // from the log (a produced log missing the group's tail
            // diverges at `produced.len()`).
            let index = (i..j)
                .find(|&k| produced.get(k) != log.get(k))
                .unwrap_or_else(|| produced.len().min(j));
            return Err(ReplayError::StampMismatch { index });
        }
        i = j;
    }
    // Produced entries past the log's end — a spurious extra the
    // recording never saw (the windowed check only ever covers logged
    // spans, and produced may legitimately have run ahead).
    if world.command_log().len() != log.len() {
        return Err(ReplayError::StampMismatch { index: log.len() });
    }
    let actual = world.clock().ticks();
    if actual != artifact.final_tick {
        return Err(ReplayError::ClockMismatch {
            expected: artifact.final_tick,
            actual,
        });
    }
    Ok(world.state_hash()?)
}

/// Drive `world` to `target`, looping `step_frame` while the §2.6.5
/// back-pressure budget splits the advance into partial frames — the
/// harness-side form of "the caller re-issues the remaining advance next
/// frame". A call that returns without advancing the clock at all is
/// [`ReplayError::AdvanceStalled`]: the budget may yield a frame but can
/// never produce zero progress below target, so a stall means the
/// harness or the sim is broken, not that the artifact is large.
fn advance_to(world: &mut World, target: u64) -> Result<(), ReplayError> {
    while world.clock().ticks() < target {
        let before = world.clock().ticks();
        let report = world.step_frame(target)?;
        // A deferred over-ceiling event batch (§2.6.5's per-batch ceiling)
        // can legitimately return zero-advance frames while flushing the
        // tail — that IS progress. A stall is literally nothing happening.
        if report.reached_tick <= before
            && report.events_processed == 0
            && report.commands_applied == 0
            && report.rejected.is_empty()
            && report.degraded.is_empty()
        {
            return Err(ReplayError::AdvanceStalled {
                target,
                reached: report.reached_tick,
            });
        }
    }
    Ok(())
}

/// The golden verdict: [`replay`] output vs the recorded expectation.
pub fn verify(artifact: &Replay, expected: [u8; 32]) -> Result<(), ReplayError> {
    let actual = replay(artifact)?;
    if actual == expected {
        Ok(())
    } else {
        Err(ReplayError::HashMismatch { expected, actual })
    }
}

/// A successful [`verify_golden`]: the reproduced hash plus the advisory
/// §7.1 fingerprint drift lines. Fixture drift is a hard error in that
/// path, so `warnings` only ever covers toolchain/dep/schema drift.
#[derive(Clone, Debug)]
pub struct Verified {
    /// The reconstructed final state hash — equal to `expected-hash.txt`.
    pub hash: [u8; 32],
    /// Advisory §7.1 fingerprint diffs between the recorded golden and
    /// this build — diagnostic context, never part of the verdict.
    pub warnings: Vec<String>,
}

/// The strict §8.3 golden check — the path `csp-replay verify` and CI's
/// golden step run. Loads the committed artifact from `dir`, then:
///
/// 1. **doctored-artifact check** — `Replay::data_fixture_hash` must equal
///    the fingerprint *inside the same artifact*, and the on-disk
///    `fingerprint.json` sidecar must equal the fingerprint `replay.bin`
///    embeds; the copies are written together, so disagreement means a
///    tampered or foreign file ([`ReplayError::MalformedGolden`]). The log
///    must also be non-empty — an empty one would make a golden vacuous.
/// 2. **fixture freeze** — the on-disk fixture dir is re-hashed and must
///    equal the recorded hash; drift is [`ReplayError::FixtureDrift`], a
///    hard error, not a printed note.
/// 3. **replay verdict** — [`replay`] reconstructs the run; its final
///    hash must equal `expected-hash.txt`.
///
/// Toolchain/dep fingerprint drift stays advisory per §7.1: it rides out
/// in [`Verified::warnings`] for the caller to print, and never changes
/// the verdict.
pub fn verify_golden(dir: &Path) -> Result<Verified, ReplayError> {
    let (artifact, expected) = load_golden(dir)?;
    if artifact.command_log.is_empty() {
        return Err(ReplayError::MalformedGolden(
            "replay.bin has an empty command log — a vacuous golden",
        ));
    }
    if artifact.data_fixture_hash != artifact.fingerprint.data_fixture_hash {
        return Err(ReplayError::MalformedGolden(
            "replay.bin's data_fixture_hash disagrees with its embedded fingerprint",
        ));
    }
    if load_fingerprint(dir)? != artifact.fingerprint {
        return Err(ReplayError::MalformedGolden(
            "fingerprint.json disagrees with the fingerprint embedded in replay.bin",
        ));
    }
    let current_fixture_hash = fixture_hash(dir)?;
    if current_fixture_hash != artifact.data_fixture_hash {
        return Err(ReplayError::FixtureDrift {
            recorded: artifact.data_fixture_hash,
            actual: current_fixture_hash,
        });
    }
    let warnings = fingerprint_warnings(&artifact.fingerprint, current_fixture_hash);
    verify(&artifact, expected)?;
    Ok(Verified {
        hash: expected,
        warnings,
    })
}

/// bincode-encode the artifact with the fixed §7.6 config.
pub fn encode(artifact: &Replay) -> Result<Vec<u8>, ReplayError> {
    bincode::serde::encode_to_vec(artifact, bincode_config()).map_err(ReplayError::Encode)
}

/// bincode-decode an artifact produced by [`encode`]. Rejects trailing
/// bytes — `replay.bin` is a whole-file artifact, so a short read means
/// appended garbage or a truncated-then-padded file, not a valid artifact.
pub fn decode(bytes: &[u8]) -> Result<Replay, ReplayError> {
    let (artifact, bytes_read) =
        bincode::serde::decode_from_slice::<Replay, _>(bytes, bincode_config())
            .map_err(ReplayError::Decode)?;
    if bytes_read != bytes.len() {
        return Err(ReplayError::MalformedGolden(
            "replay.bin has trailing bytes after the artifact",
        ));
    }
    Ok(artifact)
}

/// Write a golden directory's three artifacts (§8.3): `replay.bin`,
/// `expected-hash.txt`, `fingerprint.json`.
pub fn write_golden(dir: &Path, golden: &RecordedGolden) -> Result<(), ReplayError> {
    std::fs::create_dir_all(dir).map_err(|source| ReplayError::Io {
        path: dir.to_path_buf(),
        source,
    })?;
    write_file(&dir.join(REPLAY_FILE), &encode(&golden.artifact)?)?;
    write_file(
        &dir.join(EXPECTED_HASH_FILE),
        format!("{}\n", hex(&golden.final_hash)).as_bytes(),
    )?;
    let json =
        serde_json::to_string_pretty(&FingerprintSidecar::from(&golden.artifact.fingerprint))
            .map_err(ReplayError::Json)?;
    write_file(&dir.join(FINGERPRINT_FILE), format!("{json}\n").as_bytes())
}

/// Load a golden directory: the §7.6 artifact and the expected final hash.
pub fn load_golden(dir: &Path) -> Result<(Replay, [u8; 32]), ReplayError> {
    let artifact = decode(&read_file(&dir.join(REPLAY_FILE))?)?;
    let text = read_string(&dir.join(EXPECTED_HASH_FILE))?;
    let expected = parse_hex(text.trim()).ok_or(ReplayError::MalformedGolden(
        "expected-hash.txt is not 64 hex chars",
    ))?;
    Ok((artifact, expected))
}

/// Load the `fingerprint.json` sidecar of a golden directory — used by
/// `xtask bless-goldens` to compare schema versions (§8.4) and by
/// [`verify_golden`]'s doctored-artifact check.
pub fn load_fingerprint(dir: &Path) -> Result<Fingerprint, ReplayError> {
    let text = read_string(&dir.join(FINGERPRINT_FILE))?;
    let sidecar: FingerprintSidecar = serde_json::from_str(&text).map_err(ReplayError::Json)?;
    Ok(sidecar.into())
}

/// Lowercase hex, §7.1's golden-file convention.
pub fn hex(hash: &[u8; 32]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(64);
    for b in hash {
        // Writing to a String is infallible; write! can't fail here.
        let _ = write!(s, "{b:02x}");
    }
    s
}

fn parse_hex(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 || !s.is_ascii() {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(s.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(out)
}

fn read_file(path: &Path) -> Result<Vec<u8>, ReplayError> {
    std::fs::read(path).map_err(|source| ReplayError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn read_string(path: &Path) -> Result<String, ReplayError> {
    std::fs::read_to_string(path).map_err(|source| ReplayError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<(), ReplayError> {
    std::fs::write(path, bytes).map_err(|source| ReplayError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// serde adapter: `[u8; 32]` as lowercase hex — used ONLY by
/// `FingerprintSidecar`, the JSON view of [`Fingerprint`], so
/// `fingerprint.json` stays human-diffable (§7.1 wants the fingerprint to
/// *explain* red builds; an array of 32 decimal numbers would not). The
/// binary artifact never carries this adapter: `replay.bin` encodes the
/// hash as raw bytes.
mod hex_hash {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(h: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&super::hex(h))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
        let s = String::deserialize(d)?;
        super::parse_hex(&s)
            .ok_or_else(|| serde::de::Error::custom("expected 64 lowercase hex chars"))
    }
}

#[cfg(test)]
// §2.13's no-panic rule covers shipping fallible paths; tests signal
// failure by panicking, so unwrap/expect stays legal inside test code.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::scenarios;
    use csp_sim::{MAX_TICKS_PER_FRAME, SourceClass};

    /// The committed trivial golden (§8.3).
    fn trivial_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/test-fixtures/trivial")
    }

    /// A unique-per-run temp dir that removes itself on drop — cleanup
    /// holds on panic paths too, not just the happy one, and the
    /// pid+nanos name never collides with a parallel or leftover run.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let path = std::env::temp_dir()
                .join(format!("csp-bench-{tag}-{}-{nanos}", std::process::id()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// The M0 critical path: load the committed golden, replay it, and the
    /// reconstructed hash must equal the recorded expectation.
    #[test]
    fn golden_trivial_round_trips() {
        let dir = trivial_dir();
        let (artifact, expected) = load_golden(&dir).unwrap();
        // Assert on the committed artifact's contents — the bytes loaded
        // from disk, not a fresh recording: the log is non-empty, and the
        // same-tick Ui-before-External ordering the scenario exists to
        // exercise (§2.2.1 class order) is actually present in it.
        let log = &artifact.command_log;
        assert!(!log.is_empty());
        assert!(log.windows(2).any(|w| {
            w[0].0.tick == w[1].0.tick
                && w[0].0.source_class == SourceClass::Ui
                && w[1].0.source_class == SourceClass::External
        }));
        verify(&artifact, expected).unwrap();
        // The fingerprint sidecar agrees with the artifact it sits
        // beside, and the strict golden path — the one CI runs — accepts.
        assert_eq!(load_fingerprint(&dir).unwrap(), artifact.fingerprint);
        verify_golden(&dir).unwrap();
    }

    /// Record -> replay -> verify without touching the filesystem: the
    /// artifact carries everything needed to reproduce the hash, and
    /// [`record`] itself already ran that check before returning.
    #[test]
    fn record_replay_round_trip_in_memory() {
        let golden = record(scenarios::find("trivial").unwrap(), &trivial_dir()).unwrap();
        assert_eq!(replay(&golden.artifact).unwrap(), golden.final_hash);
        verify(&golden.artifact, golden.final_hash).unwrap();
    }

    /// A run whose log carries `SourceClass::Script` entries — the
    /// in-sim host's own issues — records, self-checks, and verifies
    /// clean. The restored world re-derives them through its control
    /// ticks; enqueueing the recorded copies again would stamp a
    /// duplicate of every one (`StampMismatch`) — the filtered path
    /// is what makes scripted runs recordable at all.
    #[test]
    fn scripted_recipe_records_and_verifies() {
        use std::collections::BTreeMap;

        use csp_core::{DQuat, DVec3};
        use csp_data::BodyId;
        use csp_sim::orbit::BodyOrbit;
        use csp_sim::{PhysicsConfig, VesselId};

        /// A single-body world with a physics environment and the Rhai
        /// host installed — the minimal stage for a `Script`-class
        /// command producer.
        fn scripted_initial(_dir: &Path) -> Result<World, ReplayError> {
            let mut bodies = BTreeMap::new();
            bodies.insert(
                BodyId::from("p"),
                BodyOrbit {
                    mu_m3_per_s2: 3.5316e12,
                    soi_m: None,
                    parent: None,
                    elements: None,
                },
            );
            let mut world = World::with_physics(
                PatchedConics::new(bodies),
                PhysicsConfig {
                    environment: csp_phys::Environment {
                        body_center: DVec3::ZERO,
                        radius_m: 600_000.0,
                        mu_m3_per_s2: 3.5316e12,
                        body_spin: DVec3::ZERO,
                        atmosphere: None,
                        settle_v: 0.05,
                        settle_w: 0.05,
                        settle_ticks: 128,
                        friction_mu: 0.5,
                        drag_cd: 1.0,
                    },
                    dominant: BodyId::from("p"),
                },
                csp_sim::DEFAULT_MAX_EVENTS_PER_FRAME,
            );
            world.set_script_engine(Box::new(csp_script::RhaiHost::new()));
            Ok(world)
        }

        /// Orbiter spawn → `LoadScript` → control ticks — the guest's
        /// `set_throttle` lands `Script`-stamped entries in the log
        /// every tick it ticks, including inside the `AdvanceTo` warp.
        fn drive_scripted(w: &mut World) -> Result<(), SimError> {
            let pos = DVec3::new(800_000.0, 0.0, 0.0);
            w.enqueue(
                SourceClass::Ui,
                0,
                SimCommand::SpawnPhysicalVessel {
                    craft: scenarios::flight_craft(),
                    pos,
                    rot: DQuat::IDENTITY,
                    vel: DVec3::new(0.0, libm::sqrt(3.5316e12 / pos.norm()), 0.0),
                    ang_vel: DVec3::ZERO,
                },
            );
            w.step_frame(w.clock().ticks() + 1)?;
            w.enqueue(
                SourceClass::Ui,
                0,
                SimCommand::LoadScript {
                    vessel: VesselId(0),
                    source: "fn on_tick(ctx) { set_throttle(0.5); }".to_string(),
                },
            );
            w.step_frame(w.clock().ticks() + 1)?;
            // Recorder contract: each frame's target is its drain
            // tick + 1; longer strides ride a logged `AdvanceTo`.
            for _ in 0..8 {
                w.step_frame(w.clock().ticks() + 1)?;
            }
            w.enqueue(SourceClass::Ui, 0, SimCommand::AdvanceTo(64));
            w.step_frame(w.clock().ticks() + 1)?;
            Ok(())
        }

        const RECIPE: ScenarioRecipe = ScenarioRecipe {
            name: "scripted-probe",
            save_seed: 0,
            initial: scripted_initial,
            drive: drive_scripted,
        };

        let golden = record(&RECIPE, &trivial_dir()).unwrap();
        // The run really did exercise the §2.10 producer path — the
        // log must carry `Script`-stamped entries, not just the
        // externally-enqueued commands.
        assert!(
            golden
                .artifact
                .command_log
                .iter()
                .any(|(k, _)| k.source_class == SourceClass::Script),
            "scripted run logged no Script-class commands"
        );
        assert_eq!(replay(&golden.artifact).unwrap(), golden.final_hash);
        verify(&golden.artifact, golden.final_hash).unwrap();
    }

    /// §7.6 wire stability: encode -> decode is an exact round-trip.
    #[test]
    fn bincode_round_trip_is_exact() {
        let golden = record(scenarios::find("trivial").unwrap(), &trivial_dir()).unwrap();
        let decoded = decode(&encode(&golden.artifact).unwrap()).unwrap();
        assert_eq!(decoded, golden.artifact);
    }

    /// `replay.bin` is a whole-file artifact: bytes the decoder did not
    /// consume are appended garbage, not a valid artifact.
    #[test]
    fn decode_rejects_trailing_bytes() {
        let golden = record(scenarios::find("trivial").unwrap(), &trivial_dir()).unwrap();
        let mut bytes = encode(&golden.artifact).unwrap();
        bytes.extend_from_slice(&[0xAA, 0xBB]);
        assert!(matches!(
            decode(&bytes),
            Err(ReplayError::MalformedGolden(_))
        ));
    }

    /// A command stamped at a tick the clock already passed cannot
    /// reproduce its stamp — the artifact is rejected, never silently
    /// mis-replayed.
    #[test]
    fn replay_rejects_stamp_behind_clock() {
        let mut artifact = record(scenarios::find("trivial").unwrap(), &trivial_dir())
            .unwrap()
            .artifact;
        // Restore from a snapshot whose clock is already past the first
        // logged tick — the entry can never reproduce its stamp.
        artifact.initial_snapshot.clock_ticks = 10_000;
        assert!(matches!(
            replay(&artifact),
            Err(ReplayError::StampBehindClock { .. })
        ));
    }

    /// A tampered seq is caught at the drain, before the hash comparison.
    #[test]
    fn replay_detects_stamp_divergence() {
        let mut artifact = record(scenarios::find("trivial").unwrap(), &trivial_dir())
            .unwrap()
            .artifact;
        artifact.command_log[1].0.source_seq += 1;
        assert!(matches!(
            replay(&artifact),
            Err(ReplayError::StampMismatch { .. })
        ));
    }

    /// Same-tick entries swapped in the log, keys intact: the drain
    /// stamps `CommandKey` order from the queue, so the recorded (wrong)
    /// order can never be reproduced — `StampMismatch`, not a hash diff.
    #[test]
    fn same_tick_key_reorder_is_stamp_mismatch() {
        let mut artifact = record(scenarios::find("trivial").unwrap(), &trivial_dir())
            .unwrap()
            .artifact;
        // The tick-500 pair: recorded Ui-then-External, doctored to
        // External-then-Ui.
        artifact.command_log.swap(1, 2);
        assert!(matches!(
            replay(&artifact),
            Err(ReplayError::StampMismatch { .. })
        ));
    }

    /// Same-tick payloads swapped, keys intact: stamps still reproduce —
    /// the wrong command text lands in final state and the verdict is
    /// `HashMismatch`.
    #[test]
    fn same_tick_payload_swap_is_hash_mismatch() {
        let golden = record(scenarios::find("trivial").unwrap(), &trivial_dir()).unwrap();
        let mut artifact = golden.artifact.clone();
        let (a, b) = (
            artifact.command_log[1].1.clone(),
            artifact.command_log[2].1.clone(),
        );
        artifact.command_log[1].1 = b;
        artifact.command_log[2].1 = a;
        assert!(matches!(
            verify(&artifact, golden.final_hash),
            Err(ReplayError::HashMismatch { .. })
        ));
    }

    /// A doctored `final_tick` is caught as `ClockMismatch` — the run's
    /// end is pinned by name, never laundered through the hash diff.
    #[test]
    fn doctored_final_tick_is_clock_mismatch() {
        let mut artifact = record(scenarios::find("trivial").unwrap(), &trivial_dir())
            .unwrap()
            .artifact;
        artifact.final_tick += 1;
        assert!(matches!(
            replay(&artifact),
            Err(ReplayError::ClockMismatch { .. })
        ));
    }

    /// A tick gap between groups larger than `MAX_TICKS_PER_FRAME`
    /// (69 120): recording resumes the partial frame, and replay must
    /// loop `step_frame` the same way — the §2.6.5 budget bounds one
    /// call, never the run.
    #[test]
    fn replay_resumes_partials_across_large_tick_gaps() {
        let gap = MAX_TICKS_PER_FRAME + 7;
        let mut world = World::new();
        world.enqueue(SourceClass::Ui, 0, SimCommand::AdvanceTo(gap));
        // Recorder contract: the drain frame's target is drain tick + 1;
        // the partial is then resumed at the logged AdvanceTo target.
        world.step_frame(1).unwrap();
        while world.clock().ticks() < gap {
            world.step_frame(gap).unwrap();
        }
        world.enqueue(SourceClass::Ui, 0, SimCommand::SetWarp(2));
        world.step_frame(gap + 1).unwrap(); // drains at `gap`, +1 tick
        let artifact = Replay {
            fingerprint: fingerprint([0; 32]),
            save_seed: 0,
            data_fixture_hash: [0; 32],
            initial_snapshot: World::new().snapshot().unwrap(),
            final_tick: world.clock().ticks(),
            command_log: world.command_log().to_vec(),
            config: world.config(),
        };
        assert_eq!(replay(&artifact).unwrap(), world.state_hash().unwrap());
    }

    /// `verify_golden` enforces the §8.3 fixture freeze: a mutated
    /// fixture file is a hard `FixtureDrift`, and a `fingerprint.json`
    /// that disagrees with `replay.bin` is a doctored artifact —
    /// errors, not advisory notes.
    #[test]
    fn verify_golden_enforces_fixture_freeze() {
        let tmp = TempDir::new("golden-freeze");
        let src = trivial_dir();
        for entry in std::fs::read_dir(&src).unwrap().flatten() {
            let p = entry.path();
            if p.extension().is_some_and(|e| e == "ron") {
                std::fs::copy(&p, tmp.0.join(p.file_name().unwrap())).unwrap();
            }
        }
        let golden = record(scenarios::find("trivial").unwrap(), &tmp.0).unwrap();
        write_golden(&tmp.0, &golden).unwrap();
        verify_golden(&tmp.0).unwrap();

        // Drift the fixture — a real content change, not a trailing
        // newline the §3.1 canonicalization would strip.
        let fixture = tmp.0.join("fixture.ron");
        let mut bytes = std::fs::read(&fixture).unwrap();
        bytes.extend_from_slice(b"// drift\n");
        std::fs::write(&fixture, &bytes).unwrap();
        assert!(matches!(
            verify_golden(&tmp.0),
            Err(ReplayError::FixtureDrift { .. })
        ));

        // Restore the fixture, then doctor the sidecar: the on-disk
        // fingerprint.json must agree with the fingerprint replay.bin
        // embeds.
        std::fs::copy(src.join("fixture.ron"), &fixture).unwrap();
        let sidecar = tmp.0.join(FINGERPRINT_FILE);
        let json = std::fs::read_to_string(&sidecar)
            .unwrap()
            .replace(&hex(&golden.artifact.data_fixture_hash), &hex(&[0xAA; 32]));
        std::fs::write(&sidecar, json).unwrap();
        assert!(matches!(
            verify_golden(&tmp.0),
            Err(ReplayError::MalformedGolden(_))
        ));

        // Rewrite the golden, then doctor the fingerprint *inside*
        // replay.bin: the two fixture-hash copies must agree within the
        // artifact too.
        write_golden(&tmp.0, &golden).unwrap();
        let mut tampered = golden.artifact.clone();
        tampered.fingerprint.data_fixture_hash = [0xBB; 32];
        write_file(&tmp.0.join(REPLAY_FILE), &encode(&tampered).unwrap()).unwrap();
        assert!(matches!(
            verify_golden(&tmp.0),
            Err(ReplayError::MalformedGolden(_))
        ));
    }

    /// A tampered command payload still reproduces its stamp — the
    /// divergence surfaces at the hash, which is the golden's verdict.
    /// (Log index 2 is the last writer in the tick-500 drain, so changing
    /// it actually moves final state.)
    #[test]
    fn tampered_command_fails_verify() {
        let golden = record(scenarios::find("trivial").unwrap(), &trivial_dir()).unwrap();
        let mut artifact = golden.artifact;
        artifact.command_log[2].1 = SimCommand::SetWarp(99);
        assert!(matches!(
            verify(&artifact, golden.final_hash),
            Err(ReplayError::HashMismatch { .. })
        ));
    }

    /// The fingerprint carries every §7.1 field from the live constants.
    #[test]
    fn fingerprint_pins_schema_versions() {
        let fp = fingerprint([0; 32]);
        assert_eq!(fp.hash_schema_version, HASH_SCHEMA_VERSION);
        assert_eq!(fp.kdf_version, KDF_VERSION);
        assert_eq!(fp.data_hash_version, DATA_HASH_VERSION);
        // These deps are in the tree today; parry/rapier are honestly absent.
        assert!(fp.blake3.is_some());
        assert!(fp.rand_chacha.is_some());
        assert!(fp.parry3d_f64.is_none());
    }

    /// §8.3's "ALL tuning lives in balance.ron" made checkable:
    /// `csp_sim::DEFAULT_MAX_EVENTS_PER_FRAME` is a documented MIRROR of
    /// the loaded file — this pins the mirror to the real value so the
    /// two can never drift silently. The §2.9 object caps mirror the
    /// same way: `VesselCaps`'s `DEFAULT_MAX_VESSELS`/`DEFAULT_MAX_DEBRIS`
    /// are what worlds built without a balance file (tests, the wasm
    /// slice) run, so the authored file and the compiled defaults must
    /// agree.
    #[test]
    fn balance_ron_mirrors_sim_event_budget() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/balance.ron");
        let b = csp_data::load_balance(&path).unwrap();
        assert_eq!(
            b.advance_to_max_events_per_frame,
            csp_sim::DEFAULT_MAX_EVENTS_PER_FRAME
        );
        assert_eq!(b.max_tracked_vessels, csp_sim::DEFAULT_MAX_VESSELS);
        assert_eq!(b.max_debris, csp_sim::DEFAULT_MAX_DEBRIS);
        // M6-B §2.4 docking mirrors — same pinning contract: the
        // compiled `default_config()`/`DEFAULT_*`s are what data-less
        // worlds run, so they must agree with the authored file.
        let d = csp_sim::DockingConfig::from_balance(&b);
        let m = csp_sim::joints::default_config();
        assert_eq!(b.max_live_joints_per_bubble, m.max_live_joints);
        assert_eq!(b.capture_window_ticks, m.capture_window_ticks);
        assert_eq!(d.max_live_joints, m.max_live_joints);
        assert_eq!(d.capture_window_ticks, m.capture_window_ticks);
        assert_eq!(d.capture_range_m, m.capture_range_m);
        assert_eq!(d.capture_min_alignment_dot, m.capture_min_alignment_dot);
        assert_eq!(d.capture_rel_speed_mps, m.capture_rel_speed_mps);
        assert_eq!(d.soft_pull_per_s, m.soft_pull_per_s);
        assert_eq!(d.soft_max_correction_mps, m.soft_max_correction_mps);
        // M6-C latch tunables — same pinning contract.
        assert_eq!(d.latch_separation_m, m.latch_separation_m);
        assert_eq!(d.latch_rel_speed_mps, m.latch_rel_speed_mps);
        assert_eq!(
            b.dock_latch_separation_m,
            csp_sim::DEFAULT_LATCH_SEPARATION_M
        );
        assert_eq!(
            b.dock_latch_rel_speed_mps,
            csp_sim::DEFAULT_LATCH_REL_SPEED_MPS
        );
        // And the literal `DEFAULT_*` mirrors the file names.
        assert_eq!(b.dock_capture_range_m, csp_sim::DEFAULT_CAPTURE_RANGE_M);
        assert_eq!(
            b.capture_window_ticks,
            csp_sim::DEFAULT_CAPTURE_WINDOW_TICKS
        );
        assert_eq!(
            b.max_live_joints_per_bubble,
            csp_sim::DEFAULT_MAX_LIVE_JOINTS
        );
    }

    /// An entry stamped at a tick the log has already moved past is
    /// rejected — the log order is ground truth, the driver never reorders.
    #[test]
    fn replay_rejects_non_monotonic_log() {
        let mut artifact = record(scenarios::find("trivial").unwrap(), &trivial_dir())
            .unwrap()
            .artifact;
        // Append a copy of the tick-0 entry after the tick-501 group: by
        // then the clock stands at 1000, so the entry's stamp is
        // unreachable.
        let entry = artifact.command_log[0].clone();
        artifact.command_log.push(entry);
        assert!(matches!(
            replay(&artifact),
            Err(ReplayError::StampBehindClock { .. })
        ));
    }

    /// Fixture hashing is canonical: artifact files are ignored and the
    /// digest is stable across calls.
    #[test]
    fn fixture_hash_is_stable_and_excludes_artifacts() {
        let dir = trivial_dir();
        let a = fixture_hash(&dir).unwrap();
        assert_eq!(a, fixture_hash(&dir).unwrap());
        // Artifacts must not move the data-fixture hash: hashing a dir
        // that contains only fixture data equals hashing the golden dir.
        let tmp = TempDir::new("fixture-hash");
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let p = entry.path();
            if p.extension().is_some_and(|e| e == "ron") {
                std::fs::copy(&p, tmp.0.join(p.file_name().unwrap())).unwrap();
            }
        }
        assert_eq!(a, fixture_hash(&tmp.0).unwrap());
    }

    /// The fixture walk is a byte-identical mirror of
    /// `csp_data::datahash`'s walk on their overlapping scope: a plain
    /// file tree (no `test-fixtures/` dir, no `art/` exclusions, no
    /// artifact files) must produce the same §3.1 hash from both
    /// functions, and nested paths must join identically.
    #[test]
    fn fixture_walk_mirrors_data_set_hash() {
        let tmp = TempDir::new("fixture-mirror");
        std::fs::write(tmp.0.join("a.ron"), b"one  \n\ntwo\r\n").unwrap();
        std::fs::create_dir(tmp.0.join("sub")).unwrap();
        std::fs::write(tmp.0.join("sub").join("b.ron"), b"three").unwrap();
        assert_eq!(
            fixture_hash(&tmp.0).unwrap(),
            csp_data::data_set_hash(&tmp.0).unwrap()
        );
    }

    /// A symlink inside the fixture dir is a named error, never
    /// followed: the walk hashes the committed tree only — the same
    /// refusal `csp_data::datahash` applies to `data/`.
    #[cfg(unix)]
    #[test]
    fn fixture_walk_refuses_symlinks() {
        let tmp = TempDir::new("fixture-symlink");
        std::fs::write(tmp.0.join("real.ron"), b"data").unwrap();
        std::os::unix::fs::symlink(tmp.0.join("real.ron"), tmp.0.join("link.ron")).unwrap();
        assert!(matches!(
            fixture_hash(&tmp.0),
            Err(ReplayError::Symlink { .. })
        ));
        assert!(matches!(
            csp_data::data_set_hash(&tmp.0),
            Err(csp_data::DataHashError::Symlink { .. })
        ));
    }

    /// A non-UTF-8 file name has no honest path-bytes key: refused by
    /// name in both walks rather than mangled through a lossy
    /// conversion that could collide two distinct names.
    #[cfg(unix)]
    #[test]
    fn fixture_walk_refuses_non_utf8_names() {
        use std::os::unix::ffi::OsStrExt as _;
        let tmp = TempDir::new("fixture-nonutf8");
        std::fs::write(
            tmp.0.join(std::ffi::OsStr::from_bytes(b"bad-\xFF.ron")),
            b"data",
        )
        .unwrap();
        assert!(matches!(
            fixture_hash(&tmp.0),
            Err(ReplayError::NonUtf8Name { .. })
        ));
        assert!(matches!(
            csp_data::data_set_hash(&tmp.0),
            Err(csp_data::DataHashError::NonUtf8Name { .. })
        ));
    }

    /// `SourceClass` ordering is exercised by the trivial log: the tick-500
    /// drain lists Ui before External (§2.2.1 class order).
    #[test]
    fn trivial_log_exercises_class_order() {
        let golden = record(scenarios::find("trivial").unwrap(), &trivial_dir()).unwrap();
        let log = &golden.artifact.command_log;
        assert_eq!(log.len(), 4);
        assert_eq!(log[1].0.source_class, SourceClass::Ui);
        assert_eq!(log[2].0.source_class, SourceClass::External);
        assert_eq!(log[1].0.tick, log[2].0.tick);
    }
}
