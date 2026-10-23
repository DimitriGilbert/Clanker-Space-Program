//! M9c §2.12.3 — the axum surface: MCP Streamable-HTTP, the WS
//! telemetry stream, and the static SPA shell.
//!
//! # Security (spec-mandated, not optional)
//!
//! - **`Origin`/`Host` allowlists on every endpoint, read-only ones
//!   included.** DNS-rebinding protection: a browser page from another
//!   origin — or any request asserting a foreign `Host` authority (the
//!   rebinding shape: `attacker.example` resolved to the bound
//!   address) — gets 403 before auth is even considered. The allowlist
//!   is derived from the bind ([`BindAllowlist`]): a loopback bind
//!   accepts the localhost names, a specific bind accepts its own
//!   address, a wildcard (`--bind-lan`) bind accepts any IP-literal
//!   authority plus the loopback names it covers. An absent
//!   `Origin`/`Host` also passes (non-browser clients do not send
//!   `Origin`; a missing `Host` falls back to the URI authority).
//! - **Bearer token on every endpoint except the SPA shell** — `/`
//!   and `/assets/*` are token-free so a browser can load the page,
//!   but the served shell never carries the secret. The token lives at
//!   `$XDG_RUNTIME_DIR/csp/<pid>.token` mode 0600, is rotated per
//!   process start, and is never stored in a save. It reaches clients
//!   only over authenticated channels — the file for MCP clients, an
//!   `Authorization: Bearer` header everywhere else, and `?token=`
//!   solely on the `/ws` upgrade, which cannot set headers.
//! - **Bounded ingress**: the body limit is the §2.2.1 64 KiB message
//!   cap; the WS frame cap matches; the command queue is
//!   [`crate::ingress::IngressQueue`]'s.
//! - **Bind `127.0.0.1`** by default (see [`crate::serve`]).
//!
//! # §2.12.2 deferrals (recorded here so the gap is explicit, not silent)
//!
//! - **MessagePack for arrays.** The plan's wire encoding is "JSON for
//!   scalars and control, MessagePack for arrays (per-part vectors)".
//!   The WS stream is currently JSON-only: the projected field set is
//!   scalar/small-object shaped and the per-part array fields that
//!   motivated MessagePack are not yet surfaced by any widget. Deferred
//!   until a subscription actually carries bulk arrays; `proto` in the
//!   envelope is the bump mechanism when it lands, and the
//!   `ws_subscription_stays_within_byte_budget` e2e is the tripwire
//!   that says when JSON has stopped fitting the 256 KB/s budget.
//! - **`SessionCommand` vs the sidecar store.** §2.12.2 says
//!   dashboards are "mutated via `SessionCommand`" — the §2.2
//!   category-C session channel. That channel is an app-side seam and
//!   does not exist in this crate; what csp-server has is
//!   *session-class tools*. The `dashboard.*` MCP tools and
//!   `/api/dashboards` write the [`crate::dashboard::DashboardStore`]
//!   directly, which preserves the invariant the plan actually cares
//!   about — sidecar config is never in the save, the command log, or
//!   the state hash — so the tools stay session-class and
//!   replay-irrelevant with or without a future SessionCommand
//!   channel behind them.
//! - **`spa_dir` has no binary wiring yet.** [`AppState::spa_dir`] is
//!   `None` in every shipped path (no binary sets it), so `/` serves
//!   the placeholder in production today. The verbatim `/` +
//!   `/assets/*` path is exercised by the `spa_serving_*` e2e, which
//!   boots the router against a temp `spa_dir`.

use std::convert::Infallible;
use std::net::{IpAddr, SocketAddr};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{DefaultBodyLimit, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use csp_sim::SimCommand;
use futures_util::{SinkExt, StreamExt, stream};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::backend::{ClientRegistry, ControlBackend};
use crate::dashboard::{Dashboard, DashboardStore};
use crate::ingress::MAX_MESSAGE_BYTES;
use crate::mcp::{Mcp, SUPPORTED_REVISIONS};

/// The subscription rate divisors — §2.12.2: "divisors of 64 Hz only",
/// default 16 Hz, never tied to the tick rate.
const VALID_RATES: &[u64] = &[64, 32, 16, 8, 4, 2, 1];
/// Cap on a client's declared field list — §2.12.2's "no send me
/// everything": a bound, not a policy.
const MAX_FIELDS: usize = 64;
/// The wire-envelope protocol version — `proto` bumps on any
/// breaking change (§2.12.2).
const WIRE_PROTO: u64 = 1;

/// Bound on a backend call riding a `spawn_blocking` thread: the
/// synchronous [`ControlBackend`] contract waits on a game-thread
/// rendezvous — a hung or wedged game thread gets a 503 here instead
/// of an in-flight request (or a parked worker) that outlives its
/// client. Generous enough for a legitimate heavy drain.
const BACKEND_CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// Live `/ws` connection bound — a telemetry stream costs a task and
/// a per-tick projection, so the count is a resource limit like the
/// body/frame caps (R19-F18). The cap+1th upgrade gets a 503.
const MAX_WS_CONNECTIONS: usize = 16;

/// `/ws` keepalive cadence — a protocol-level Ping per interval;
/// the peer's Pong refreshes `last_seen`.
const WS_PING_INTERVAL: Duration = Duration::from_secs(30);

/// `/ws` liveness bound — three missed ping cycles with *no* inbound
/// frame at all closes the session (a dead or deaf peer must not
/// leak a task + a connection slot forever).
const WS_IDLE_TIMEOUT: Duration = Duration::from_secs(90);

/// Bound on one outbound send — the drop-to-latest contract means a
/// send never queues; a client that stops reading fills the socket
/// buffer and would stall `Sink::send` (and the whole session loop)
/// without it.
const WS_SEND_TIMEOUT: Duration = Duration::from_secs(10);

/// The shared app state behind every route.
pub struct AppState {
    /// The bearer token — `None` only for the test-side router
    /// (`serve` always generates or takes one).
    pub token: Option<String>,
    /// The host game — `Send + Sync` because axum shares it across
    /// worker threads: a [`crate::backend::GameHost`].
    pub backend: Arc<dyn ControlBackend + Send + Sync>,
    /// §2.12.3's client-index allocator.
    pub registry: ClientRegistry,
    /// The transport-free MCP engine.
    pub mcp: Mcp,
    /// The `Origin`/`Host` allowlist the guard enforces — derived
    /// from the bound address ([`BindAllowlist::for_bind`]), never a
    /// hardcoded localhost set.
    pub allowlist: BindAllowlist,
    /// M9d §2.12.2 — the sidecar dashboard store (`/api/dashboards` +
    /// the `dashboard.*` session tools). `None` = unconfigured (503s,
    /// not a fake empty list).
    pub dashboards: Option<Arc<DashboardStore>>,
    /// M9d — the built SPA's static root (`apps/telemetry` output);
    /// `/` serves its `index.html` verbatim (the shell is token-free
    /// and must never carry the secret), `/assets/*` serves the
    /// bundle. `None` = the placeholder page.
    pub spa_dir: Option<PathBuf>,
    /// Live `/ws` sessions — counted so the connection bound is a
    /// real limit, not a wish (see `ws_permit`).
    pub ws_live: Arc<AtomicUsize>,
}

/// A held `/ws` connection slot — decrements the live count on drop,
/// i.e. exactly when the session task ends (any exit path).
pub struct WsPermit {
    live: Arc<AtomicUsize>,
}

impl Drop for WsPermit {
    fn drop(&mut self) {
        self.live.fetch_sub(1, Ordering::Relaxed);
    }
}

impl AppState {
    /// Wire the state's parts from a backend + token + the address
    /// the socket binds (the allowlist follows it).
    pub fn new(
        backend: Arc<dyn ControlBackend + Send + Sync>,
        token: Option<String>,
        bind: SocketAddr,
    ) -> Self {
        Self {
            token,
            mcp: Mcp::new(backend.clone()),
            backend,
            registry: ClientRegistry::new(),
            allowlist: BindAllowlist::for_bind(bind),
            dashboards: None,
            spa_dir: None,
            ws_live: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Take a `/ws` connection slot — `None` once
    /// [`MAX_WS_CONNECTIONS`] sessions are live; the returned permit
    /// holds the slot until the session ends.
    fn ws_permit(&self) -> Option<WsPermit> {
        if self.ws_live.fetch_add(1, Ordering::Relaxed) >= MAX_WS_CONNECTIONS {
            self.ws_live.fetch_sub(1, Ordering::Relaxed);
            return None;
        }
        Some(WsPermit {
            live: self.ws_live.clone(),
        })
    }
}

/// The route table — every handler sits behind [`guard`].
pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/api/telemetry", get(api_telemetry))
        .route("/api/dashboards", get(dashboard_list))
        .route(
            "/api/dashboards/{name}",
            get(dashboard_get).put(dashboard_put),
        )
        .route("/assets/{*path}", get(spa_asset))
        .route("/mcp", post(mcp_post).get(mcp_reject).delete(mcp_reject))
        .route("/ws", get(ws_upgrade))
        .layer(DefaultBodyLimit::max(MAX_MESSAGE_BYTES))
        .layer(middleware::from_fn_with_state(state.clone(), guard))
        .with_state(state)
}

// ---------------------------------------------------------------------------
// Guard — Origin allowlist + bearer token (all endpoints)
// ---------------------------------------------------------------------------

/// The `Origin`/`Host` allowlist, derived from the bound address —
/// §2.12.3's DNS-rebinding gate follows where the socket actually
/// listens, never a hardcoded localhost set (a LAN bind with a
/// localhost-only list would fail closed for the intended users and
/// open for `Host: localhost` spoofers).
#[derive(Clone, Debug)]
pub enum BindAllowlist {
    /// A loopback bind: the localhost authority set — `localhost`,
    /// `127.0.0.0/8`, `[::1]` on any port.
    Loopback,
    /// A specific non-loopback bind (e.g. `192.168.1.5`): the bound
    /// address alone — every connection arrives on it, and no other
    /// authority (a name, a different address, `localhost` itself)
    /// can be verified.
    Bound(IpAddr),
    /// A wildcard bind (`0.0.0.0`/`::` — the app's deliberate
    /// `--bind-lan`): every local address is bound, so any IP-literal
    /// authority that reached us names a bound address, and the
    /// localhost set rides the loopback the wildcard covers. Other
    /// *names* stay denied — a DNS name for this host cannot be
    /// verified, so LAN clients must address us literally.
    Wildcard,
}

impl BindAllowlist {
    /// The allowlist for a bound socket address.
    pub fn for_bind(bind: SocketAddr) -> Self {
        let ip = bind.ip();
        if ip.is_unspecified() {
            Self::Wildcard
        } else if ip.is_loopback() {
            Self::Loopback
        } else {
            Self::Bound(ip)
        }
    }

    /// `true` when `host` — the host part of a `Host`/`Origin`
    /// authority — names a bound address under this allowlist.
    fn name_allowed(&self, host: &str) -> bool {
        match self {
            Self::Loopback => {
                localhost_name(host) || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
            }
            Self::Bound(ip) => host.parse::<IpAddr>().is_ok_and(|h| h == *ip),
            Self::Wildcard => host.parse::<IpAddr>().is_ok() || localhost_name(host),
        }
    }

    /// `true` when `Origin`'s authority names a bound address — the
    /// spec's DNS-rebinding rule. We compare the *authority*, not the
    /// scheme string, so `http://localhost:PORT` and
    /// `https://127.0.0.1:PORT` both pass on a loopback bind and
    /// `http://evil.example` never does anywhere.
    fn origin_allowed(&self, origin: &str) -> bool {
        // Strip the scheme, then the path — keep the authority.
        let rest = origin
            .split("://")
            .nth(1)
            .unwrap_or(origin)
            .split('/')
            .next()
            .unwrap_or_default();
        self.name_allowed(authority_host(rest))
    }

    /// `true` when a `Host` (or URI) authority names a bound
    /// address — the other half of the DNS-rebinding rule. An
    /// unbracketed IPv6 literal (`::1` bare) is malformed for an
    /// authority and denies.
    fn host_allowed(&self, host: &str) -> bool {
        self.name_allowed(authority_host(host.trim()))
    }
}

/// The `localhost` names — the loopback-bind authority set (and the
/// name half a wildcard bind keeps for the loopback it covers).
fn localhost_name(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "::1"
}

/// The host part of an `authority` — `user@host:port`, `[v6]:port`,
/// or a bare host. Userinfo cannot smuggle (`rsplit('@')`), and an
/// IPv6 literal's `]` terminates it before the port split.
fn authority_host(authority: &str) -> &str {
    let host = authority.rsplit('@').next().unwrap_or(authority);
    match host.strip_prefix('[').and_then(|h| h.split(']').next()) {
        Some(inner) => inner,
        None => host.split(':').next().unwrap_or(host),
    }
}

/// The guard: the §2.12.3 Origin/Host allowlist first (403 on a
/// foreign authority — the DNS-rebinding shape), then the bearer
/// token on everything except the token-free SPA shell. `?token=`
/// authenticates the `/ws` upgrade path only.
async fn guard(
    State(state): State<Arc<AppState>>,
    req: axum::http::Request<Body>,
    next: Next,
) -> Response {
    if let Some(origin) = req.headers().get(header::ORIGIN)
        && let Ok(s) = origin.to_str()
        && !state.allowlist.origin_allowed(s)
    {
        return (StatusCode::FORBIDDEN, "origin not allowed").into_response();
    }
    // `Host` gets the same allowlist — asserted on every endpoint,
    // `/` included. Absent (HTTP/1.0, or h2's `:authority` riding the
    // URI) the URI's own authority is checked instead; neither
    // present means nothing foreign was claimed and the bound
    // address stands.
    if let Some(host) = req
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .or_else(|| req.uri().host())
        && !state.allowlist.host_allowed(host)
    {
        return (StatusCode::FORBIDDEN, "host not allowed").into_response();
    }
    // The SPA shell + its static bundle are token-free — a browser
    // cannot set Authorization on a <script>/<link> fetch, and the
    // shell never carries the secret (the token is never served; it
    // arrives over the authenticated channel). The Origin/Host
    // allowlist above still covers them (every endpoint gets it); the
    // token's job is protecting the *data* surfaces, and those stay
    // guarded.
    let path = req.uri().path();
    if path == "/" || path.starts_with("/assets/") {
        return next.run(req).await;
    }
    if let Some(token) = &state.token {
        // `?token=` exists for the WS bootstrap only — a browser's
        // upgrade request cannot set headers — never for the REST/MCP
        // surfaces, where a bearer-carrying query string would leak
        // into logs and history.
        let query_ok = path == "/ws"
            && Query::<TokenQuery>::try_from_uri(req.uri())
                .ok()
                .is_some_and(|q| {
                    q.token
                        .as_deref()
                        .is_some_and(|t| constant_time_eq(t, token))
                });
        if !bearer_matches(req.headers(), token) && !query_ok {
            return (StatusCode::UNAUTHORIZED, "bad or missing bearer token").into_response();
        }
    }
    next.run(req).await
}

#[derive(Deserialize)]
struct TokenQuery {
    token: Option<String>,
}

/// `Authorization: Bearer <token>` — the auth scheme is
/// case-insensitive (RFC 9110 §11.4); the credential compare never
/// short-circuits on a matching prefix.
fn bearer_matches(headers: &HeaderMap, token: &str) -> bool {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            let (scheme, credential) = v.split_once(' ')?;
            scheme.eq_ignore_ascii_case("bearer").then_some(credential)
        })
        .is_some_and(|t| constant_time_eq(t, token))
}

/// Constant-time string equality: every byte pair is folded into one
/// accumulator (the length difference included), so the running time
/// cannot reveal where a guess diverged.
fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut diff = a.len() ^ b.len();
    for i in 0..a.len().max(b.len()) {
        diff |= usize::from(a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0));
    }
    diff == 0
}

// ---------------------------------------------------------------------------
// `/` + `/api/telemetry` — the SPA shell and the debug read
// ---------------------------------------------------------------------------

/// The SPA shell — M9d: serve the built app's `index.html` from
/// `spa_dir` **verbatim** (§2.12.3 — `/` is the token-free page, so
/// the secret must never appear in what it serves; the token reaches
/// the client only over the authenticated channel). Without a
/// configured `spa_dir`, the token-free placeholder remains.
async fn index(State(state): State<Arc<AppState>>) -> Response {
    if let Some(dir) = &state.spa_dir
        && let Ok(html) = tokio::fs::read_to_string(dir.join("index.html")).await
    {
        return Html(html).into_response();
    }
    Html(format!(
        "<!doctype html><html><head><meta charset=\"utf-8\">\
         <title>Clanker Space Program — telemetry</title></head>\
         <body><h1>csp-server</h1><p>Build the telemetry SPA in \
         <code>apps/telemetry</code> and point <code>spa_dir</code> at \
         its output. The WS endpoint is <code>/ws</code>; MCP is \
         <code>POST /mcp</code> (protocol revision {}).<p></body></html>",
        crate::mcp::PROTOCOL_REVISION
    ))
    .into_response()
}

/// The `/assets/{*path}` gate (R19-F13): `path` must be a plain
/// relative path of `Normal` segments — `\` is refused outright (a
/// separator on Windows, where `a\..\b` would traverse even though a
/// `/`-split sees one safe segment), NUL likewise (OS paths truncate
/// at it), `:` too (Windows drive prefixes and ADS forks survive the
/// `Normal` walk on Unix), and a `Component` walk rejects every
/// `.`/`..`/root/Windows-prefix piece; empty `/`-segments (`a//b`)
/// are refused too.
fn clean_asset_path(path: &str) -> bool {
    !path.is_empty()
        && !path.contains(['\\', '\0', ':'])
        && !path.split('/').any(str::is_empty)
        && Path::new(path)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
}

/// `GET /assets/{*path}` — the SPA bundle's static files. Bounded:
/// the path must be relative-clean (see [`clean_asset_path`]), the
/// file under `spa_dir`, and small (bundle files, not arbitrary fs).
async fn spa_asset(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(path): axum::extract::Path<String>,
) -> Response {
    const MAX_ASSET: u64 = 16 * 1024 * 1024;
    let Some(dir) = &state.spa_dir else {
        return (StatusCode::NOT_FOUND, "no SPA configured").into_response();
    };
    if !clean_asset_path(&path) {
        return (StatusCode::BAD_REQUEST, "bad asset path").into_response();
    }
    let file = dir.join("assets").join(&path);
    // async fs — a blocking `std::fs` call on a runtime worker would
    // stall sibling handlers under a slow disk (R19-F16).
    match tokio::fs::metadata(&file).await {
        Ok(m) if m.is_file() && m.len() <= MAX_ASSET => {}
        _ => return (StatusCode::NOT_FOUND, "asset not found").into_response(),
    }
    let bytes = match tokio::fs::read(&file).await {
        Ok(b) => b,
        Err(_) => return (StatusCode::NOT_FOUND, "asset not found").into_response(),
    };
    let ty = match file.extension().and_then(|e| e.to_str()) {
        Some("js" | "mjs") => "text/javascript",
        Some("css") => "text/css",
        Some("html") => "text/html",
        Some("json" | "map") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("woff" | "woff2") => "font/woff2",
        _ => "application/octet-stream",
    };
    ([(header::CONTENT_TYPE, ty)], bytes).into_response()
}

/// Run one synchronous [`DashboardStore`] op on the blocking pool —
/// the store's fs I/O must not park a runtime worker (R19-F16); the
/// `JoinError` arm is a panicked task, surfaced as a 500.
async fn store_call<R: Send + 'static>(
    state: &Arc<AppState>,
    op: impl FnOnce(&DashboardStore) -> R + Send + 'static,
) -> Result<R, Response> {
    let Some(store) = state.dashboards.clone() else {
        return Err((StatusCode::SERVICE_UNAVAILABLE, "dashboards not configured").into_response());
    };
    tokio::task::spawn_blocking(move || op(&store))
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("dashboard task failed: {e}"),
            )
                .into_response()
        })
}

/// `GET /api/dashboards` — the store's name list.
async fn dashboard_list(State(state): State<Arc<AppState>>) -> Response {
    match store_call(&state, |s| s.list()).await {
        Err(resp) => resp,
        Ok(Ok(names)) => Json(json!({"dashboards": names})).into_response(),
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// `GET /api/dashboards/{name}` — one stored dashboard document.
async fn dashboard_get(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> Response {
    match store_call(&state, move |s| s.load(&name)).await {
        Err(resp) => resp,
        Ok(Ok(dash)) => Json(dash).into_response(),
        Ok(Err(crate::dashboard::DashboardError::Invalid(m))) => {
            (StatusCode::NOT_FOUND, m).into_response()
        }
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// `PUT /api/dashboards/{name}` — validate + store the document.
/// Body cap is the shared 64 KiB message limit (a dashboard is a
/// bounded config doc, not a blob).
async fn dashboard_put(
    State(state): State<Arc<AppState>>,
    axum::extract::Path(name): axum::extract::Path<String>,
    body: Bytes,
) -> Response {
    // The 503 precedes the body parse — an unconfigured store says
    // "not configured", never a JSON complaint about a body it
    // would not have stored anyway.
    if state.dashboards.is_none() {
        return (StatusCode::SERVICE_UNAVAILABLE, "dashboards not configured").into_response();
    }
    let dash: Dashboard = match serde_json::from_slice(&body) {
        Ok(d) => d,
        Err(e) => return (StatusCode::BAD_REQUEST, format!("dashboard json: {e}")).into_response(),
    };
    match store_call(&state, move |s| s.save(&name, &dash).map(|()| name)).await {
        Err(resp) => resp,
        Ok(Ok(saved)) => Json(json!({"name": saved})).into_response(),
        Ok(Err(crate::dashboard::DashboardError::Invalid(m))) => {
            (StatusCode::BAD_REQUEST, m).into_response()
        }
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// `GET /api/telemetry` — the current snapshot, verbatim JSON. A
/// debug/read surface (the WS subscription path is the real stream).
async fn api_telemetry(State(state): State<Arc<AppState>>) -> Response {
    match state.backend.telemetry() {
        Some(snap) => Json(&*snap).into_response(),
        None => (StatusCode::SERVICE_UNAVAILABLE, "no snapshot published").into_response(),
    }
}

// ---------------------------------------------------------------------------
// `/mcp` — Streamable HTTP (revision 2026-07-28)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct McpQuery {
    /// `X-CSP-Client`'s query-form twin — names the client for the
    /// `source_id` allocator.
    client: Option<String>,
}

/// POST the one endpoint: a single JSON-RPC message in; a JSON
/// response — or a per-request `text/event-stream` upgrade when the
/// client accepts it. `202 Accepted` when the message was all
/// notifications (no response bodies exist — the spec's rule).
async fn mcp_post(
    State(state): State<Arc<AppState>>,
    Query(q): Query<McpQuery>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    // `MCP-Protocol-Version`, when sent, must be a revision we speak —
    // post-initialize clients send it on every request.
    if let Some(v) = headers
        .get("mcp-protocol-version")
        .and_then(|v| v.to_str().ok())
        && !SUPPORTED_REVISIONS.contains(&v)
    {
        return (
            StatusCode::BAD_REQUEST,
            format!("unsupported MCP-Protocol-Version {v}"),
        )
            .into_response();
    }
    let msg: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("invalid JSON-RPC message: {e}"),
            )
                .into_response();
        }
    };
    if !msg.is_object() {
        // 2026-07-28's Streamable HTTP carries one message per POST —
        // JSON-RPC batching was removed in revision 2025-03-26.
        return (StatusCode::BAD_REQUEST, "one JSON-RPC message per POST").into_response();
    }
    let client_name = q
        .client
        .clone()
        .or_else(|| {
            headers
                .get("x-csp-client")
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
        })
        .unwrap_or_else(|| "default".to_string());
    let client = state.registry.id(&client_name);
    // The backend behind `mcp.handle` is synchronous (its `call`
    // waits on a game-thread rendezvous) — run it on a blocking
    // thread with a bound so a slow drain parks the blocking pool,
    // never a runtime worker, and a wedged thread answers 503.
    let mcp = state.mcp.clone();
    let handled = tokio::time::timeout(
        BACKEND_CALL_TIMEOUT,
        tokio::task::spawn_blocking(move || mcp.handle(client, &msg)),
    )
    .await;
    let handled = match handled {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("mcp task failed: {e}"),
            )
                .into_response();
        }
        Err(_) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "game thread did not answer in time",
            )
                .into_response();
        }
    };
    let Some(response) = handled else {
        return StatusCode::ACCEPTED.into_response();
    };
    // Per-request SSE upgrade — the plan's transport shape: when the
    // client accepts `text/event-stream`, the response rides an SSE
    // stream of `event: message` frames; otherwise plain JSON.
    let accepts_sse = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|a| a.contains("text/event-stream"));
    if accepts_sse {
        let event = Event::default().event("message").data(response.to_string());
        let s = stream::iter(vec![Ok::<Event, Infallible>(event)]);
        Sse::new(s).into_response()
    } else {
        Json(response).into_response()
    }
}

/// `GET`/`DELETE` on `/mcp`: revision 2026-07-28 removed the GET
/// listen stream and protocol-level sessions — both are 405.
async fn mcp_reject() -> (StatusCode, &'static str) {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        "POST only — Streamable HTTP at revision 2026-07-28",
    )
}

// ---------------------------------------------------------------------------
// `/ws` — the §2.12.2 subscription stream
// ---------------------------------------------------------------------------

/// The WS subscribe envelope: `{proto, kind: "subscribe", fields,
/// rate_hz?}`; afterwards `{kind: "command", command}` is the opt-in
/// command channel (§2.12's "mostly read; opt-in command channel").
/// Bounded: [`MAX_WS_CONNECTIONS`] slots, and the frame cap matches
/// the message cap — the §2.2.1 64 KiB wire bound applies to single
/// frames too, not just reassembled messages (R19-F10).
async fn ws_upgrade(
    State(state): State<Arc<AppState>>,
    Query(q): Query<McpQuery>,
    ws: WebSocketUpgrade,
) -> Response {
    let Some(permit) = state.ws_permit() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "too many websocket connections",
        )
            .into_response();
    };
    let client = state.registry.id(q.client.as_deref().unwrap_or("default"));
    ws.max_frame_size(MAX_MESSAGE_BYTES)
        .max_message_size(MAX_MESSAGE_BYTES)
        .on_upgrade(move |socket| ws_session(socket, state, client, permit))
}

/// One WS session: subscribe → project fields per tick of the rate
/// interval → send `{proto, kind: "telemetry", seq, tick, payload}`.
/// Backpressure is drop-to-latest by construction — each send reads
/// the `ArcSwap` anew; a slow client degrades its own rate only.
/// Keepalive is a protocol Ping per [`WS_PING_INTERVAL`]; any inbound
/// frame (Pongs included) refreshes liveness, and
/// [`WS_IDLE_TIMEOUT`] without one closes the session. Sends are
/// bounded by [`WS_SEND_TIMEOUT`] — a peer that stops reading is
/// dropped, never queued (R19-F18). `_permit` holds the connection
/// slot for the session's life.
// The select! loop holds the whole session state machine — splitting
// the arms would scatter the shared `fields`/`ticker`/`seq` state.
#[allow(clippy::too_many_lines, clippy::cognitive_complexity)]
async fn ws_session(socket: WebSocket, state: Arc<AppState>, client: u32, _permit: WsPermit) {
    let (mut sink, mut stream) = socket.split();
    let mut fields: Option<Vec<String>> = None;
    let mut rate_hz = 16_u64;
    // The telemetry frame counter. `command_ack`s carry their own —
    // sharing it would tie a request/response sequence to the
    // stream's cadence (R19-F14).
    let mut seq = 0_u64;
    let mut cmd_seq = 0_u64;
    let mut ticker = tokio::time::interval(std::time::Duration::from_millis(1000 / rate_hz));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut keepalive = tokio::time::interval_at(
        tokio::time::Instant::now() + WS_PING_INTERVAL,
        WS_PING_INTERVAL,
    );
    let mut last_seen = tokio::time::Instant::now();
    loop {
        tokio::select! {
            // The subscription gate keeps an unsubscribed session
            // from waking 16×/s to do nothing.
            _ = ticker.tick(), if fields.is_some() => {
                let Some(fs) = fields.as_ref() else { continue };
                let Some(snap) = state.backend.telemetry() else { continue };
                let (payload, unknown) = snap.project(fs);
                let frame = json!({
                    "proto": WIRE_PROTO, "kind": "telemetry",
                    "seq": seq, "tick": snap.tick,
                    "payload": payload, "unknown_fields": unknown,
                });
                seq += 1;
                if !send_msg(&mut sink, Message::Text(frame.to_string().into())).await {
                    return;
                }
            }
            _ = keepalive.tick() => {
                if last_seen.elapsed() > WS_IDLE_TIMEOUT {
                    // Missed-pong cycles: the peer is dead or deaf —
                    // best-effort close, then reclaim the slot.
                    let _ = send_msg(&mut sink, Message::Close(None)).await;
                    return;
                }
                if !send_msg(&mut sink, Message::Ping(Bytes::new())).await {
                    return;
                }
            }
            msg = stream.next() => {
                let Some(Ok(msg)) = msg else { return };
                // Any inbound frame — Pongs to our pings included —
                // is liveness proof.
                last_seen = tokio::time::Instant::now();
                let Message::Text(text) = msg else { continue };
                let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
                // Every client→server envelope carries `proto` — the
                // version field is the bump mechanism for the whole
                // wire surface, not just subscriptions (R19-F14).
                if matches!(
                    v.get("kind").and_then(Value::as_str),
                    Some("subscribe" | "command")
                ) && v.get("proto") != Some(&json!(WIRE_PROTO))
                {
                    if !send_json(&mut sink, &json!({
                        "proto": WIRE_PROTO, "kind": "error",
                        "error": format!("wire proto must be {WIRE_PROTO}"),
                    })).await {
                        return;
                    }
                    continue;
                }
                match v.get("kind").and_then(Value::as_str) {
                    Some("subscribe") => {
                        let fs: Vec<String> = v
                            .get("fields")
                            .and_then(Value::as_array)
                            .map(|a| {
                                a.iter()
                                    .filter_map(|f| f.as_str().map(str::to_string))
                                    .collect()
                            })
                            .unwrap_or_default();
                        if fs.is_empty() {
                            if !send_json(&mut sink, &json!({
                                "proto": WIRE_PROTO, "kind": "error",
                                "error": "subscription needs a non-empty fields list",
                            })).await {
                                return;
                            }
                            continue;
                        }
                        // Over-cap is an explicit error, never a
                        // silent truncation — paths past the cap
                        // would vanish without even an
                        // `unknown_fields` mention (R19-F14).
                        if fs.len() > MAX_FIELDS {
                            if !send_json(&mut sink, &json!({
                                "proto": WIRE_PROTO, "kind": "error",
                                "error": format!(
                                    "fields list {} exceeds the {MAX_FIELDS}-field cap",
                                    fs.len()
                                ),
                            })).await {
                                return;
                            }
                            continue;
                        }
                        let rate = v.get("rate_hz").and_then(Value::as_u64).unwrap_or(16);
                        if !VALID_RATES.contains(&rate) {
                            if !send_json(&mut sink, &json!({
                                "proto": WIRE_PROTO, "kind": "error",
                                "error": format!("rate_hz must divide 64: {VALID_RATES:?}"),
                            })).await {
                                return;
                            }
                            continue;
                        }
                        rate_hz = rate;
                        fields = Some(fs);
                        ticker = tokio::time::interval(
                            std::time::Duration::from_millis(1000 / rate_hz),
                        );
                        ticker.set_missed_tick_behavior(
                            tokio::time::MissedTickBehavior::Skip,
                        );
                        if !send_json(&mut sink, &json!({
                            "proto": WIRE_PROTO, "kind": "subscribed",
                            "rate_hz": rate_hz,
                        })).await {
                            return;
                        }
                    }
                    Some("command") => {
                        let cmd = v.get("command").cloned().unwrap_or(Value::Null);
                        // Same spawn_blocking + bound as `/mcp` —
                        // `submit` waits on the game thread.
                        let backend = state.backend.clone();
                        let result = tokio::time::timeout(
                            BACKEND_CALL_TIMEOUT,
                            tokio::task::spawn_blocking(move || {
                                serde_json::from_value::<SimCommand>(cmd)
                                    .map_err(|e| e.to_string())
                                    .and_then(|c| {
                                        backend.submit(client, c).map_err(|e| e.to_string())
                                    })
                            }),
                        )
                        .await;
                        let result = match result {
                            Ok(Ok(r)) => r,
                            Ok(Err(e)) => Err(format!("command task failed: {e}")),
                            Err(_) => Err("game thread did not answer in time".to_string()),
                        };
                        let frame = match result {
                            Ok(report) => json!({
                                "proto": WIRE_PROTO, "kind": "command_ack",
                                "seq": cmd_seq, "rejected": report.rejected,
                                "partial": report.partial,
                                "reached": report.reached,
                                "target": report.target,
                            }),
                            Err(e) => json!({
                                "proto": WIRE_PROTO, "kind": "command_ack",
                                "seq": cmd_seq, "error": e,
                            }),
                        };
                        cmd_seq += 1;
                        if !send_json(&mut sink, &frame).await {
                            return;
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

/// A bounded send — `false` means the peer or the deadline lost, so
/// the caller ends the session. The timeout exists because
/// drop-to-latest forbids queueing: a stalled `Sink::send` on a
/// full socket buffer must close the session, not park it
/// (`WS_SEND_TIMEOUT`, R19-F18).
async fn send_msg(
    sink: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    msg: Message,
) -> bool {
    matches!(
        tokio::time::timeout(WS_SEND_TIMEOUT, sink.send(msg)).await,
        Ok(Ok(()))
    )
}

async fn send_json(
    sink: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    v: &Value,
) -> bool {
    send_msg(sink, Message::Text(v.to_string().into())).await
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // test assertions
mod tests {
    use super::*;

    fn loopback() -> BindAllowlist {
        BindAllowlist::for_bind("127.0.0.1:47900".parse().unwrap())
    }

    #[test]
    fn origin_allowlist() {
        let a = loopback();
        for ok in [
            "http://localhost:47900",
            "https://127.0.0.1",
            "http://[::1]:8080/path?q=1",
            "http://LOCALHOST",
        ] {
            assert!(a.origin_allowed(ok), "{ok}");
        }
        for bad in [
            "http://evil.example",
            "http://localhost.evil.example",
            "http://127.0.0.1.evil.example",
            "javascript:alert(1)",
            "",
        ] {
            assert!(!a.origin_allowed(bad), "{bad}");
        }
    }

    #[test]
    fn host_allowlist() {
        let a = loopback();
        for ok in [
            "localhost",
            "localhost:47900",
            "127.0.0.1",
            "127.0.0.1:8080",
            "127.0.0.2", // any loopback address, not just .1
            "[::1]",
            "[::1]:9000",
            "LOCALHOST:80",
        ] {
            assert!(a.host_allowed(ok), "{ok}");
        }
        for bad in [
            "evil.example",
            "localhost.evil.example",
            "127.0.0.1.evil.example",
            "user@evil.example:80",
            "::1",         // unbracketed v6 is a malformed authority
            "192.168.1.5", // a LAN address is foreign to a loopback bind
            "",
        ] {
            assert!(!a.host_allowed(bad), "{bad}");
        }
    }

    /// R19-F3: the allowlist follows the bind — a `--bind-lan`
    /// (wildcard) bind accepts the IP-literal authorities LAN
    /// clients actually send, keeps the loopback names the wildcard
    /// covers, and still refuses unverifiable names (the
    /// DNS-rebinding shape). A specific non-loopback bind accepts
    /// its own address only — `localhost` included in the refused set.
    #[test]
    fn allowlist_follows_the_bind() {
        // Wildcard bind (the --bind-lan shape).
        let a = BindAllowlist::for_bind("0.0.0.0:47900".parse().unwrap());
        for ok in [
            "192.168.1.5:47900", // a LAN client addressing us literally
            "192.168.1.5",
            "10.0.0.2",
            "[fe80::1]:47900",
            "localhost:47900", // loopback is covered by the wildcard
            "127.0.0.1",
            "[::1]",
        ] {
            assert!(a.host_allowed(ok), "{ok}");
        }
        for bad in [
            "evil.example", // the rebinding shape — still denied
            "desktop.lan",  // unverifiable name — denied, use the literal IP
            "localhost.evil.example",
            "user@evil.example:80",
            "",
        ] {
            assert!(!a.host_allowed(bad), "{bad}");
        }
        assert!(a.origin_allowed("http://192.168.1.5:47900"));
        assert!(!a.origin_allowed("http://evil.example"));

        // A specific non-loopback bind — only the bound address.
        let a = BindAllowlist::for_bind("192.168.1.5:47900".parse().unwrap());
        assert!(a.host_allowed("192.168.1.5"));
        assert!(a.host_allowed("192.168.1.5:80"));
        for bad in ["localhost", "127.0.0.1", "192.168.1.6", "desktop.lan", ""] {
            assert!(!a.host_allowed(bad), "{bad}");
        }

        // `::` is a wildcard too.
        assert!(matches!(
            BindAllowlist::for_bind("[::]:47900".parse().unwrap()),
            BindAllowlist::Wildcard
        ));
    }

    /// R19-F13: the asset-path gate refuses every non-`Normal`
    /// component — `..`/`.`/roots on any platform, and the Windows
    /// shapes a `/`-only split cannot see (`a\..\b` is one safe-
    /// looking `/`-segment that traverses on Windows).
    #[test]
    fn asset_path_gate_refuses_traversal() {
        // `a/./b` normalizes to `a/b` under the Component walk — the
        // join resolves to the same file, so it is accepted.
        for ok in [
            "app.js",
            "css/site.css",
            "a/b/c.chunk.mjs",
            "x.json",
            "a/./b",
        ] {
            assert!(clean_asset_path(ok), "{ok}");
        }
        for bad in [
            "",
            "/app.js",
            "a//b",
            "a/",
            "..",
            "../index.html",
            "a/../b",
            "a\\..\\b", // the Windows traversal — `\` never reaches join
            "a\\b",     // any backslash, traversal or not
            "a\0b.js",  // NUL truncates OS paths
            "C:/x.js",  // a Windows drive root is not a relative asset
        ] {
            assert!(!clean_asset_path(bad), "{bad:?}");
        }
    }
}
