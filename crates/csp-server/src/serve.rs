//! M9c §2.12.3 — `csp-server` runtime: bind, token file, shutdown.
//!
//! Defaults are the plan's: **bind `127.0.0.1` only** (a LAN bind is a
//! deliberate flag, `--bind-lan` in the app, printing a warning —
//! the config field here is `bind`, the policy lives app-side), a
//! fixed default port, and a token at
//! `$XDG_RUNTIME_DIR/csp/<pid>.token` (created mode 0600 atomically,
//! contents `<token>\n<port>\n`, removed on shutdown, rotated per
//! start, never in a save) that MCP clients read.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::backend::ControlBackend;
use crate::http::{self, AppState};

/// The fixed default port — `serve` refuses nothing else.
pub const DEFAULT_PORT: u16 = 47900;

/// The token's runtime-dir subdirectory (§2.12.3's lockfile/discovery
/// convention: `$XDG_RUNTIME_DIR/csp/`).
pub const RUNTIME_SUBDIR: &str = "csp";

/// Server construction config — a `Server` hands a copy to the
/// router for its guard state.
#[derive(Clone, Debug)]
pub struct ServerConfig {
    /// Bind address — `127.0.0.1:47900` by default; `0.0.0.0` is the
    /// app's deliberate `--bind-lan` choice, never the default.
    pub bind: SocketAddr,
    /// The bearer token — `None` generates one per process start (the
    /// rotation rule); tests pass `Some(_)` or `auth_disabled`.
    pub token: Option<String>,
    /// `true` = no token required (test and development only — the
    /// served binary never sets it).
    pub auth_disabled: bool,
    /// Directory for `<pid>.token` — `$XDG_RUNTIME_DIR/csp` or the
    /// platform temp dir fallback; `None` disables the file write.
    pub token_dir: Option<PathBuf>,
    /// M9d §2.12.2 — the sidecar dashboard directory
    /// (`*.json` per dashboard). Default [`config_dashboard_dir`];
    /// `None` disables the dashboard store (503 endpoints).
    pub dashboard_dir: Option<PathBuf>,
    /// M9d — the built telemetry SPA's static root (the
    /// `apps/telemetry` build output: `index.html` + `assets/`).
    /// `None` serves the placeholder page.
    pub spa_dir: Option<PathBuf>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), DEFAULT_PORT),
            token: None,
            auth_disabled: false,
            token_dir: Some(runtime_dir()),
            dashboard_dir: Some(config_dashboard_dir()),
            spa_dir: None,
        }
    }
}

/// `$XDG_RUNTIME_DIR/csp`, falling back to `<temp>/csp` — the token
/// dir a headless/test environment can still satisfy. The fallback is
/// a shared location, so the hardening lives in the writer: the dir is
/// enforced `0700` and the `<pid>.token` file is created `0600`
/// atomically with pre-existing paths refused.
pub fn runtime_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(RUNTIME_SUBDIR)
}

/// `$XDG_CONFIG_HOME/csp/dashboards` (→ `~/.config/csp/dashboards`
/// fallback, → temp last resort) — §2.12.2's "$config_dir/csp/
/// dashboards/*.json" sidecar location.
pub fn config_dashboard_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(std::env::temp_dir);
    base.join(RUNTIME_SUBDIR).join("dashboards")
}

/// A running server — the bound address (for ephemeral-port tests)
/// plus the token it enforces.
pub struct Server {
    /// The address `TcpListener` actually bound.
    pub addr: SocketAddr,
    /// The bearer token in force.
    pub token: String,
    /// The `<pid>.token` path written, when enabled.
    pub token_file: Option<PathBuf>,
    task: tokio::task::JoinHandle<Result<(), ServeError>>,
}

impl Server {
    /// Build the router state + axum app, bind, and spawn serving.
    /// The backend must be `Send + Sync` — a [`crate::backend::GameHost`]
    /// (axum shares state across worker threads).
    pub async fn start(
        config: ServerConfig,
        backend: Arc<dyn ControlBackend + Send + Sync>,
    ) -> Result<Self, ServeError> {
        let token = match &config.token {
            Some(t) => t.clone(),
            None => generate_token()?,
        };
        // M9d: one store serves both the `/api/dashboards` routes and
        // the session-class `dashboard.*` MCP tools.
        let dashboards = config
            .dashboard_dir
            .map(|d| Arc::new(crate::dashboard::DashboardStore::new(d)));
        let state = Arc::new(AppState {
            token: if config.auth_disabled {
                None
            } else {
                Some(token.clone())
            },
            backend: backend.clone(),
            registry: crate::backend::ClientRegistry::new(),
            mcp: crate::mcp::Mcp::with_dashboards(backend, dashboards.clone()),
            // The Origin/Host gate follows the socket: a LAN bind
            // accepts the addresses it actually listens on, a loopback
            // bind keeps the localhost set — never a hardcoded list.
            allowlist: http::BindAllowlist::for_bind(config.bind),
            dashboards,
            spa_dir: config.spa_dir.clone(),
            ws_live: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        });
        let app = http::router(state);
        let listener = tokio::net::TcpListener::bind(config.bind)
            .await
            .map_err(|e| ServeError::Bind(format!("{}: {e}", config.bind)))?;
        let addr = listener
            .local_addr()
            .map_err(|e| ServeError::Bind(e.to_string()))?;
        // The discovery file is written after bind: it carries the
        // bound port (an ephemeral `:0` only resolves here), and a
        // failed bind leaves no stale file behind.
        let token_file = match &config.token_dir {
            Some(dir) => Some(write_token_file(dir, &token, addr.port())?),
            None => None,
        };
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .map_err(|e| ServeError::Serve(e.to_string()))
        });
        Ok(Self {
            addr,
            token,
            token_file,
            task,
        })
    }

    /// Stop the server task and remove the `<pid>.token` discovery
    /// file — no stale token may outlive the server it unlocks
    /// (§2.12.3). `Drop` performs the cleanup, so this is the
    /// explicit-stop spelling of dropping.
    pub fn shutdown(self) {
        // `Drop` aborts the task and unlinks the token file.
    }
}

impl Drop for Server {
    /// Runs on `shutdown` *and* on a plain drop — a leaked `Server`
    /// cannot leave a stale `<pid>.token` naming a dead port.
    fn drop(&mut self) {
        self.task.abort();
        if let Some(path) = self.token_file.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// The shipped hosting path (report-19/20 F1): build a
/// `tokio::runtime`, start the server against `backend`, print the
/// attach coordinates (`addr` + the `<pid>.token` discovery file +
/// the SPA URL), and block until Ctrl-C or the serve task exits.
/// `csp --serve` is a thin wrapper over this — keeping the runtime
/// here means the app needs only `csp-server`, never `tokio`.
pub fn run_blocking(
    config: ServerConfig,
    backend: Arc<dyn ControlBackend + Send + Sync>,
) -> Result<(), ServeError> {
    let rt = tokio::runtime::Runtime::new()
        .map_err(|e| ServeError::Serve(format!("tokio runtime: {e}")))?;
    rt.block_on(async move {
        let mut server = Server::start(config, backend).await?;
        eprintln!(
            "csp-server: listening on http://{} (token file: {})",
            server.addr,
            server
                .token_file
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "disabled".to_string())
        );
        // Ctrl-C or the serve task ending — whichever lands first.
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            r = &mut server.task => {
                if let Err(e) = r {
                    return Err(ServeError::Serve(format!("serve task: {e}")));
                }
            }
        }
        Ok(())
    })
}

/// Serve-side failures.
#[derive(Debug)]
pub enum ServeError {
    /// `TcpListener::bind` failed.
    Bind(String),
    /// The CSPRNG could not mint a bearer token — there is no weak
    /// fallback: an unguessable token is the auth boundary, so
    /// entropy failure is a startup failure.
    Token(String),
    /// The token file could not be written.
    TokenFile(String),
    /// `axum::serve` returned.
    Serve(String),
}

impl std::fmt::Display for ServeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bind(m) => write!(f, "bind: {m}"),
            Self::Token(m) => write!(f, "token: {m}"),
            Self::TokenFile(m) => write!(f, "token file: {m}"),
            Self::Serve(m) => write!(f, "serve: {m}"),
        }
    }
}

impl std::error::Error for ServeError {}

/// The per-start token — 32 bytes from the OS CSPRNG rendered hex.
/// `getrandom` covers every native target (urandom, BCrypt, …), so
/// there is no platform fallback to a guessable pid+time hash — a
/// token that might be predictable is no token at all (R19-F20).
fn generate_token() -> Result<String, ServeError> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| ServeError::Token(e.to_string()))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// `<dir>/<pid>.token` holding `<token>\n<port>\n` — the discovery
/// file MCP clients read (§2.12.3). The file is created mode `0600`
/// atomically (`create_new`, never write-then-chmod): a pre-existing
/// path — a stale file from a killed run or an attacker-planted
/// symlink under the shared `<temp>/csp` fallback — is refused, never
/// followed and truncated.
fn write_token_file(dir: &Path, token: &str, port: u16) -> Result<PathBuf, ServeError> {
    use std::io::Write;
    ensure_token_dir(dir)?;
    let path = dir.join(format!("{}.token", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path).map_err(|e| {
        let hint = if e.kind() == std::io::ErrorKind::AlreadyExists {
            " (a stale or pre-planted token file — remove it and retry)"
        } else {
            ""
        };
        ServeError::TokenFile(format!("{e}{hint}"))
    })?;
    if let Err(e) = file.write_all(format!("{token}\n{port}\n").as_bytes()) {
        let _ = std::fs::remove_file(&path);
        return Err(ServeError::TokenFile(e.to_string()));
    }
    Ok(path)
}

/// Create the runtime dir if absent — `0700` on unix so the
/// `<temp>/csp` fallback is never world-browsable — and enforce the
/// same perms on a pre-existing dir we own. A symlink, a non-dir, or
/// a foreign-owned dir (the chmod fails) all fail closed: the token
/// must never land where another user can read or pre-plant it.
fn ensure_token_dir(dir: &Path) -> Result<(), ServeError> {
    match std::fs::symlink_metadata(dir) {
        Ok(meta) => {
            if meta.is_symlink() || !meta.is_dir() {
                return Err(ServeError::TokenFile(format!(
                    "token dir {} is not a plain directory",
                    dir.display()
                )));
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = std::fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder
                .create(dir)
                .map_err(|e| ServeError::TokenFile(e.to_string()))?;
        }
        Err(e) => return Err(ServeError::TokenFile(e.to_string())),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = std::fs::metadata(dir).map_err(|e| ServeError::TokenFile(e.to_string()))?;
        if meta.permissions().mode() & 0o777 != 0o700 {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| ServeError::TokenFile(format!("token dir perms: {e}")))?;
        }
    }
    Ok(())
}
