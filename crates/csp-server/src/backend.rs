//! M9c §2.12 — the host seam.
//!
//! [`ControlBackend`] is what the interface tier talks to: **read**
//! telemetry off the published snapshot, **mutate** through the
//! bounded ingress, and **session** through the save/snapshot verbs.
//! Two hosts ship with it:
//!
//! - [`LocalGame`] — the synchronous headless host: it owns a `World`
//!   on the caller's thread, drains ingress into `step_frame` on
//!   submit (the headless lockstep), and re-publishes the `ArcSwap`
//!   snapshot after every mutation. `csp-mcp` (the stdio bin) and
//!   unit tests drive this one — `World` is `!Send` (the script
//!   engine's `Rc`-carrying host), so `LocalGame` is a single-thread
//!   host by construction.
//! - [`GameHost`] — the attach-to-a-running-game host: a dedicated
//!   thread owns the `World` (built *inside* the thread, since the
//!   type cannot move), the `ArcSwap` publishes out, and commands
//!   arrive over a channel — exactly the §2.12.3 shape a windowed
//!   `csp-app` wires to its frame loop. `GameHost` is `Send + Sync`;
//!   it is what the axum server holds.
//!
//! [`ClientRegistry`] assigns the §2.12.3 client index — `source_id`
//! of the `External` producer, "assigned in connection order,
//! recorded in the log". Streamable HTTP has no protocol-level
//! session (revision 2026-07-28), so identity is the client's
//! declared name (`X-CSP-Client` / stdio's single "local" client):
//! first sight in connection order gets the next index.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use arc_swap::ArcSwap;
use csp_sim::orbit::PatchedConics;
use csp_sim::{PhysicsConfig, SimCommand, World, WorldSnapshot};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ingress::{IngressError, IngressQueue};
use crate::telemetry::TelemetrySnapshot;

/// What a session tool reports about one save on disk.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaveInfo {
    /// The save's name (file stem).
    pub name: String,
    /// The snapshot's sim tick.
    pub tick: u64,
    /// The snapshot's state hash, lowercase hex.
    pub hash: String,
}

/// What a session verb reports after running.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SessionSummary {
    /// The world's sim tick.
    pub tick: u64,
    /// The world's state hash, lowercase hex.
    pub hash: String,
}

/// What one `submit`/`submit_batch` drain reports back — §2.2.1's
/// per-channel outcome plus where the advance actually landed. Both
/// shipped hosts drain synchronously, so the report is real data the
/// client can branch on, not a status echo.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SubmitReport {
    /// Commands the drain's frames applied.
    pub applied: u64,
    /// Drain-time rejections, human-readable — the §2.2.1 "error
    /// delivered on the originating channel" made literal.
    pub rejected: Vec<String>,
    /// `true` when the drain's frame cap (64 partial `step_frame`s)
    /// cut the advance short of its target — the clock reached
    /// `reached`, not `target`; re-issue the advance to continue it.
    /// Surfaced rather than silently swallowed: a short-of-target
    /// `Ok` would lie about where the run stands.
    pub partial: bool,
    /// The tick the clock stands at after the drain.
    pub reached: u64,
    /// The target the drain last chased — the `AdvanceTo`-extended
    /// effective target, or `clock + 1` for a plain boundary.
    pub target: u64,
}

/// A backend refusal — ingress backpressure passes through as data;
/// everything else is a session/host failure rendered to the client.
/// Drain-time command rejections are *not* errors here: they ride back
/// inside [`SubmitReport::rejected`], the §2.2.1 "error delivered on
/// the originating channel" made literal.
#[derive(Debug)]
pub enum BackendError {
    /// §2.2.1 explicit backpressure — structured, so the wire keeps
    /// the `error` tag the client can branch on.
    Ingress(IngressError),
    /// Session-side failure (missing save, refused snapshot, IO).
    Session(String),
}

impl fmt::Display for BackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ingress(e) => write!(f, "{e}"),
            Self::Session(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for BackendError {}

impl From<IngressError> for BackendError {
    fn from(e: IngressError) -> Self {
        Self::Ingress(e)
    }
}

/// The host seam — read telemetry, submit commands, session verbs.
/// Deliberately *not* `Send + Sync`-bounded: `LocalGame` (a
/// `!Send`-world host) implements it for the single-threaded stdio
/// path, while axum-side holders require
/// `Arc<dyn ControlBackend + Send + Sync>` — which [`GameHost`]
/// satisfies.
pub trait ControlBackend {
    /// The latest published snapshot — `ArcSwap`-backed (§2.12.3: "one
    /// publish point"; readers never block the frame loop). `None`
    /// only before the host's first publish.
    fn telemetry(&self) -> Option<Arc<TelemetrySnapshot>>;

    /// Push one `SimCommand` into bounded ingress under `client`.
    /// Returns the [`SubmitReport`] the drain observed — both shipped
    /// hosts drain synchronously, so the §2.2.1 "error delivered on
    /// the originating channel" arrives in the result, and a partial
    /// advance reports where the clock actually stopped.
    fn submit(&self, client: u32, command: SimCommand) -> Result<SubmitReport, BackendError>;

    /// Push a *batch* in one drain window — the form under which the
    /// §2.2.1 per-client budget (≤32 commands per drain) is reachable:
    /// commands queue together, then drain together. The default
    /// loops `submit`; hosts override for queue-then-drain semantics
    /// (and for atomicity: a refused push leaves nothing queued).
    fn submit_batch(
        &self,
        client: u32,
        commands: Vec<SimCommand>,
    ) -> Result<SubmitReport, BackendError> {
        let mut report = SubmitReport::default();
        for c in commands {
            let r = self.submit(client, c)?;
            report.applied = report.applied.saturating_add(r.applied);
            report.rejected.extend(r.rejected);
            report.partial = r.partial;
            report.reached = r.reached;
            report.target = r.target;
        }
        Ok(report)
    }

    /// Session-class: the saves this host can load. `Ok(vec![])` is
    /// honest for a save-less host.
    fn list_saves(&self) -> Result<Vec<SaveInfo>, BackendError>;

    /// Session-class: replace the running world with the named save.
    fn load_save(&self, name: &str) -> Result<SessionSummary, BackendError>;

    /// Session-class: write the world's snapshot under `name`.
    fn store_save(&self, name: &str) -> Result<SaveInfo, BackendError>;

    /// Session-class: the world's `(tick, state-hash)` summary.
    fn summary(&self) -> Result<SessionSummary, BackendError>;
}

/// The `?client=`/`X-CSP-Client` name bound — a `source_id` label is
/// a diagnostic tag, not a document. Longer names truncate (on a
/// char boundary); distinct names that collide past the cap simply
/// share an id, which aliases the *stamp* and nothing else.
pub const MAX_CLIENT_NAME: usize = 64;

/// The registry's entry bound — attacker-controlled names cannot
/// grow the map without limit (R19-F9). Past it, every new name
/// folds into [`OVERFLOW_CLIENT`]: the overflow pool shares one id,
/// so its members also share one §2.2.1 per-client ingress budget
/// among themselves — the flood starves itself, not the server.
pub const MAX_CLIENTS: usize = 256;

/// The id every name past [`MAX_CLIENTS`] resolves to — outside the
/// `0..MAX_CLIENTS` range the allocator assigns, never colliding
/// with a real client index.
pub const OVERFLOW_CLIENT: u32 = u32::MAX;

/// §2.12.3's `source_id` allocator — first-seen client names get
/// ascending indexes ("assigned in connection order"). The map is
/// live-session state, never sim state: the command log records the
/// numeric id; replay stamps it verbatim. Bounded by
/// [`MAX_CLIENT_NAME`]/[`MAX_CLIENTS`]; see their docs.
#[derive(Debug, Default)]
pub struct ClientRegistry {
    map: Mutex<BTreeMap<String, u32>>,
}

impl ClientRegistry {
    /// An empty registry — the first client seen is index 0.
    pub fn new() -> Self {
        Self::default()
    }

    /// `name`'s client index — assigned on first sight, stable for
    /// the server's life. Names are normalized to
    /// [`MAX_CLIENT_NAME`] bytes first; past [`MAX_CLIENTS`]
    /// distinct names, every newcomer gets [`OVERFLOW_CLIENT`].
    pub fn id(&self, name: &str) -> u32 {
        // Poison recovery is sound here: each critical section is a
        // single map read or insert with no multi-step invariant, so
        // a panic elsewhere can never leave a half-updated ledger.
        let mut map = self.map.lock().unwrap_or_else(|p| p.into_inner());
        let name = normalize_client_name(name);
        if let Some(id) = map.get(name) {
            return *id;
        }
        if map.len() >= MAX_CLIENTS {
            return OVERFLOW_CLIENT;
        }
        let id = map.len() as u32;
        map.insert(name.to_string(), id);
        id
    }

    /// Every `(name, id)` pair — diagnostics.
    pub fn clients(&self) -> Vec<(String, u32)> {
        self.map
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .map(|(n, i)| (n.clone(), *i))
            .collect()
    }
}

/// Truncate `name` to [`MAX_CLIENT_NAME`] bytes on a char boundary.
fn normalize_client_name(name: &str) -> &str {
    if name.len() <= MAX_CLIENT_NAME {
        return name;
    }
    let mut end = MAX_CLIENT_NAME;
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    &name[..end]
}

// ---------------------------------------------------------------------------
// HostCore — the world-side half both hosts share
// ---------------------------------------------------------------------------

/// The save file on disk — `SaveInfo` header + the bincode snapshot.
/// The replay-artifact precedent: bincode, version-scoped, no
/// migration promise (§7.6/§7.7).
#[derive(Serialize, Deserialize)]
struct SaveFile {
    /// Format tag — bumped deliberately, never migrated in-band.
    schema: u32,
    /// `World::clock().ticks()` at save time.
    tick: u64,
    /// The snapshot's state hash, hex — a sanity read, not a proof.
    hash: String,
    /// The §7.2 serialized set.
    snapshot: WorldSnapshot,
}

const SAVE_SCHEMA: u32 = 1;
const SAVE_EXT: &str = "cspsave";

fn hex(hash: &[u8; 32]) -> String {
    hash.iter().map(|b| format!("{b:02x}")).collect()
}

fn bincode_cfg() -> bincode::config::Configuration {
    bincode::config::standard()
}

fn safe_name(name: &str) -> Result<(), BackendError> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if ok {
        Ok(())
    } else {
        Err(BackendError::Session(format!(
            "save name must be 1-64 chars of [A-Za-z0-9._-], got {name:?}"
        )))
    }
}

/// The world-owning core — the code `LocalGame` runs inline and
/// `GameHost` runs on its dedicated thread. Not `Send` because
/// `World` is not: it must live on whichever thread built it.
/// The §7.3 construction-config hook type (see
/// [`LocalGame::with_reconfigure`]/[`GameHost::spawn_full`]).
type Reconfigure = Box<dyn Fn(&mut World) + Send>;

/// See [`HostCore`]'s doc on `reconfigure`.
struct HostCore {
    world: World,
    /// The construction config `World::restore` needs — orbits +
    /// physics are data-derived, never in the snapshot (§7.3).
    orbits: PatchedConics,
    physics: Option<PhysicsConfig>,
    queue: IngressQueue,
    /// Post-restore construction-config hook — engines, drivers and
    /// budgets are §7.3's data-derived channel: `session.load`
    /// re-installs them through this.
    reconfigure: Option<Reconfigure>,
    save_dir: PathBuf,
}

impl HostCore {
    fn new(
        world: World,
        save_dir: PathBuf,
        queue: IngressQueue,
        reconfigure: Option<Reconfigure>,
    ) -> Self {
        let config = world.config();
        Self {
            orbits: PatchedConics::new(config.orbits.clone()),
            physics: config.physics.clone(),
            world,
            queue,
            reconfigure,
            save_dir,
        }
    }

    /// Pop the ingress queue into the world, then step one boundary
    /// (plus any `AdvanceTo` the drain extends to, honouring
    /// `partial` by re-issuing the same *effective* target — the
    /// recorder contract). Returns the [`SubmitReport`] — including
    /// the `partial` flag when the frame cap leaves the clock short
    /// of target.
    fn drain_and_step(&mut self) -> Result<SubmitReport, BackendError> {
        self.queue.drain_into(&mut self.world);
        let mut target = self.world.clock().ticks().saturating_add(1);
        let mut report = SubmitReport::default();
        // Bounded like the frame loop: a partial advance re-issues the
        // call's *effective* target — the `AdvanceTo`-extended one the
        // report carries — never the stale `clock + 1`, which a
        // progressed clock turns into a guaranteed no-op (a warp past
        // `MAX_TICKS_PER_FRAME` would land on the first partial and
        // silently stall). A sane run finishes in one call.
        for _ in 0..64 {
            let r = self
                .world
                .step_frame(target)
                .map_err(|e| BackendError::Session(format!("step_frame: {e}")))?;
            report.applied = report.applied.saturating_add(r.commands_applied);
            report
                .rejected
                .extend(r.rejected.iter().map(|(_, e)| format!("{e}")));
            report.reached = r.reached_tick;
            report.target = r.effective_target;
            report.partial = r.partial;
            if !r.partial {
                break;
            }
            target = r.effective_target;
        }
        Ok(report)
    }

    /// Queue-then-drain: the whole batch lands in the ingress queue
    /// (budget-checked per command), then one drain applies it — the
    /// shape under which `ClientBudget`/`QueueFull` actually fire.
    /// The queue side is atomic: a mid-push refusal rolls the queued
    /// prefix back, so a failed batch leaves `pending` untouched —
    /// never residue the next unrelated submit would silently apply.
    fn submit_batch(
        &mut self,
        client: u32,
        commands: Vec<SimCommand>,
    ) -> Result<SubmitReport, BackendError> {
        for (pushed, c) in commands.into_iter().enumerate() {
            if let Err(e) = self.queue.push(client, c) {
                self.queue.rollback_last(pushed);
                return Err(BackendError::Ingress(e));
            }
        }
        self.drain_and_step()
    }

    fn save_path(&self, name: &str) -> PathBuf {
        self.save_dir.join(format!("{name}.{SAVE_EXT}"))
    }

    fn list_saves(&self) -> Result<Vec<SaveInfo>, BackendError> {
        let mut out = Vec::new();
        let dir = match std::fs::read_dir(&self.save_dir) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(BackendError::Session(format!("save dir: {e}"))),
        };
        for entry in dir.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some(SAVE_EXT) {
                continue;
            }
            let bytes = match std::fs::read(&path) {
                Ok(b) => b,
                Err(_) => continue,
            };
            let Ok((file, _)) =
                bincode::serde::decode_from_slice::<SaveFile, _>(&bytes, bincode_cfg())
            else {
                continue;
            };
            if file.schema != SAVE_SCHEMA {
                continue;
            }
            out.push(SaveInfo {
                name: path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or_default()
                    .to_string(),
                tick: file.tick,
                hash: file.hash,
            });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    fn load_save(&mut self, name: &str) -> Result<SessionSummary, BackendError> {
        safe_name(name)?;
        let bytes = std::fs::read(self.save_path(name))
            .map_err(|e| BackendError::Session(format!("load {name}: {e}")))?;
        let (file, _) = bincode::serde::decode_from_slice::<SaveFile, _>(&bytes, bincode_cfg())
            .map_err(|e| BackendError::Session(format!("decode {name}: {e}")))?;
        if file.schema != SAVE_SCHEMA {
            return Err(BackendError::Session(format!(
                "save {name}: schema {} != {SAVE_SCHEMA}",
                file.schema
            )));
        }
        let mut world = World::restore(self.orbits.clone(), self.physics.clone(), file.snapshot)
            .map_err(|e| BackendError::Session(format!("restore {name}: {e}")))?;
        // Re-install the data-derived channel the snapshot cannot
        // carry (§7.3): engines, drivers, budgets.
        if let Some(f) = self.reconfigure.as_ref() {
            f(&mut world);
        }
        self.world = world;
        self.summary()
    }

    fn store_save(&self, name: &str) -> Result<SaveInfo, BackendError> {
        safe_name(name)?;
        let snapshot = self
            .world
            .snapshot()
            .map_err(|e| BackendError::Session(format!("snapshot refused: {e}")))?;
        let hash = self
            .world
            .state_hash()
            .map_err(|e| BackendError::Session(format!("hash: {e}")))?;
        let info = SaveInfo {
            name: name.to_string(),
            tick: self.world.clock().ticks(),
            hash: hex(&hash),
        };
        let file = SaveFile {
            schema: SAVE_SCHEMA,
            tick: info.tick,
            hash: info.hash.clone(),
            snapshot,
        };
        let bytes = bincode::serde::encode_to_vec(&file, bincode_cfg())
            .map_err(|e| BackendError::Session(format!("encode: {e}")))?;
        std::fs::create_dir_all(&self.save_dir)
            .map_err(|e| BackendError::Session(format!("save dir: {e}")))?;
        std::fs::write(self.save_path(name), bytes)
            .map_err(|e| BackendError::Session(format!("write {name}: {e}")))?;
        Ok(info)
    }

    fn summary(&self) -> Result<SessionSummary, BackendError> {
        let hash = self
            .world
            .state_hash()
            .map_err(|e| BackendError::Session(format!("hash: {e}")))?;
        Ok(SessionSummary {
            tick: self.world.clock().ticks(),
            hash: hex(&hash),
        })
    }
}

// ---------------------------------------------------------------------------
// LocalGame — the synchronous headless host (stdio, tests)
// ---------------------------------------------------------------------------

/// The synchronous headless host — owns a `World` on the caller's
/// thread, drains the bounded ingress into `step_frame` on every
/// submit (one-tick latency, headless lockstep), and re-publishes the
/// `ArcSwap` snapshot after every mutation.
pub struct LocalGame {
    inner: Mutex<HostCore>,
    /// The §2.12.3 publish point — readers `load_full`, never block.
    published: ArcSwap<TelemetrySnapshot>,
}

impl LocalGame {
    /// Wrap `world`; session tools land in `save_dir` (created on
    /// first save). The starting snapshot is published immediately.
    pub fn new(world: World, save_dir: PathBuf) -> Self {
        let published = ArcSwap::from_pointee(TelemetrySnapshot::from_world(&world));
        Self {
            inner: Mutex::new(HostCore::new(world, save_dir, IngressQueue::new(), None)),
            published,
        }
    }

    /// Tighter ingress bounds — tests exercise the refusals.
    pub fn with_ingress(self, queue: IngressQueue) -> Self {
        self.inner_or_recover().queue = queue;
        self
    }

    /// Install the post-restore construction-config hook (script
    /// engine, autopilot driver, op budget — the `set_*` calls a
    /// fixture-built world needs re-applied after `session.load`).
    pub fn with_reconfigure(self, f: impl Fn(&mut World) + Send + 'static) -> Self {
        self.inner_or_recover().reconfigure = Some(Box::new(f));
        self
    }

    /// Drain queued commands into the world and step one boundary —
    /// the `submit` internals without the submit, for hosts driving
    /// the pump out-of-band. Returns the [`SubmitReport`].
    pub fn pump(&self) -> Result<SubmitReport, BackendError> {
        let mut g = self.inner()?;
        let r = g.drain_and_step()?;
        self.publish(&g);
        drop(g);
        Ok(r)
    }

    /// Read-side access to the world under the host's lock — test and
    /// diagnostic surface (assert the command log, inspect arena
    /// state) without exposing the mutex. Poison-recoverable like
    /// [`Self::inner_or_recover`]: a diagnostic *read* of a world left
    /// mid-mutation by a panic is memory-safe and best-effort — the
    /// mutation paths, where a half-drained queue would corrupt the
    /// stream, refuse instead.
    pub fn with_world<R>(&self, f: impl FnOnce(&World) -> R) -> R {
        let g = self.inner_or_recover();
        f(&g.world)
    }

    /// The host lock for the mutation/session paths — `Err` on
    /// poison: a panic inside a critical section (mid-drain,
    /// mid-restore) can leave the queue half-drained or the world
    /// half-stepped, and recovering the guard would silently resume
    /// from that state (R19-F19). Propagating the refusal is the
    /// honest answer.
    fn inner(&self) -> Result<MutexGuard<'_, HostCore>, BackendError> {
        self.inner
            .lock()
            .map_err(|_| BackendError::Session("host lock poisoned by a panic mid-mutation".into()))
    }

    /// The builder/diagnostic lock — poison-*recoverable*, unlike
    /// [`Self::inner`]: these callers only swap construction config
    /// or read the world, never continue a half-run mutation. Safe
    /// Rust keeps the `HostCore` itself coherent under a panic, so
    /// the recovered guard sees consistent memory.
    fn inner_or_recover(&self) -> MutexGuard<'_, HostCore> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn publish(&self, g: &HostCore) {
        self.published
            .store(Arc::new(TelemetrySnapshot::from_world(&g.world)));
    }
}

impl ControlBackend for LocalGame {
    fn telemetry(&self) -> Option<Arc<TelemetrySnapshot>> {
        Some(self.published.load_full())
    }

    fn submit(&self, client: u32, command: SimCommand) -> Result<SubmitReport, BackendError> {
        self.submit_batch(client, vec![command])
    }

    fn submit_batch(
        &self,
        client: u32,
        commands: Vec<SimCommand>,
    ) -> Result<SubmitReport, BackendError> {
        let report;
        {
            let mut g = self.inner()?;
            report = g.submit_batch(client, commands)?;
            self.publish(&g);
        }
        Ok(report)
    }

    fn list_saves(&self) -> Result<Vec<SaveInfo>, BackendError> {
        self.inner()?.list_saves()
    }

    fn load_save(&self, name: &str) -> Result<SessionSummary, BackendError> {
        let mut g = self.inner()?;
        let r = g.load_save(name)?;
        self.publish(&g);
        Ok(r)
    }

    fn store_save(&self, name: &str) -> Result<SaveInfo, BackendError> {
        self.inner()?.store_save(name)
    }

    fn summary(&self) -> Result<SessionSummary, BackendError> {
        self.inner()?.summary()
    }
}

// ---------------------------------------------------------------------------
// GameHost — the attach-to-a-running-game host (axum side)
// ---------------------------------------------------------------------------

/// The last-resort bound on a game-thread reply — generous enough to
/// cover the longest legitimate drain (64 budget-split `step_frame`s
/// or a big `session.load`); a *dead* thread disconnects instantly
/// and never waits this out. The HTTP layer's own timeout is far
/// shorter — this one exists so a wedged-but-alive thread can never
/// park a caller forever.
const REPLY_TIMEOUT: Duration = Duration::from_secs(120);

/// Drop's bound on the game thread's exit — a `Shutdown` request
/// normally lands the instant the current drain returns, but a thread
/// wedged *inside* a drain (a pathological `step_frame`, a deadlock)
/// would park an unbounded `join()` forever. On timeout the
/// `JoinHandle` is dropped: std offers no abort, so the thread
/// detaches and keeps running — the process stops waiting instead of
/// hanging.
const SHUTDOWN_JOIN_TIMEOUT: Duration = Duration::from_secs(10);

/// A request to the game thread — `reply` is the rendezvous channel
/// the answer rides back on.
enum Request {
    /// `ControlBackend::submit`/`submit_batch`.
    Submit {
        /// The caller's §2.12.3 client index.
        client: u32,
        /// The command(s) to enqueue in one drain window.
        commands: Vec<SimCommand>,
        /// Rejection-report rendezvous.
        reply: std::sync::mpsc::Sender<Result<SubmitReport, BackendError>>,
    },
    /// `ControlBackend::list_saves`.
    ListSaves(std::sync::mpsc::Sender<Result<Vec<SaveInfo>, BackendError>>),
    /// `ControlBackend::load_save`.
    LoadSave {
        /// The save name.
        name: String,
        /// Rendezvous.
        reply: std::sync::mpsc::Sender<Result<SessionSummary, BackendError>>,
    },
    /// `ControlBackend::store_save`.
    StoreSave {
        /// The save name.
        name: String,
        /// Rendezvous.
        reply: std::sync::mpsc::Sender<Result<SaveInfo, BackendError>>,
    },
    /// `ControlBackend::summary`.
    Summary(std::sync::mpsc::Sender<Result<SessionSummary, BackendError>>),
    /// Read access for tests/diagnostics — a `Send` closure run
    /// against `&World` on the game thread.
    Inspect {
        /// The read.
        f: Box<dyn FnOnce(&World) -> Value + Send>,
        /// Rendezvous.
        reply: std::sync::mpsc::Sender<Value>,
    },
    /// Stop the thread (Drop).
    Shutdown,
}

/// The attach-to-a-running-game host — a dedicated thread owns the
/// `World` (it is `!Send`, so the builder runs *inside* the thread
/// and the world never moves), the `ArcSwap` publishes out, and
/// commands land on a channel. `Send + Sync`: this is what axum
/// holds, and the shape `csp-app` mirrors when it attaches the same
/// backend to its frame loop (M9d).
pub struct GameHost {
    tx: std::sync::mpsc::Sender<Request>,
    /// The publish point — written by the game thread, read here.
    published: Arc<ArcSwap<TelemetrySnapshot>>,
    join: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl GameHost {
    /// Spawn the game thread: `make` builds the world *inside* the
    /// thread (a `World` cannot cross threads — the builder must be
    /// `Send`, the result needn't be), `save_dir` hosts session
    /// tools. A thread-spawn failure is a `BackendError::Session`,
    /// never a process-killing panic — a library host must surface
    /// its own failures (R19-F19).
    pub fn spawn(
        make: impl FnOnce() -> World + Send + 'static,
        save_dir: PathBuf,
    ) -> Result<Self, BackendError> {
        Self::spawn_full(make, save_dir, IngressQueue::new(), None)
    }

    /// `spawn` with explicit ingress bounds and the post-restore
    /// construction-config hook (`LocalGame::with_reconfigure`'s
    /// threaded twin).
    pub fn spawn_full(
        make: impl FnOnce() -> World + Send + 'static,
        save_dir: PathBuf,
        queue: IngressQueue,
        reconfigure: Option<Reconfigure>,
    ) -> Result<Self, BackendError> {
        let published = Arc::new(ArcSwap::from_pointee(TelemetrySnapshot::from_world(
            &World::new(),
        )));
        let pub_in = published.clone();
        let (tx, rx) = std::sync::mpsc::channel::<Request>();
        let join = std::thread::Builder::new()
            .name("csp-game".into())
            .spawn(move || {
                let mut core = HostCore::new(make(), save_dir, queue, reconfigure);
                pub_in.store(Arc::new(TelemetrySnapshot::from_world(&core.world)));
                while let Ok(req) = rx.recv() {
                    // A failed `reply.send` means *this requester*
                    // went away (a timed-out or cancelled call drops
                    // its receiver) — the host must keep serving, not
                    // exit the loop and strand every later request.
                    match req {
                        Request::Submit {
                            client,
                            commands,
                            reply,
                        } => {
                            let r = core.submit_batch(client, commands);
                            pub_in.store(Arc::new(TelemetrySnapshot::from_world(&core.world)));
                            let _ = reply.send(r);
                        }
                        Request::ListSaves(reply) => {
                            let _ = reply.send(core.list_saves());
                        }
                        Request::LoadSave { name, reply } => {
                            let r = core.load_save(&name);
                            pub_in.store(Arc::new(TelemetrySnapshot::from_world(&core.world)));
                            let _ = reply.send(r);
                        }
                        Request::StoreSave { name, reply } => {
                            let _ = reply.send(core.store_save(&name));
                        }
                        Request::Summary(reply) => {
                            let _ = reply.send(core.summary());
                        }
                        Request::Inspect { f, reply } => {
                            let _ = reply.send(f(&core.world));
                        }
                        Request::Shutdown => break,
                    }
                }
            });
        // A thread that cannot spawn is a hard failure — surface it
        // rather than serving a dead backend, and never `panic!` in a
        // library: the caller decides what a dead host means.
        let join =
            join.map_err(|e| BackendError::Session(format!("csp-game thread spawn: {e}")))?;
        Ok(Self {
            tx,
            published,
            join: Mutex::new(Some(join)),
        })
    }

    /// Read access on the game thread — the test/diagnostic channel
    /// [`LocalGame::with_world`] mirrors. Bounded by `REPLY_TIMEOUT`:
    /// a wedged game thread yields `Value::Null`, never a parked
    /// caller.
    pub fn inspect(&self, f: impl FnOnce(&World) -> Value + Send + 'static) -> Value {
        let (rtx, rrx) = std::sync::mpsc::channel();
        if self
            .tx
            .send(Request::Inspect {
                f: Box::new(f),
                reply: rtx,
            })
            .is_err()
        {
            return Value::Null;
        }
        rrx.recv_timeout(REPLY_TIMEOUT).unwrap_or(Value::Null)
    }

    /// Blocking callers: the sync [`ControlBackend`] contract means
    /// this waits on a std channel — it must only ever run on a real
    /// thread (the HTTP layer drives it through
    /// `tokio::task::spawn_blocking` so a slow drain parks a blocking
    /// thread, never a runtime worker).
    fn call<R>(
        &self,
        req: impl FnOnce(std::sync::mpsc::Sender<Result<R, BackendError>>) -> Request,
    ) -> Result<R, BackendError> {
        let (rtx, rrx) = std::sync::mpsc::channel();
        if self.tx.send(req(rtx)).is_err() {
            return Err(BackendError::Session("game thread gone".into()));
        }
        match rrx.recv_timeout(REPLY_TIMEOUT) {
            Ok(r) => r,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                Err(BackendError::Session("game thread reply timed out".into()))
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                Err(BackendError::Session("game thread gone".into()))
            }
        }
    }
}

impl Drop for GameHost {
    fn drop(&mut self) {
        let _ = self.tx.send(Request::Shutdown);
        // Poison-recoverable by construction: the only critical
        // section is the atomic `take`, so a poisoned lock can only
        // mean a prior `Drop` panicked *after* it — recovering still
        // completes the shutdown `Drop` owes the thread.
        let Some(j) = self.join.lock().unwrap_or_else(|p| p.into_inner()).take() else {
            return;
        };
        // Bounded join (see `SHUTDOWN_JOIN_TIMEOUT`): poll
        // `is_finished` — `JoinHandle::join` alone is unbounded and a
        // wedged game thread would park the dropper forever. On
        // timeout the handle drops here, detaching the thread.
        let deadline = std::time::Instant::now() + SHUTDOWN_JOIN_TIMEOUT;
        while !j.is_finished() {
            if std::time::Instant::now() >= deadline {
                eprintln!(
                    "csp-server: game thread did not exit within \
                     {SHUTDOWN_JOIN_TIMEOUT:?}; detaching"
                );
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = j.join();
    }
}

impl ControlBackend for GameHost {
    fn telemetry(&self) -> Option<Arc<TelemetrySnapshot>> {
        Some(self.published.load_full())
    }

    fn submit(&self, client: u32, command: SimCommand) -> Result<SubmitReport, BackendError> {
        self.submit_batch(client, vec![command])
    }

    fn submit_batch(
        &self,
        client: u32,
        commands: Vec<SimCommand>,
    ) -> Result<SubmitReport, BackendError> {
        self.call(|reply| Request::Submit {
            client,
            commands,
            reply,
        })
    }

    fn list_saves(&self) -> Result<Vec<SaveInfo>, BackendError> {
        self.call(Request::ListSaves)
    }

    fn load_save(&self, name: &str) -> Result<SessionSummary, BackendError> {
        self.call(|reply| Request::LoadSave {
            name: name.to_string(),
            reply,
        })
    }

    fn store_save(&self, name: &str) -> Result<SaveInfo, BackendError> {
        self.call(|reply| Request::StoreSave {
            name: name.to_string(),
            reply,
        })
    }

    fn summary(&self) -> Result<SessionSummary, BackendError> {
        self.call(Request::Summary)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // test assertions
mod tests {
    use super::*;

    /// R19-F9: the registry is bounded — names normalize to
    /// [`MAX_CLIENT_NAME`] bytes (on a char boundary), and distinct
    /// names past [`MAX_CLIENTS`] fold into the shared
    /// [`OVERFLOW_CLIENT`] id instead of growing the map.
    #[test]
    fn registry_is_bounded_on_attacker_names() {
        let r = ClientRegistry::new();
        // Name length cap: a 200-byte name truncates; the truncated
        // twin resolves to the same id.
        let long = "x".repeat(200);
        let id = r.id(&long);
        assert_eq!(r.id(&long[..MAX_CLIENT_NAME]), id);
        // Multibyte truncation stays on a char boundary (€ is 3
        // bytes; byte 64 lands mid-char → normalizes to 63).
        let mb = "€".repeat(22); // 66 bytes
        let id_mb = r.id(&mb);
        assert_eq!(r.id(&"€".repeat(21)), id_mb);
        // Entry cap: fill to the cap, then every new name overflows.
        let r = ClientRegistry::new();
        for i in 0..MAX_CLIENTS {
            assert_eq!(r.id(&format!("c{i}")), i as u32);
        }
        assert_eq!(r.id("one-too-many"), OVERFLOW_CLIENT);
        assert_eq!(r.id("another"), OVERFLOW_CLIENT);
        assert_eq!(r.clients().len(), MAX_CLIENTS);
        // …while existing names still resolve to their real ids.
        assert_eq!(r.id("c0"), 0);
    }

    /// Regression — an `AdvanceTo` past [`csp_sim::MAX_TICKS_PER_FRAME`]
    /// must land on its target, not stall on the first `partial`:
    /// `drain_and_step` re-issues the report's *effective* target (the
    /// `AdvanceTo`-extended one), never the stale `clock + 1`.
    #[test]
    fn advance_to_past_frame_budget_lands() {
        let dir = std::env::temp_dir().join(format!("csp-backend-{}", std::process::id()));
        let game = LocalGame::new(World::new(), dir);
        let target = csp_sim::MAX_TICKS_PER_FRAME * 2 + 42;
        let report = game
            .submit(0, SimCommand::AdvanceTo(target))
            .expect("submit");
        assert!(report.rejected.is_empty(), "{:?}", report.rejected);
        assert!(!report.partial);
        assert_eq!(report.reached, target);
        game.with_world(|w| assert_eq!(w.clock().ticks(), target));
        assert_eq!(game.telemetry().expect("snapshot").tick, target);
        // A further warp from the new clock lands too — the host
        // chains partials, it never accumulates a stall.
        let target2 = target + csp_sim::MAX_TICKS_PER_FRAME + 7;
        game.submit(0, SimCommand::AdvanceTo(target2))
            .expect("submit");
        game.with_world(|w| assert_eq!(w.clock().ticks(), target2));
    }

    /// A batch refused mid-push must leave nothing queued — otherwise
    /// the pushed prefix silently drains under the next unrelated
    /// submit (R19-F5). Here a 3-command batch exceeds the per-client
    /// budget at the third push; a following `pump` must find the
    /// queue empty (nothing applied, nothing in the log).
    #[test]
    fn failed_batch_leaves_no_residue() {
        let dir = std::env::temp_dir().join(format!("csp-backend-{}", std::process::id()));
        let game = LocalGame::new(World::new(), dir).with_ingress(IngressQueue::with_limits(64, 2));
        let err = game
            .submit_batch(
                0,
                vec![
                    SimCommand::SetWarp(1),
                    SimCommand::SetWarp(1),
                    SimCommand::SetWarp(1),
                ],
            )
            .expect_err("budget refusal");
        assert!(matches!(
            err,
            BackendError::Ingress(IngressError::ClientBudget { .. })
        ));
        // The rolled-back prefix never drains: the pump steps an empty
        // boundary (no commands applied) and the log stays empty.
        let report = game.pump().expect("pump");
        assert_eq!(report.applied, 0);
        game.with_world(|w| assert!(w.command_log().is_empty()));
        // A clean submit still works — and applies only itself.
        let report = game.submit(0, SimCommand::SetWarp(9)).expect("submit");
        assert!(report.rejected.is_empty());
        game.with_world(|w| {
            let entries: Vec<_> = w.command_log().to_vec();
            assert_eq!(entries.len(), 1, "{entries:?}");
        });
    }

    /// The 64-partial-frame cap must be *reported*, not swallowed
    /// (R19-F15): an `AdvanceTo` the drain cannot finish comes back
    /// `partial: true` with the clock's real landing tick, so the
    /// caller knows to re-issue rather than assume arrival.
    #[test]
    fn drain_cap_reports_partial_shortfall() {
        let dir = std::env::temp_dir().join(format!("csp-backend-{}", std::process::id()));
        let game = LocalGame::new(World::new(), dir);
        // One past what 64 budget-split frames can reach.
        let target = 64 * csp_sim::MAX_TICKS_PER_FRAME + 1;
        let report = game
            .submit(0, SimCommand::AdvanceTo(target))
            .expect("submit");
        assert!(report.partial, "{report:?}");
        assert_eq!(report.target, target);
        assert!(report.reached < target);
        game.with_world(|w| assert_eq!(w.clock().ticks(), report.reached));
        // Re-issuing continues the same advance (the recorder
        // contract): the next drain's target resumes from `reached`
        // and the remaining tick lands it — no stall, no restart.
        let report = game
            .submit(0, SimCommand::AdvanceTo(target))
            .expect("submit");
        assert_eq!(report.target, target);
        assert!(!report.partial, "{report:?}");
        assert_eq!(report.reached, target);
    }
}
