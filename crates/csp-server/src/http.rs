//! M9c §2.12.3 — the axum surface: MCP Streamable-HTTP, the WS
//! telemetry stream, and the static SPA shell.
//!
//! # Security (spec-mandated, not optional)
//!
//! - **`Origin`/`Host` allowlists on every endpoint, read-only ones
//!   included.** DNS-rebinding protection: a browser page from another
//!   origin — or any request asserting a foreign `Host` authority (the
//!   rebinding shape: `attacker.example` resolved to 127.0.0.1) — gets
//!   403 before auth is even considered. Allowed: `localhost`,
//!   `127.0.0.1`, `[::1]` on any port, or an absent `Origin`/`Host`
//!   (non-browser clients do not send `Origin`; a missing `Host` falls
//!   back to the URI authority, then to the bound localhost address).
//! - **Bearer token on every endpoint except `/`** — the token is
//!   injected into the served HTML (a `<meta>` tag), lives at
//!   `$XDG_RUNTIME_DIR/csp/<pid>.token` mode 0600, is rotated per
//!   process start, and is never stored in a save. MCP clients read it
//!   from the file; browsers send it as `Authorization: Bearer` (or
//!   `?token=` on the WS upgrade, which cannot set headers).
//! - **Bounded ingress**: the body limit is the §2.2.1 64 KiB message
//!   cap; the WS frame cap matches; the command queue is
//!   [`crate::ingress::IngressQueue`]'s.
//! - **Bind `127.0.0.1`** by default (see [`crate::serve`]).

use std::convert::Infallible;
use std::sync::Arc;

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
}

impl AppState {
    /// Wire the state's parts from a backend + token.
    pub fn new(backend: Arc<dyn ControlBackend + Send + Sync>, token: Option<String>) -> Self {
        Self {
            token,
            mcp: Mcp::new(backend.clone()),
            backend,
            registry: ClientRegistry::new(),
        }
    }
}

/// The route table — every handler sits behind [`guard`].
pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/api/telemetry", get(api_telemetry))
        .route("/mcp", post(mcp_post).get(mcp_reject).delete(mcp_reject))
        .route("/ws", get(ws_upgrade))
        .layer(DefaultBodyLimit::max(MAX_MESSAGE_BYTES))
        .layer(middleware::from_fn_with_state(state.clone(), guard))
        .with_state(state)
}

// ---------------------------------------------------------------------------
// Guard — Origin allowlist + bearer token (all endpoints)
// ---------------------------------------------------------------------------

/// The `localhost` names — what `Origin` and `Host` authorities must
/// both reduce to under the §2.12.3 allowlist.
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

/// `true` when `Origin` is unset or names a localhost authority —
/// the spec's DNS-rebinding rule. We compare the *authority*, not the
/// scheme string, so `http://localhost:PORT` and
/// `https://127.0.0.1:PORT` both pass and `http://evil.example` never
/// does.
fn origin_allowed(origin: &str) -> bool {
    // Strip the scheme, then the path — keep the authority.
    let rest = origin
        .split("://")
        .nth(1)
        .unwrap_or(origin)
        .split('/')
        .next()
        .unwrap_or_default();
    localhost_name(authority_host(rest))
}

/// `true` when a `Host` (or URI) authority names localhost — the
/// other half of the DNS-rebinding rule. An unbracketed IPv6 literal
/// (`::1` bare) is malformed for an authority and denies.
fn host_allowed(host: &str) -> bool {
    localhost_name(authority_host(host.trim()))
}

/// The guard: the §2.12.3 Origin/Host allowlist first (403 on a
/// foreign authority — the DNS-rebinding shape), then the bearer
/// token on everything except the SPA shell the token is injected
/// into. `?token=` authenticates the WS upgrade path.
async fn guard(
    State(state): State<Arc<AppState>>,
    req: axum::http::Request<Body>,
    next: Next,
) -> Response {
    if let Some(origin) = req.headers().get(header::ORIGIN)
        && let Ok(s) = origin.to_str()
        && !origin_allowed(s)
    {
        return (StatusCode::FORBIDDEN, "origin not allowed").into_response();
    }
    // `Host` gets the same allowlist — asserted on every endpoint,
    // `/` included. Absent (HTTP/1.0, or h2's `:authority` riding the
    // URI) the URI's own authority is checked instead; neither
    // present means nothing foreign was claimed and the bound
    // localhost address stands.
    if let Some(host) = req
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .or_else(|| req.uri().host())
        && !host_allowed(host)
    {
        return (StatusCode::FORBIDDEN, "host not allowed").into_response();
    }
    if req.uri().path() == "/" {
        return next.run(req).await;
    }
    if let Some(token) = &state.token {
        let ok = bearer_matches(req.headers(), token)
            || Query::<TokenQuery>::try_from_uri(req.uri())
                .ok()
                .is_some_and(|q| q.token.as_deref() == Some(token.as_str()));
        if !ok {
            return (StatusCode::UNAUTHORIZED, "bad or missing bearer token").into_response();
        }
    }
    next.run(req).await
}

#[derive(Deserialize)]
struct TokenQuery {
    token: Option<String>,
}

fn bearer_matches(headers: &HeaderMap, token: &str) -> bool {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|t| t == token)
}

// ---------------------------------------------------------------------------
// `/` + `/api/telemetry` — the SPA shell and the debug read
// ---------------------------------------------------------------------------

/// The SPA shell — a placeholder until M9d ships the TanStack app;
/// the token lands in a `<meta>` the app reads (§2.12.3).
async fn index(State(state): State<Arc<AppState>>) -> Html<String> {
    let token = state.token.as_deref().unwrap_or("");
    Html(format!(
        "<!doctype html><html><head><meta charset=\"utf-8\">\
         <meta name=\"csp-token\" content=\"{token}\">\
         <title>Clanker Space Program — telemetry</title></head>\
         <body><h1>csp-server</h1><p>The telemetry SPA ships in M9d.\
         The WS endpoint is <code>/ws</code>; MCP is <code>POST /mcp</code>\
         (protocol revision {}).<p></body></html>",
        crate::mcp::PROTOCOL_REVISION
    ))
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
    let Some(response) = state.mcp.handle(client, &msg) else {
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
async fn ws_upgrade(
    State(state): State<Arc<AppState>>,
    Query(q): Query<McpQuery>,
    ws: WebSocketUpgrade,
) -> Response {
    let client = state.registry.id(q.client.as_deref().unwrap_or("default"));
    ws.max_message_size(MAX_MESSAGE_BYTES)
        .on_upgrade(move |socket| ws_session(socket, state, client))
}

/// One WS session: subscribe → project fields per tick of the rate
/// interval → send `{proto, kind: "telemetry", seq, tick, payload}`.
/// Backpressure is drop-to-latest by construction — each send reads
/// the `ArcSwap` anew; a slow client degrades its own rate only.
#[allow(clippy::too_many_lines)] // the select! loop holds the whole session state machine
async fn ws_session(socket: WebSocket, state: Arc<AppState>, client: u32) {
    let (mut sink, mut stream) = socket.split();
    let mut fields: Option<Vec<String>> = None;
    let mut rate_hz = 16_u64;
    let mut seq = 0_u64;
    let mut ticker = tokio::time::interval(std::time::Duration::from_millis(1000 / rate_hz));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                let Some(fs) = fields.as_ref() else { continue };
                let Some(snap) = state.backend.telemetry() else { continue };
                let (payload, unknown) = snap.project(fs);
                let frame = json!({
                    "proto": WIRE_PROTO, "kind": "telemetry",
                    "seq": seq, "tick": snap.tick,
                    "payload": payload, "unknown_fields": unknown,
                });
                seq += 1;
                if sink.send(Message::Text(frame.to_string().into())).await.is_err() {
                    return;
                }
            }
            msg = stream.next() => {
                let Some(Ok(msg)) = msg else { return };
                let Message::Text(text) = msg else { continue };
                let Ok(v) = serde_json::from_str::<Value>(&text) else { continue };
                match v.get("kind").and_then(Value::as_str) {
                    Some("subscribe") => {
                        if v.get("proto") != Some(&json!(WIRE_PROTO)) {
                            let _ = send_json(&mut sink, &json!({
                                "proto": WIRE_PROTO, "kind": "error",
                                "error": format!("wire proto must be {WIRE_PROTO}"),
                            })).await;
                            continue;
                        }
                        let fs: Vec<String> = v
                            .get("fields")
                            .and_then(Value::as_array)
                            .map(|a| {
                                a.iter()
                                    .filter_map(|f| f.as_str().map(str::to_string))
                                    .take(MAX_FIELDS)
                                    .collect()
                            })
                            .unwrap_or_default();
                        if fs.is_empty() {
                            let _ = send_json(&mut sink, &json!({
                                "proto": WIRE_PROTO, "kind": "error",
                                "error": "subscription needs a non-empty fields list",
                            })).await;
                            continue;
                        }
                        let rate = v.get("rate_hz").and_then(Value::as_u64).unwrap_or(16);
                        if !VALID_RATES.contains(&rate) {
                            let _ = send_json(&mut sink, &json!({
                                "proto": WIRE_PROTO, "kind": "error",
                                "error": format!("rate_hz must divide 64: {VALID_RATES:?}"),
                            })).await;
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
                        let _ = send_json(&mut sink, &json!({
                            "proto": WIRE_PROTO, "kind": "subscribed",
                            "rate_hz": rate_hz,
                        })).await;
                    }
                    Some("command") => {
                        let cmd = v.get("command").cloned().unwrap_or(Value::Null);
                        let result = serde_json::from_value::<SimCommand>(cmd)
                            .map_err(|e| e.to_string())
                            .and_then(|c| {
                                state
                                    .backend
                                    .submit(client, c)
                                    .map_err(|e| e.to_string())
                            });
                        let frame = match result {
                            Ok(rejected) => json!({
                                "proto": WIRE_PROTO, "kind": "command_ack",
                                "seq": seq, "rejected": rejected,
                            }),
                            Err(e) => json!({
                                "proto": WIRE_PROTO, "kind": "command_ack",
                                "seq": seq, "error": e,
                            }),
                        };
                        seq += 1;
                        let _ = send_json(&mut sink, &frame).await;
                    }
                    _ => {}
                }
            }
        }
    }
}

async fn send_json(
    sink: &mut futures_util::stream::SplitSink<WebSocket, Message>,
    v: &Value,
) -> Result<(), axum::Error> {
    sink.send(Message::Text(v.to_string().into())).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_allowlist() {
        for ok in [
            "http://localhost:47900",
            "https://127.0.0.1",
            "http://[::1]:8080/path?q=1",
            "http://LOCALHOST",
        ] {
            assert!(origin_allowed(ok), "{ok}");
        }
        for bad in [
            "http://evil.example",
            "http://localhost.evil.example",
            "http://127.0.0.1.evil.example",
            "javascript:alert(1)",
            "",
        ] {
            assert!(!origin_allowed(bad), "{bad}");
        }
    }

    #[test]
    fn host_allowlist() {
        for ok in [
            "localhost",
            "localhost:47900",
            "127.0.0.1",
            "127.0.0.1:8080",
            "[::1]",
            "[::1]:9000",
            "LOCALHOST:80",
        ] {
            assert!(host_allowed(ok), "{ok}");
        }
        for bad in [
            "evil.example",
            "localhost.evil.example",
            "127.0.0.1.evil.example",
            "user@evil.example:80",
            "::1", // unbracketed v6 is a malformed authority
            "",
        ] {
            assert!(!host_allowed(bad), "{bad}");
        }
    }
}
