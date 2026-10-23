//! M9c §2.12.3 — `csp-server` runtime: bind, token file, shutdown.
//!
//! Defaults are the plan's: **bind `127.0.0.1` only** (a LAN bind is a
//! deliberate flag, `--bind-lan` in the app, printing a warning —
//! the config field here is `bind`, the policy lives app-side), a
//! fixed default port, and a token at
//! `$XDG_RUNTIME_DIR/csp/<pid>.token` (mode 0600, rotated per start,
//! never in a save) that MCP clients read.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
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
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), DEFAULT_PORT),
            token: None,
            auth_disabled: false,
            token_dir: Some(runtime_dir()),
        }
    }
}

/// `$XDG_RUNTIME_DIR/csp`, falling back to `<temp>/csp` — the token
/// dir a headless/test environment can still satisfy.
pub fn runtime_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(RUNTIME_SUBDIR)
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
        let token = config.token.clone().unwrap_or_else(generate_token);
        let token_file = match &config.token_dir {
            Some(dir) => Some(write_token_file(dir, &token)?),
            None => None,
        };
        let state = Arc::new(AppState {
            token: if config.auth_disabled {
                None
            } else {
                Some(token.clone())
            },
            backend: backend.clone(),
            registry: crate::backend::ClientRegistry::new(),
            mcp: crate::mcp::Mcp::new(backend),
        });
        let app = http::router(state);
        let listener = tokio::net::TcpListener::bind(config.bind)
            .await
            .map_err(|e| ServeError::Bind(format!("{}: {e}", config.bind)))?;
        let addr = listener
            .local_addr()
            .map_err(|e| ServeError::Bind(e.to_string()))?;
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

    /// Stop the server task.
    pub fn shutdown(self) {
        self.task.abort();
    }
}

/// Serve-side failures.
#[derive(Debug)]
pub enum ServeError {
    /// `TcpListener::bind` failed.
    Bind(String),
    /// The token file could not be written.
    TokenFile(String),
    /// `axum::serve` returned.
    Serve(String),
}

impl std::fmt::Display for ServeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bind(m) => write!(f, "bind: {m}"),
            Self::TokenFile(m) => write!(f, "token file: {m}"),
            Self::Serve(m) => write!(f, "serve: {m}"),
        }
    }
}

impl std::error::Error for ServeError {}

/// The per-start token — 32 bytes from `/dev/urandom` rendered hex
/// (a CSPRNG read, not a seeded stream); non-unix builds fall back to
/// a hash of pid + system time, which is honest for a localhost token.
fn generate_token() -> String {
    #[cfg(unix)]
    {
        if let Ok(bytes) = std::fs::read("/dev/urandom")
            && bytes.len() >= 32
        {
            return bytes[..32].iter().map(|b| format!("{b:02x}")).collect();
        }
    }
    // Fallback: pid + nanos + a stack address — weak but local-only.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut h = std::collections::hash_map::DefaultHasher::new();
    use std::hash::Hasher;
    h.write_u64(std::process::id() as u64);
    h.write_u128(nanos);
    h.write_usize(&nanos as *const _ as usize);
    format!("{:016x}{:016x}", h.finish(), nanos)
}

/// `<dir>/<pid>.token` mode 0600, contents `<token>\n<port>\n` — the
/// discovery file MCP clients read (§2.12.3).
fn write_token_file(dir: &PathBuf, token: &str) -> Result<PathBuf, ServeError> {
    std::fs::create_dir_all(dir).map_err(|e| ServeError::TokenFile(e.to_string()))?;
    let path = dir.join(format!("{}.token", std::process::id()));
    std::fs::write(&path, format!("{token}\n"))
        .map_err(|e| ServeError::TokenFile(e.to_string()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| ServeError::TokenFile(e.to_string()))?;
    }
    Ok(path)
}
