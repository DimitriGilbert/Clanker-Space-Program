#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(clippy::arc_with_non_send_sync)]
//! M9c §8.5 interface e2e — a test client drives MCP tool calls and
//! the WS subscription against a live `csp-server` over a real socket
//! (the harness's `LocalGame`-equivalent is `GameHost`, the
//! attach-to-a-running-game host).

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::Arc;

use csp_server::backend::GameHost;
use csp_server::serve::{Server, ServerConfig};
use csp_server::{ControlBackend, IngressQueue};
use csp_sim::{SourceClass, World};
use serde_json::{Value, json};

/// An ephemeral-port, fixed-token server over a `World::new()`.
async fn boot() -> (Server, Arc<GameHost>) {
    boot_with(World::new).await
}

async fn boot_with(make: impl FnOnce() -> World + Send + 'static) -> (Server, Arc<GameHost>) {
    boot_with_full(make, IngressQueue::new()).await
}

async fn boot_with_full(
    make: impl FnOnce() -> World + Send + 'static,
    queue: IngressQueue,
) -> (Server, Arc<GameHost>) {
    let dir = PathBuf::from(
        std::env::var("CARGO_TARGET_TMPDIR")
            .unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().into_owned()),
    )
    .join(format!("csp-e2e-{}", std::process::id()));
    let host = Arc::new(GameHost::spawn_full(make, dir, queue, None).expect("game host"));
    let backend: Arc<dyn ControlBackend + Send + Sync> = host.clone();
    let cfg = ServerConfig {
        bind: "127.0.0.1:0".parse().expect("bind"),
        token: Some("e2e-token".into()),
        auth_disabled: false,
        token_dir: None,
        dashboard_dir: None,
        spa_dir: None,
    };
    let server = Server::start(cfg, backend).await.expect("server binds");
    (server, host)
}

/// `boot` with a dashboard store under the temp dir (the M9d
/// sidecar config path).
async fn boot_dashboards() -> (Server, Arc<GameHost>, PathBuf) {
    let dir = PathBuf::from(
        std::env::var("CARGO_TARGET_TMPDIR")
            .unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().into_owned()),
    )
    .join(format!("csp-e2e-dash-{}", std::process::id()));
    let host = Arc::new(
        GameHost::spawn_full(World::new, dir.clone(), IngressQueue::new(), None)
            .expect("game host"),
    );
    let backend: Arc<dyn ControlBackend + Send + Sync> = host.clone();
    let cfg = ServerConfig {
        bind: "127.0.0.1:0".parse().expect("bind"),
        token: Some("e2e-token".into()),
        auth_disabled: false,
        token_dir: None,
        dashboard_dir: Some(dir.clone()),
        spa_dir: None,
    };
    let server = Server::start(cfg, backend).await.expect("server binds");
    (server, host, dir)
}

/// `boot` with a temp `spa_dir` shaped like the `apps/telemetry`
/// build output (`index.html` + `assets/app.js`) — the M9d
/// SPA-serving path no binary wires up yet. Returns the spa root so
/// the test can prove which file was served.
async fn boot_spa() -> (Server, Arc<GameHost>, PathBuf) {
    let dir = PathBuf::from(
        std::env::var("CARGO_TARGET_TMPDIR")
            .unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().into_owned()),
    )
    .join(format!("csp-e2e-spa-{}", std::process::id()));
    let spa = dir.join("spa");
    std::fs::create_dir_all(spa.join("assets")).expect("spa dir");
    std::fs::write(
        spa.join("index.html"),
        "<!doctype html><html><head><title>t</title></head>\
         <body>SPA-SHELL-MARKER</body></html>",
    )
    .expect("index.html");
    std::fs::write(spa.join("assets").join("app.js"), "const csp_marker = 1;\n").expect("app.js");
    let host = Arc::new(
        GameHost::spawn_full(World::new, dir.join("saves"), IngressQueue::new(), None)
            .expect("game host"),
    );
    let backend: Arc<dyn ControlBackend + Send + Sync> = host.clone();
    let cfg = ServerConfig {
        bind: "127.0.0.1:0".parse().expect("bind"),
        token: Some("e2e-token".into()),
        auth_disabled: false,
        token_dir: None,
        dashboard_dir: None,
        spa_dir: Some(spa.clone()),
    };
    let server = Server::start(cfg, backend).await.expect("server binds");
    (server, host, spa)
}

/// A per-test scratch dir under `CARGO_TARGET_TMPDIR` (or the system
/// temp dir).
fn scratch(tag: &str) -> PathBuf {
    PathBuf::from(
        std::env::var("CARGO_TARGET_TMPDIR")
            .unwrap_or_else(|_| std::env::temp_dir().to_string_lossy().into_owned()),
    )
    .join(format!("{tag}-{}", std::process::id()))
}

/// A one-shot HTTP/1.1 request over a real socket — returns
/// `(status, body)`.
fn http(
    addr: std::net::SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> (u16, String) {
    http_host(addr, &addr.to_string(), method, path, headers, body)
}

/// `http` with the `Host` header under test control — the guard's
/// Host allowlist needs requests that assert a foreign authority.
fn http_host(
    addr: std::net::SocketAddr,
    host: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> (u16, String) {
    let mut s = TcpStream::connect(addr).expect("connect");
    let mut req = format!(
        "{method} {path} HTTP/1.1\r\nHost: {host}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str(&format!("\r\n{body}"));
    s.write_all(req.as_bytes()).expect("write");
    let mut buf = String::new();
    s.read_to_string(&mut buf).expect("read");
    let status: u16 = buf[9..12].trim().parse().expect("status");
    let body = buf.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
    (status, body)
}

fn bearer() -> [(&'static str, &'static str); 1] {
    [("Authorization", "Bearer e2e-token")]
}

fn rpc(id: u64, method: &str, params: Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string()
}

fn tool(id: u64, name: &str, args: Value) -> String {
    rpc(id, "tools/call", json!({"name": name, "arguments": args}))
}

/// The MCP `initialize` handshake for one client name (`None` = the
/// default) — the lifecycle gate makes it every tool-driving test's
/// first request on that client.
fn mcp_init(addr: std::net::SocketAddr, client: Option<&str>) {
    let path = match client {
        Some(c) => format!("/mcp?client={c}"),
        None => "/mcp".to_string(),
    };
    let (status, body) = http(
        addr,
        "POST",
        &path,
        &[
            ("Authorization", "Bearer e2e-token"),
            ("Content-Type", "application/json"),
        ],
        &rpc(
            0,
            "initialize",
            json!({"protocolVersion": "2026-07-28", "capabilities": {}}),
        ),
    );
    let v: Value = serde_json::from_str(&body).expect("init json");
    assert_eq!(status, 200, "{body}");
    assert_eq!(v["result"]["protocolVersion"], "2026-07-28", "{body}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn server_boots_and_serves() {
    let (server, _host) = boot().await;
    // `/` is token-free by design — and free of the secret: the shell
    // is served verbatim; the token only ever travels the
    // authenticated channel (token file / `Authorization` header /
    // `?token=` on `/ws`).
    let (status, body) = http(server.addr, "GET", "/", &[], "");
    assert_eq!(status, 200);
    assert!(!body.contains("e2e-token"), "token leaked into /: {body}");
    assert!(!body.contains("csp-token"), "{body}");
    // `/api/telemetry` needs the bearer.
    let (status, _) = http(server.addr, "GET", "/api/telemetry", &[], "");
    assert_eq!(status, 401);
    let (status, body) = http(server.addr, "GET", "/api/telemetry", &bearer(), "");
    assert_eq!(status, 200);
    assert!(body.contains("\"tick\""));
    server.shutdown();
}

/// R19-F1 regression: a generated token must be minted promptly — the
/// old `fs::read("/dev/urandom")` blocked on an EOF that never comes,
/// so this `Server::start` would deadlock before bind. The timeout is
/// the proof.
/// R19-F2 regression: the generated token must not appear in `GET /`
/// — the token-free shell no longer carries the secret — while the
/// bearer-authenticated path still works.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn generated_token_is_prompt_and_never_served() {
    let dir = scratch("csp-e2e-gen");
    let host = Arc::new(
        GameHost::spawn_full(World::new, dir, IngressQueue::new(), None).expect("game host"),
    );
    let backend: Arc<dyn ControlBackend + Send + Sync> = host.clone();
    let cfg = ServerConfig {
        bind: "127.0.0.1:0".parse().expect("bind"),
        token: None, // the production shape: generate per start
        auth_disabled: false,
        token_dir: None,
        dashboard_dir: None,
        spa_dir: None,
    };
    let server = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        Server::start(cfg, backend),
    )
    .await
    .expect("Server::start hangs — generate_token must return promptly")
    .expect("server binds");
    assert_eq!(server.token.len(), 64);
    assert!(server.token.bytes().all(|b| b.is_ascii_hexdigit()));
    let (status, body) = http(server.addr, "GET", "/", &[], "");
    assert_eq!(status, 200);
    assert!(
        !body.contains(&server.token),
        "generated token served at /: {body}"
    );
    let auth = format!("Bearer {}", server.token);
    let (status, _) = http(
        server.addr,
        "GET",
        "/api/telemetry",
        &[("Authorization", auth.as_str())],
        "",
    );
    assert_eq!(status, 200);
    server.shutdown();
}

/// `?token=` is the WS bootstrap credential only — a browser upgrade
/// cannot set headers. On every other endpoint a bearer in the query
/// string must not satisfy auth (it would leak into logs).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn query_token_authenticates_only_the_ws_path() {
    let (server, _host) = boot().await;
    for (method, path) in [
        ("GET", "/api/telemetry?token=e2e-token"),
        ("GET", "/api/dashboards?token=e2e-token"),
        ("GET", "/mcp?token=e2e-token"),
        ("POST", "/mcp?token=e2e-token"),
    ] {
        let (status, _) = http(server.addr, method, path, &[], "");
        assert_eq!(status, 401, "{method} {path}");
    }
    // `/ws` still accepts it: the guard passes, so the failure (if
    // any) is the extractor's — anything but 401 means auth held.
    let (status, _) = http(server.addr, "GET", "/ws?token=e2e-token", &[], "");
    assert_ne!(status, 401);
    server.shutdown();
}

/// RFC 9110 §11.4: the auth scheme is case-insensitive — `bearer` and
/// `BEARER` authenticate like `Bearer`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bearer_scheme_is_case_insensitive() {
    let (server, _host) = boot().await;
    for scheme in ["bearer", "Bearer", "BEARER"] {
        let header = format!("{scheme} e2e-token");
        let (status, _) = http(
            server.addr,
            "GET",
            "/api/telemetry",
            &[("Authorization", header.as_str())],
            "",
        );
        assert_eq!(status, 200, "{scheme}");
    }
    server.shutdown();
}

/// The discovery file contract (§2.12.3): `<token>\n<port>\n` under a
/// `0700` runtime dir, created `0600` atomically, and removed on
/// `shutdown` — no stale token may outlive its server.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn token_file_lifecycle_is_0600_and_cleaned_up() {
    let dir = scratch("csp-e2e-tokenfile");
    let token_dir = dir.join("runtime");
    let host = Arc::new(
        GameHost::spawn_full(World::new, dir.join("saves"), IngressQueue::new(), None)
            .expect("game host"),
    );
    let backend: Arc<dyn ControlBackend + Send + Sync> = host.clone();
    let cfg = ServerConfig {
        bind: "127.0.0.1:0".parse().expect("bind"),
        token: Some("e2e-token".into()),
        auth_disabled: false,
        token_dir: Some(token_dir.clone()),
        dashboard_dir: None,
        spa_dir: None,
    };
    let server = Server::start(cfg, backend).await.expect("server binds");
    let file = server.token_file.clone().expect("token file written");
    assert_eq!(file.parent(), Some(token_dir.as_path()));
    let contents = std::fs::read_to_string(&file).expect("read token file");
    assert_eq!(contents, format!("e2e-token\n{}\n", server.addr.port()));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&file)
                .expect("file meta")
                .permissions()
                .mode()
                & 0o777,
            0o600,
            "the token file must be created 0600, not chmod'd after"
        );
        assert_eq!(
            std::fs::metadata(&token_dir)
                .expect("dir meta")
                .permissions()
                .mode()
                & 0o777,
            0o700,
            "the runtime dir must be owner-only"
        );
    }
    server.shutdown();
    assert!(!file.exists(), "stale token file survived shutdown");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A pre-existing `<pid>.token` — here an attacker-planted symlink —
/// must be refused, never followed and truncated (R19-F7).
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn token_file_refuses_a_preplanted_symlink() {
    let dir = scratch("csp-e2e-toklink");
    std::fs::create_dir_all(&dir).expect("token dir");
    let target = dir.join("victim.txt");
    std::fs::write(&target, "keep me").expect("victim");
    std::os::unix::fs::symlink(&target, dir.join(format!("{}.token", std::process::id())))
        .expect("symlink");
    let host = Arc::new(
        GameHost::spawn_full(World::new, dir.join("saves"), IngressQueue::new(), None)
            .expect("game host"),
    );
    let backend: Arc<dyn ControlBackend + Send + Sync> = host.clone();
    let cfg = ServerConfig {
        bind: "127.0.0.1:0".parse().expect("bind"),
        token: Some("e2e-token".into()),
        auth_disabled: false,
        token_dir: Some(dir.clone()),
        dashboard_dir: None,
        spa_dir: None,
    };
    match Server::start(cfg, backend).await {
        Err(csp_server::ServeError::TokenFile(_)) => {}
        Err(e) => panic!("expected a token-file refusal, got {e}"),
        Ok(_) => panic!("a pre-planted symlinked token path was followed"),
    }
    assert_eq!(
        std::fs::read_to_string(&target).expect("victim"),
        "keep me",
        "the symlink target must be untouched"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn origin_allowlist_blocks_foreign_origins() {
    let (server, _host) = boot().await;
    let (status, _) = http(
        server.addr,
        "GET",
        "/api/telemetry",
        &[
            ("Authorization", "Bearer e2e-token"),
            ("Origin", "http://evil.example"),
        ],
        "",
    );
    assert_eq!(status, 403);
    let (status, _) = http(
        server.addr,
        "GET",
        "/api/telemetry",
        &[
            ("Authorization", "Bearer e2e-token"),
            ("Origin", "http://localhost:9999"),
        ],
        "",
    );
    assert_eq!(status, 200);
    server.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn host_allowlist_blocks_foreign_hosts() {
    let (server, _host) = boot().await;
    // The DNS-rebinding shape: a request asserting a foreign Host
    // authority gets 403 before auth — on the token-free `/` too.
    for host in ["evil.example", "localhost.evil.example"] {
        let (status, _) = http_host(server.addr, host, "GET", "/", &[], "");
        assert_eq!(status, 403, "{host}");
    }
    // Localhost authorities pass on any port.
    for host in ["localhost", "localhost:1", "127.0.0.1:47900", "[::1]:8080"] {
        let (status, _) = http_host(server.addr, host, "GET", "/", &[], "");
        assert_eq!(status, 200, "{host}");
    }
    server.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mcp_round_trip_and_external_ordering() {
    let (server, host) = boot().await;
    // initialize → protocolVersion is the pinned revision.
    let (status, body) = http(
        server.addr,
        "POST",
        "/mcp",
        &[
            ("Authorization", "Bearer e2e-token"),
            ("Content-Type", "application/json"),
        ],
        &rpc(
            1,
            "initialize",
            json!({"protocolVersion": "2026-07-28", "capabilities": {}}),
        ),
    );
    assert_eq!(status, 200);
    let v: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(v["result"]["protocolVersion"], "2026-07-28");
    // tools/list declares session-class tools.
    let (_, body) = http(
        server.addr,
        "POST",
        "/mcp",
        &[
            ("Authorization", "Bearer e2e-token"),
            ("Content-Type", "application/json"),
        ],
        &rpc(2, "tools/list", json!({})),
    );
    let v: Value = serde_json::from_str(&body).expect("json");
    assert!(
        v["result"]["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .any(|t| t["class"] == "session")
    );
    // timewarp.set lands an External-stamped command — the e2e
    // ordering proof: the command log records `(External, 0)` under
    // the one-tick contract. `agent-a` is its own client — the
    // lifecycle gate wants its handshake first.
    mcp_init(server.addr, Some("agent-a"));
    let (_, body) = http(
        server.addr,
        "POST",
        "/mcp?client=agent-a",
        &[
            ("Authorization", "Bearer e2e-token"),
            ("Content-Type", "application/json"),
        ],
        &tool(3, "timewarp.set", json!({"level": 8})),
    );
    let v: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(v["result"]["isError"], false);
    let stamped = host.inspect(|w| {
        let k = w.command_log().last().expect("log entry").0;
        json!({"class": format!("{:?}", k.source_class), "id": k.source_id})
    });
    assert_eq!(stamped["class"], "External");
    // "default" registered first (the initialize call) → "agent-a" is
    // client 1, assigned in connection order (§2.12.3).
    assert_eq!(stamped["id"], 1);
    // The SSE upgrade path — same call, text/event-stream wire.
    let (status, body) = http(
        server.addr,
        "POST",
        "/mcp",
        &[
            ("Authorization", "Bearer e2e-token"),
            ("Content-Type", "application/json"),
            ("Accept", "application/json, text/event-stream"),
        ],
        &tool(4, "session.snapshot", json!({})),
    );
    assert_eq!(status, 200);
    assert!(body.contains("event: message") || body.contains("data:"));
    // GET/DELETE are 405 under revision 2026-07-28.
    let (status, _) = http(server.addr, "GET", "/mcp", &bearer(), "");
    assert_eq!(status, 405);
    server.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bounded_ingress_returns_backpressure() {
    // Per-client budget 2 → a batch of 3 in one drain window is
    // refused at the third command (§2.2.1 backpressure error).
    let (server, host) = boot_with_full(World::new, IngressQueue::with_limits(64, 2)).await;
    mcp_init(server.addr, None);
    let cmds: Vec<Value> = (0..3).map(|_| json!({"SetWarp": 1})).collect();
    let (_, body) = http(
        server.addr,
        "POST",
        "/mcp",
        &[
            ("Authorization", "Bearer e2e-token"),
            ("Content-Type", "application/json"),
        ],
        &tool(1, "sim.command", json!({"commands": cmds})),
    );
    let v: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(v["result"]["isError"], true, "{body}");
    let text = v["result"]["content"][0]["text"].as_str().expect("text");
    assert!(text.contains("budget"), "{text}");
    // R19-F5: the refused batch is atomic — its pushed prefix must not
    // linger and drain under the next unrelated submit. A clean
    // AdvanceTo(5) follows; the log must hold exactly that one command.
    let (_, body) = http(
        server.addr,
        "POST",
        "/mcp",
        &[
            ("Authorization", "Bearer e2e-token"),
            ("Content-Type", "application/json"),
        ],
        &tool(2, "sim.command", json!({"command": {"AdvanceTo": 5}})),
    );
    let v: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(v["result"]["isError"], false, "{body}");
    let logged = host.inspect(|w| {
        let cmds: Vec<String> = w
            .command_log()
            .iter()
            .map(|(_, c)| format!("{c:?}"))
            .collect();
        json!({"tick": w.clock().ticks(), "commands": cmds})
    });
    assert_eq!(
        logged,
        json!({"tick": 5, "commands": ["AdvanceTo(5)"]}),
        "the failed batch's residue must not apply"
    );
    server.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lease_enforcement_over_the_wire() {
    // A fixture world gives us a real vessel to lease — the flight
    // recipe's own construction.
    let dir =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/test-fixtures/flight-launch");
    let make = move || {
        let recipe = csp_bench::scenarios::find("flight-launch").expect("recipe");
        (recipe.initial)(&dir).expect("fixture world")
    };
    let (server, host) = boot_with(make).await;
    // Two named clients drive tools — each needs its own handshake.
    mcp_init(server.addr, Some("setup"));
    mcp_init(server.addr, Some("agent"));
    // Spawn a rails vessel as the player (Ui through sim.command? —
    // external clients stamp External; SpawnVessel is not
    // lease-gated, so sim.command works for the setup too).
    let spawn = json!({"SpawnVessel": {
    "parent": "terrin",
    "elements": {
        "semi_major_axis_m": 700_000.0, "eccentricity": 0.0,
        "inclination_rad": 0.0, "ascending_node_rad": 0.0,
        "arg_periapsis_rad": 0.0, "mean_anomaly_epoch_rad": 0.0,
        "period_s": 1958.1284360312443
    }}});
    let (_, body) = http(
        server.addr,
        "POST",
        "/mcp?client=setup",
        &[
            ("Authorization", "Bearer e2e-token"),
            ("Content-Type", "application/json"),
        ],
        &tool(1, "sim.command", json!({"command": spawn})),
    );
    let v: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(v["result"]["isError"], false, "{body}");
    // Without a lease, vessel.control rejects for client "agent".
    let (_, body) = http(
        server.addr,
        "POST",
        "/mcp?client=agent",
        &[
            ("Authorization", "Bearer e2e-token"),
            ("Content-Type", "application/json"),
        ],
        &tool(2, "vessel.control", json!({"vessel": 0, "throttle": 1.0})),
    );
    let v: Value = serde_json::from_str(&body).expect("json");
    let text = v["result"]["content"][0]["text"].as_str().unwrap_or("");
    assert!(text.contains("lease"), "{body}");
    // take_control, then release — the full handshake over the wire.
    let (_, body) = http(
        server.addr,
        "POST",
        "/mcp?client=agent",
        &[
            ("Authorization", "Bearer e2e-token"),
            ("Content-Type", "application/json"),
        ],
        &tool(3, "vessel.take_control", json!({"vessel": 0})),
    );
    let v: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(v["result"]["isError"], false, "{body}");
    let lease = host.inspect(|w| {
        json!(format!(
            "{:?}",
            w.vessels()[&csp_sim::VesselId(0)].control_lease
        ))
    });
    // "setup" registered first (id 0), "agent" is client 1.
    assert_eq!(lease, json!("Agent(1)"));
    server.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn commands_vs_local_world_hash() {
    // The e2e determinism check: an identical command stream through
    // MCP produces the byte-identical state hash as the same stream
    // driven directly into `World` — "agents get the same one-tick
    // latency and the same determinism contract".
    let seq = [
        csp_sim::SimCommand::SetWarp(4),
        csp_sim::SimCommand::AdvanceTo(100),
        csp_sim::SimCommand::SetAlarm {
            tick: 50,
            label: "e2e".into(),
        },
    ];
    // Same production pattern as the headless host: enqueue, step
    // one boundary — the identical stamped sequence must result.
    let mut local = World::new();
    for c in &seq {
        local.enqueue(SourceClass::External, 0, c.clone());
        let t = local.clock().ticks();
        local.step_frame(t + 1).expect("step");
    }

    let (server, _host) = boot().await;
    mcp_init(server.addr, None);
    for (i, c) in seq.iter().enumerate() {
        let cmd = serde_json::to_value(c).expect("encode");
        let (_, body) = http(
            server.addr,
            "POST",
            "/mcp",
            &[
                ("Authorization", "Bearer e2e-token"),
                ("Content-Type", "application/json"),
            ],
            &tool(i as u64 + 1, "sim.command", json!({"command": cmd})),
        );
        let v: Value = serde_json::from_str(&body).expect("json");
        assert_eq!(v["result"]["isError"], false, "{body}");
    }
    let (_, body) = http(
        server.addr,
        "POST",
        "/mcp",
        &[
            ("Authorization", "Bearer e2e-token"),
            ("Content-Type", "application/json"),
        ],
        &tool(9, "session.snapshot", json!({})),
    );
    let v: Value = serde_json::from_str(&body).expect("json");
    let text = v["result"]["content"][0]["text"].as_str().expect("text");
    let remote: Value = serde_json::from_str(text).expect("summary");
    let local_hash = local.state_hash().expect("hash");
    let local_hex: String = local_hash.iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(remote["hash"], json!(local_hex));
    // AdvanceTo(100) drains at tick 1; SetAlarm{50} drains at tick
    // 100 and its alarm clamps to 101 (strictly-ahead), so the clock
    // lands on the drain of the pump that stepped it there — both
    // sides agree, which is the point.
    assert_eq!(remote["tick"], json!(local.clock().ticks()));
    server.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ws_subscription_streams_fields() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite;

    let (server, _host) = boot().await;
    let url = format!("ws://{}/ws?token=e2e-token&client=sub", server.addr);
    let (mut ws, _) = tokio_tungstenite::connect_async(url)
        .await
        .expect("ws connect");
    ws.send(tungstenite::Message::Text(
        json!({"proto": 1, "kind": "subscribe", "fields": ["tick", "warp_level"], "rate_hz": 64})
            .to_string()
            .into(),
    ))
    .await
    .expect("send subscribe");
    // First frame: the subscribed ack.
    let msg = ws.next().await.expect("frame").expect("ok");
    let v: Value = serde_json::from_str(&msg.into_text().expect("text")).expect("json");
    assert_eq!(v["kind"], "subscribed");
    // Then telemetry frames at the subscribed rate.
    let msg = ws.next().await.expect("frame").expect("ok");
    let v: Value = serde_json::from_str(&msg.into_text().expect("text")).expect("json");
    assert_eq!(v["kind"], "telemetry");
    assert_eq!(v["proto"], 1);
    assert!(v["payload"]["tick"].is_u64());
    server.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ws_command_channel_enqueues() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite;

    let (server, host) = boot().await;
    let url = format!("ws://{}/ws?token=e2e-token&client=wsc", server.addr);
    let (mut ws, _) = tokio_tungstenite::connect_async(url)
        .await
        .expect("ws connect");
    ws.send(tungstenite::Message::Text(
        json!({"proto": 1, "kind": "subscribe", "fields": ["tick"], "rate_hz": 64})
            .to_string()
            .into(),
    ))
    .await
    .expect("subscribe");
    ws.send(tungstenite::Message::Text(
        json!({"proto": 1, "kind": "command",
               "command": {"SetWarp": 3}})
        .to_string()
        .into(),
    ))
    .await
    .expect("send command");
    // Frames arrive: subscribed, then command_ack, possibly telemetry
    // interleaved — scan a few.
    let mut saw_ack = false;
    for _ in 0..8 {
        let msg = ws.next().await.expect("frame").expect("ok");
        let v: Value = serde_json::from_str(&msg.into_text().expect("text")).expect("json");
        if v["kind"] == "command_ack" {
            saw_ack = true;
            break;
        }
    }
    assert!(saw_ack);
    let stamped = host.inspect(|w| {
        let k = w.command_log().last().expect("log").0;
        json!({"class": format!("{:?}", k.source_class)})
    });
    assert_eq!(stamped["class"], "External");
    server.shutdown();
}

/// R19-F14 wire asymmetries: `command` envelopes must pass the same
/// `proto` gate as `subscribe`; an over-cap `fields` list is an
/// explicit error, never a silent truncation; and `command_ack`s
/// carry their own sequence, not the telemetry `seq`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ws_envelope_rules_are_symmetric() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite;

    let (server, _host) = boot().await;
    let url = format!("ws://{}/ws?token=e2e-token&client=asym", server.addr);
    let (mut ws, _) = tokio_tungstenite::connect_async(url)
        .await
        .expect("ws connect");
    // A command without `proto` must be refused like a proto-less
    // subscribe — the version field covers the whole wire surface.
    ws.send(tungstenite::Message::Text(
        json!({"kind": "command", "command": {"SetWarp": 1}})
            .to_string()
            .into(),
    ))
    .await
    .expect("send command");
    let msg = ws.next().await.expect("frame").expect("ok");
    let v: Value = serde_json::from_str(&msg.into_text().expect("text")).expect("json");
    assert_eq!(v["kind"], "error");
    assert!(v["error"].as_str().expect("error").contains("proto"), "{v}");

    // An over-cap fields list is refused, not truncated — nothing
    // streams, and the refusal names the cap.
    let fields: Vec<String> = (0..65).map(|i| format!("f{i}")).collect();
    ws.send(tungstenite::Message::Text(
        json!({"proto": 1, "kind": "subscribe", "fields": fields})
            .to_string()
            .into(),
    ))
    .await
    .expect("send subscribe");
    let msg = ws.next().await.expect("frame").expect("ok");
    let v: Value = serde_json::from_str(&msg.into_text().expect("text")).expect("json");
    assert_eq!(v["kind"], "error");
    assert!(v["error"].as_str().expect("error").contains("cap"), "{v}");

    // A legal subscribe at 1 Hz, then two commands: their acks run
    // on their own `seq` (0, 1), disjoint from the telemetry stream.
    ws.send(tungstenite::Message::Text(
        json!({"proto": 1, "kind": "subscribe", "fields": ["tick"], "rate_hz": 1})
            .to_string()
            .into(),
    ))
    .await
    .expect("subscribe");
    for _ in 0..2 {
        ws.send(tungstenite::Message::Text(
            json!({"proto": 1, "kind": "command", "command": {"SetWarp": 1}})
                .to_string()
                .into(),
        ))
        .await
        .expect("command");
    }
    let mut ack_seqs = Vec::new();
    for _ in 0..10 {
        let msg = ws.next().await.expect("frame").expect("ok");
        let v: Value = serde_json::from_str(&msg.into_text().expect("text")).expect("json");
        if v["kind"] == "command_ack" {
            ack_seqs.push(v["seq"].as_u64().expect("seq"));
        }
        if ack_seqs.len() == 2 {
            break;
        }
    }
    assert_eq!(ack_seqs, vec![0, 1], "ack sequence must start at 0");
    server.shutdown();
}

/// R19-F18: the `/ws` connection count is bounded — the cap+1th
/// upgrade is refused (503), and a closed session returns its slot.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ws_connection_cap_refuses_overflow() {
    let (server, _host) = boot().await;
    let url = |i: usize| format!("ws://{}/ws?token=e2e-token&client=c{i}", server.addr);
    let mut held = Vec::new();
    // csp_server::http's MAX_WS_CONNECTIONS = 16.
    for i in 0..16 {
        let (ws, _) = tokio_tungstenite::connect_async(url(i))
            .await
            .expect("within cap");
        held.push(ws);
    }
    // The 17th upgrade is refused before the WS handshake.
    let err = tokio_tungstenite::connect_async(url(16))
        .await
        .expect_err("cap+1th connection must refuse");
    assert!(err.to_string().contains("503"), "{err}");
    // Dropping one session frees its slot — the permit returns on
    // session end, and a retry then connects.
    held.pop();
    for _ in 0..40 {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        if tokio_tungstenite::connect_async(url(17)).await.is_ok() {
            server.shutdown();
            return;
        }
    }
    panic!("a freed WS slot never became available");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dashboards_sidecar_api_and_tools() {
    let (server, _host, dir) = boot_dashboards().await;
    mcp_init(server.addr, None);
    // 503 when unconfigured — verify on the store-less boot too:
    // here the store exists, so exercise the full round-trip.
    let dash = json!({
        "schema_version": 1, "name": "flight", "title": "Flight",
        "rate_hz": 16,
        "widgets": [{
            "id": "alt", "kind": "gauge", "title": "Altitude",
            "fields": ["vessels.0.altitude_m"],
            "rect": {"x": 0, "y": 0, "w": 4, "h": 3},
            "options": {"unit": "m"}
        }],
    });
    // PUT validates + stores; the file lands as sidecar JSON.
    let (status, _) = http(
        server.addr,
        "PUT",
        "/api/dashboards/flight",
        &bearer(),
        &dash.to_string(),
    );
    assert_eq!(status, 200);
    assert!(dir.join("flight.json").exists());
    // GET returns the stored document verbatim.
    let (status, body) = http(server.addr, "GET", "/api/dashboards/flight", &bearer(), "");
    assert_eq!(status, 200);
    let got: Value = serde_json::from_str(&body).expect("json");
    assert_eq!(got["name"], json!("flight"));
    // List shows it.
    let (status, body) = http(server.addr, "GET", "/api/dashboards", &bearer(), "");
    assert_eq!(status, 200);
    assert!(body.contains("flight"));
    // Validation refuses: unknown widget kind → 400.
    let bad = json!({"schema_version": 1, "name": "x", "title": "x",
        "widgets": [{"id": "w", "kind": "hologram", "title": "w",
                     "fields": ["tick"], "rect": {"x":0,"y":0,"w":2,"h":2}}]});
    let (status, _) = http(
        server.addr,
        "PUT",
        "/api/dashboards/x",
        &bearer(),
        &bad.to_string(),
    );
    assert_eq!(status, 400);
    // The dashboard.* MCP tools share the same store (session-class).
    let (_, body) = http(
        server.addr,
        "POST",
        "/mcp",
        &[
            ("Authorization", "Bearer e2e-token"),
            ("Content-Type", "application/json"),
        ],
        &tool(1, "dashboard.list", json!({})),
    );
    let v: Value = serde_json::from_str(&body).expect("json");
    let text = v["result"]["content"][0]["text"].as_str().expect("text");
    assert!(text.contains("flight"), "{text}");
    let (_, body) = http(
        server.addr,
        "POST",
        "/mcp",
        &[
            ("Authorization", "Bearer e2e-token"),
            ("Content-Type", "application/json"),
        ],
        &tool(2, "dashboard.get", json!({"name": "flight"})),
    );
    let v: Value = serde_json::from_str(&body).expect("json");
    let text = v["result"]["content"][0]["text"].as_str().expect("text");
    assert!(
        text.contains("alt-gauge") || text.contains("\"alt\""),
        "{text}"
    );
    server.shutdown();
}

/// M9d's wire budget: a dashboard-width subscription at 16 Hz must
/// stay inside §2.12's 256 KB/s — i.e. ≤ 16 KiB per frame at 16 Hz
/// (16 × 16 KiB = 256 KiB). The assertion is per-frame, so it's a
/// content-size check, not a timing flake.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ws_subscription_stays_within_byte_budget() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite;

    // A fixture world + a spawned rails vessel gives the projection
    // real vessel fields to carry.
    let dir =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/test-fixtures/flight-launch");
    let make = move || {
        let recipe = csp_bench::scenarios::find("flight-launch").expect("recipe");
        (recipe.initial)(&dir).expect("fixture world")
    };
    let (server, _host) = boot_with(make).await;
    mcp_init(server.addr, None);
    // Spawn a vessel so the vessel.* fields resolve to real data.
    let spawn = json!({"SpawnVessel": {
    "parent": "terrin",
    "elements": {
        "semi_major_axis_m": 700_000.0, "eccentricity": 0.0,
        "inclination_rad": 0.0, "ascending_node_rad": 0.0,
        "arg_periapsis_rad": 0.0, "mean_anomaly_epoch_rad": 0.0,
        "period_s": 1958.1284360312443
    }}});
    let (_, body) = http(
        server.addr,
        "POST",
        "/mcp",
        &[
            ("Authorization", "Bearer e2e-token"),
            ("Content-Type", "application/json"),
        ],
        &tool(1, "sim.command", json!({"command": spawn})),
    );
    assert!(body.contains("\"isError\":false"), "{body}");

    // The field set the shipped default dashboard subscribes —
    // apps/telemetry's default.json in mirrored form (the comment
    // documents the coupling).
    let fields = [
        "tick",
        "warp_level",
        "vessels.0.altitude_m",
        "vessels.0.speed_mps",
        "vessels.0.vertical_speed_mps",
        "vessels.0.horizontal_speed_mps",
        "vessels.0.apoapsis_altitude_m",
        "vessels.0.periapsis_altitude_m",
        "vessels.0.time_to_apoapsis_s",
        "vessels.0.throttle",
        "vessels.0.mass_kg",
        "vessels.0.residence",
        "vessels.0.pos_m",
        "vessels.0.vel_mps",
        "vessels.0.elements",
        "vessels.0.maneuver_node",
        "vessels.0.control_lease",
    ];
    let url = format!("ws://{}/ws?token=e2e-token&client=budget", server.addr);
    let (mut ws, _) = tokio_tungstenite::connect_async(url)
        .await
        .expect("ws connect");
    ws.send(tungstenite::Message::Text(
        json!({"proto": 1, "kind": "subscribe", "fields": fields, "rate_hz": 16})
            .to_string()
            .into(),
    ))
    .await
    .expect("subscribe");
    // Skip the subscribed ack; measure real telemetry frames.
    const PER_FRAME_BUDGET: usize = 16 * 1024; // 256 KiB/s ÷ 16 Hz
    let mut frames = 0;
    let mut max_frame = 0usize;
    for _ in 0..12 {
        let msg = ws.next().await.expect("frame").expect("ok");
        let text = msg.into_text().expect("text");
        let v: Value = serde_json::from_str(&text).expect("json");
        if v["kind"] == "telemetry" {
            frames += 1;
            max_frame = max_frame.max(text.len());
            // Every field resolved — no unknown_fields leaks.
            assert_eq!(v["unknown_fields"], json!([]));
        }
        if frames >= 3 {
            break;
        }
    }
    assert!(frames >= 3, "no telemetry frames arrived");
    assert!(
        max_frame <= PER_FRAME_BUDGET,
        "telemetry frame {max_frame} B over the 16 Hz × 16 KiB budget"
    );
    server.shutdown();
}

/// M9d's SPA-serving path end to end (§2.12.3): with a real `spa_dir`
/// the server serves the built `index.html` verbatim — token-free and
/// carrying no secret — serves `/assets/*` token-free for the
/// browser's `<script>`/`<link>` fetches, refuses traversal before it
/// reaches the fs, and keeps the Origin/Host allowlist on the exempt
/// path.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn spa_serving_is_token_free_but_allowlisted() {
    let (server, _host, spa) = boot_spa().await;

    // `/` returns the *built* index.html — not the placeholder —
    // verbatim: the token-free shell must never carry the secret.
    let (status, body) = http(server.addr, "GET", "/", &[], "");
    assert_eq!(status, 200);
    assert!(body.contains("SPA-SHELL-MARKER"), "{body}");
    assert!(!body.contains("Build the telemetry SPA"), "{body}");
    assert!(!body.contains("e2e-token"), "token leaked into /: {body}");
    assert!(!body.contains("csp-token"), "{body}");

    // `/assets/app.js` is served token-free — no Authorization header
    // — verbatim bytes out of `spa_dir/assets/`.
    let (status, body) = http(server.addr, "GET", "/assets/app.js", &[], "");
    assert_eq!(status, 200);
    assert_eq!(body, "const csp_marker = 1;\n");

    // Traversal is refused before the join ever happens — and the
    // target (`spa_dir/index.html`, which exists, so the rejection is
    // load-bearing) must not leak. Both the raw and the
    // percent-encoded `..` shapes.
    assert!(spa.join("index.html").exists());
    for path in ["/assets/../index.html", "/assets/%2e%2e/index.html"] {
        let (status, body) = http(server.addr, "GET", path, &[], "");
        assert_eq!(status, 400, "{path}");
        assert!(!body.contains("SPA-SHELL-MARKER"), "{path} leaked: {body}");
    }

    // Token-free ≠ anything-goes: an unknown asset 404s, and the
    // exemption is exactly `/` + `/assets/*` — the data surfaces keep
    // their bearer requirement.
    let (status, _) = http(server.addr, "GET", "/assets/nope.js", &[], "");
    assert_eq!(status, 404);
    let (status, _) = http(server.addr, "GET", "/api/telemetry", &[], "");
    assert_eq!(status, 401);

    // The allowlist covers the exempt path: a foreign Origin, or a
    // request asserting a foreign Host (the DNS-rebinding shape), is
    // 403 before the token check is even reached.
    let (status, _) = http(
        server.addr,
        "GET",
        "/assets/app.js",
        &[("Origin", "http://evil.example")],
        "",
    );
    assert_eq!(status, 403);
    let (status, _) = http_host(
        server.addr,
        "evil.example",
        "GET",
        "/assets/app.js",
        &[],
        "",
    );
    assert_eq!(status, 403);
    // And a localhost Origin passes — the allowlist, not a blanket ban.
    let (status, _) = http(
        server.addr,
        "GET",
        "/assets/app.js",
        &[("Origin", "http://localhost:9999")],
        "",
    );
    assert_eq!(status, 200);
    server.shutdown();
}

/// With `dashboard_dir` unset the `/api/dashboards` routes answer a
/// sane 503 — the store-absent path, distinct from an empty store
/// (a `[]` would lie: "no dashboards" ≠ "dashboards not configured").
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dashboards_unconfigured_returns_503() {
    let (server, _host) = boot().await; // dashboard_dir: None
    for (method, path, req_body) in [
        ("GET", "/api/dashboards", ""),
        ("GET", "/api/dashboards/x", ""),
        ("PUT", "/api/dashboards/x", "{}"),
    ] {
        let (status, body) = http(server.addr, method, path, &bearer(), req_body);
        assert_eq!(status, 503, "{method} {path}: {body}");
        assert!(body.contains("not configured"), "{method} {path}: {body}");
    }
    server.shutdown();
}

/// R19-F3: the allowlist derives from the bind — a wildcard
/// (`--bind-lan`) bind accepts the IP-literal authorities LAN clients
/// actually send, keeps the loopback names the wildcard covers, and
/// still refuses unverifiable *names* (the DNS-rebinding shape).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lan_bind_allowlist_accepts_ip_authorities() {
    let dir = scratch("csp-e2e-lan");
    let host = Arc::new(
        GameHost::spawn_full(World::new, dir, IngressQueue::new(), None).expect("game host"),
    );
    let backend: Arc<dyn ControlBackend + Send + Sync> = host.clone();
    let cfg = ServerConfig {
        bind: "0.0.0.0:0".parse().expect("bind"),
        token: Some("e2e-token".into()),
        auth_disabled: false,
        token_dir: None,
        dashboard_dir: None,
        spa_dir: None,
    };
    let server = Server::start(cfg, backend).await.expect("server binds");

    // A LAN browser addresses us literally — `Host: <ip>:<port>` must
    // not 403 the way a hardcoded localhost set would.
    let (status, _) = http_host(
        server.addr,
        "192.168.10.5:47900",
        "GET",
        "/api/telemetry",
        &bearer(),
        "",
    );
    assert_eq!(status, 200);
    // Loopback names still pass — the wildcard covers loopback too.
    let (status, _) = http_host(server.addr, "localhost:1", "GET", "/", &[], "");
    assert_eq!(status, 200);
    // Names still deny — `evil.example` rebound to this host is the
    // exact DNS-rebinding shape the guard exists for, and a LAN DNS
    // name is unverifiable (clients use the literal address).
    for host in ["evil.example", "desktop.lan", "localhost.evil.example"] {
        let (status, _) = http_host(server.addr, host, "GET", "/", &[], "");
        assert_eq!(status, 403, "{host}");
    }
    server.shutdown();
}

/// R19-F4: backend calls must ride the blocking pool, not runtime
/// workers. With a single worker thread, a `/mcp` call queued behind
/// a held game thread must not starve a `/api/telemetry` read — under
/// the old blocking-recv the one worker would have been parked.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn backend_calls_leave_workers_free() {
    let (server, host) = boot().await;
    mcp_init(server.addr, None);
    // Occupy the game thread — the shape of a long drain/session op.
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel::<()>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let held = {
        let host = host.clone();
        std::thread::spawn(move || {
            host.inspect(move |_| {
                let _ = entered_tx.send(());
                let _ = release_rx.recv();
                json!(null)
            });
        })
    };
    entered_rx.await.expect("game thread held");

    let addr = server.addr;
    let mcp_call = std::thread::spawn(move || {
        http(
            addr,
            "POST",
            "/mcp",
            &[
                ("Authorization", "Bearer e2e-token"),
                ("Content-Type", "application/json"),
            ],
            &tool(1, "session.snapshot", json!({})),
        )
    });
    // Let the request reach the handler — under the old code it parks
    // the single worker right about now.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    // The game thread is still held; a telemetry read (ArcSwap, no
    // backend call) must answer anyway — proof the worker is free.
    let (tele_tx, tele_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tele_tx.send(http(
            addr,
            "GET",
            "/api/telemetry",
            &[("Authorization", "Bearer e2e-token")],
            "",
        ));
    });
    let (status, _) = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio::task::spawn_blocking(move || tele_rx.recv()),
    )
    .await
    .expect("telemetry starved — the backend call parked the worker pool")
    .expect("join")
    .expect("recv");
    assert_eq!(status, 200);

    // Release the game thread: the queued backend call completes.
    let _ = release_tx.send(());
    let (status, _) = tokio::task::spawn_blocking(move || mcp_call.join().expect("mcp thread"))
        .await
        .expect("join");
    assert_eq!(status, 200);
    tokio::task::spawn_blocking(move || held.join().expect("held thread"))
        .await
        .expect("join");
    server.shutdown();
}
