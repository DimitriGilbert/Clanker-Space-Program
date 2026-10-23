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
    let host = Arc::new(GameHost::spawn_full(make, dir, queue, None));
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
    let host = Arc::new(GameHost::spawn_full(
        World::new,
        dir.clone(),
        IngressQueue::new(),
        None,
    ));
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
    let host = Arc::new(GameHost::spawn_full(
        World::new,
        dir.join("saves"),
        IngressQueue::new(),
        None,
    ));
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn server_boots_and_serves() {
    let (server, _host) = boot().await;
    // `/` is token-free by design — the token is injected into it.
    let (status, body) = http(server.addr, "GET", "/", &[], "");
    assert_eq!(status, 200);
    assert!(body.contains("csp-token"));
    // `/api/telemetry` needs the bearer.
    let (status, _) = http(server.addr, "GET", "/api/telemetry", &[], "");
    assert_eq!(status, 401);
    let (status, body) = http(server.addr, "GET", "/api/telemetry", &bearer(), "");
    assert_eq!(status, 200);
    assert!(body.contains("\"tick\""));
    server.shutdown();
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
    // the one-tick contract.
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
    let (server, _host) = boot_with_full(World::new, IngressQueue::with_limits(64, 2)).await;
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
    // Spawn a rails vessel as the player (Ui through sim.command? —
    // external clients stamp External; SpawnVessel is not
    // lease-gated, so sim.command works for the setup too).
    let spawn = json!({"SpawnVessel": {
    "parent": "terrin",
    "elements": {
        "semi_major_axis_m": 700_000.0, "eccentricity": 0.0,
        "inclination_rad": 0.0, "ascending_node_rad": 0.0,
        "arg_periapsis_rad": 0.0, "mean_anomaly_epoch_rad": 0.0,
        "period_s": 0.0
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dashboards_sidecar_api_and_tools() {
    let (server, _host, dir) = boot_dashboards().await;
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
    // Spawn a vessel so the vessel.* fields resolve to real data.
    let spawn = json!({"SpawnVessel": {
    "parent": "terrin",
    "elements": {
        "semi_major_axis_m": 700_000.0, "eccentricity": 0.0,
        "inclination_rad": 0.0, "ascending_node_rad": 0.0,
        "arg_periapsis_rad": 0.0, "mean_anomaly_epoch_rad": 0.0,
        "period_s": 0.0
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
/// the server injects the token into the built `index.html`, serves
/// `/assets/*` token-free for the browser's `<script>`/`<link>`
/// fetches, refuses traversal before it reaches the fs, and keeps the
/// Origin/Host allowlist on the exempt path.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn spa_serving_is_token_free_but_allowlisted() {
    let (server, _host, spa) = boot_spa().await;

    // `/` returns the *built* index.html — not the placeholder — with
    // the token injected before `</head>` (the injection proof).
    let (status, body) = http(server.addr, "GET", "/", &[], "");
    assert_eq!(status, 200);
    assert!(body.contains("SPA-SHELL-MARKER"), "{body}");
    assert!(!body.contains("Build the telemetry SPA"), "{body}");
    let meta = "<meta name=\"csp-token\" content=\"e2e-token\">";
    let injected = body.find(meta).expect("token meta injected");
    let head_end = body.find("</head>").expect("</head>");
    assert!(injected < head_end, "meta must land inside <head>: {body}");

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
