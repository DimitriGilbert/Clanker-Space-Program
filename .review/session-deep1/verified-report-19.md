# Cluster C19 — Verification Report: `csp-server`

Source of truth: `crates/csp-server/src/{lib,backend,dashboard,http,ingress,mcp,serve,telemetry}.rs`
+ `crates/csp-server/tests/e2e.rs` (note: report paths read `crates/csp-server/...`,
not `csp-server/src/...` — actual files live under `crates/csp-server/src/`).
All findings re-checked against the code as it stands. Two items verified
empirically (rustc 1.97.1 repro for Finding 1; `cargo test -p csp-server --lib`
— 18/18 pass — for the codebase's own claimed invariants).

Verdict summary: **19 findings CONFIRMED, 0 findings DISMISSED.**
One sub-item inside Finding 20 is dismissed/overstated (the "server filesystem
path echo" claim). Two confirmed findings carry a reachability caveat worth
recording (Findings 1 and 3: no shipped binary wires `Server::start` or
`--bind-lan` yet — the defects are real in code, latent in deployment).

---

### [HIGH] Finding 1: `generate_token()` hangs forever on Unix — CONFIRMED (empirically reproduced)

**Evidence:** `serve.rs:180-183` does `std::fs::read("/dev/urandom")`.
`std::fs::read` → `File::open` + `read_to_end`, which loops until `read(2)`
returns 0. `/dev/urandom` never EOFs.

**Empirical repro (this session):** compiled a 6-line `std::fs::read("/dev/urandom")`
repro with `rustc -O` (rustc 1.97.1); `timeout 5 ./repro` printed the pre-read
marker then was killed — `EXIT=124`, read never returned. Matches the report's
claimed verification.

**Reachability confirmed in code:**
- `ServerConfig::default().token` is `None` (`serve.rs:54`).
- `Server::start` calls `config.token.clone().unwrap_or_else(generate_token)`
  at `serve.rs:103`, **before** `TcpListener::bind` at `serve.rs:126` — startup
  deadlocks, never binds.
- Coverage gap confirmed: all three `ServerConfig` literals in `tests/e2e.rs`
  (lines 41, 68, 106) pass `token: Some("e2e-token")`. `generate_token` has zero
  test coverage.
- The non-Unix fallback (`serve.rs:186-197`) is unreachable in practice: on
  Unix the `fs::read` never returns `Err` — it never returns at all.

**Reachability caveat:** `grep` across the workspace shows no binary calls
`Server::start`. `csp-app` does not depend on `csp-server`; the `csp-mcp` stdio
bin (`crates/csp-bench/src/bin/csp-mcp.rs:27-28`) uses `LocalGame` + `Mcp` over
stdio. So "production server never binds" is currently latent — but the defect
is exactly as described and will hang the first real caller that uses the
default config. Severity as a code defect stands.

---

### [HIGH] Finding 2: Bearer token served to any unauthenticated client at `GET /` — CONFIRMED

**Evidence, all verified in code:**
- `http.rs:219-222`: `if path == "/" || path.starts_with("/assets/") { return
  next.run(req).await; }` — token check skipped.
- `http.rs:261-267`: token injected into `spa_dir/index.html` as
  `<meta name="csp-token" content="{token}">`.
- `http.rs:269-279`: the **placeholder** page (served when `spa_dir` is `None`,
  i.e. every configured default) embeds the same meta tag — so the token leaks
  even with no SPA built.
- `http.rs:194-213`: the only gates ahead of the exemption are the Origin and
  Host allowlists. `Origin` is simply absent for non-browser clients; `Host`
  is client-asserted and `localhost`/`127.0.0.1`/`[::1]` authorities pass
  (`http.rs:146-183`, verified by the file's own unit tests at `http.rs:608-652`,
  which pass).
- `serve.rs:204-209`: the token file is written then chmod'd 0600 — the
  report's point that `/` gives away what the file mode protects is accurate.

**Wire-level proof already in the repo:** `tests/e2e.rs:170-173`
(`server_boots_and_serves`) asserts `GET /` with **no auth headers** returns
200 and `body.contains("csp-token")`. The disclosure is not theoretical — it is
asserted-by-test behaviour.

**Consequences confirmed:** `sim.command` is a raw `SimCommand` passthrough
(`mcp.rs:430-454`); `session.load` replaces the running world
(`backend.rs:353-374`). Any local process — including other users on a
multi-user box — that opens a socket to `127.0.0.1:47900` gets the token and
full control. The module doc's rationale ("the only token-free page, so the
injection can't leak", `http.rs:254-255`) is wrong: the page is fetchable by
anything that can open a connection. Design intent notwithstanding, the
finding's mechanism and impact are accurate.

---

### [MEDIUM] Finding 3: Host/Origin allowlist both breaks and fails to protect `--bind-lan` — CONFIRMED (with reachability caveat)

`localhost_name`/`origin_allowed`/`host_allowed` hardcode the localhost set
(`http.rs:146-183`) and `guard` applies them unconditionally to every request
including `/` (`http.rs:205-213`). With `bind = 0.0.0.0`, a LAN browser sends
`Host: 192.168.x.y:47900` → 403; an attacker sends `Host: localhost` → passes.
Fails closed for intended users, open for adversaries — accurate.

**Caveat:** `--bind-lan` exists only in doc comments (`serve.rs:4`, `28-29`);
no binary in the workspace sets `bind: 0.0.0.0`. The code behaviour is as
described; deployment impact is latent pending a binary that exposes it.

---

### [MEDIUM] Finding 4: `GameHost::call` blocks a tokio worker on `mpsc::recv()` — CONFIRMED

`backend.rs:700`: `rrx.recv()` — a blocking std-channel receive — inside
`GameHost::call`, with no `spawn_blocking`, no timeout. Reachability verified:

- `http.rs:443`: `mcp_post` (async handler) calls `state.mcp.handle(client,
  &msg)` **synchronously** → `run_tool` → `self.backend.submit` /
  `submit_batch` / session verbs → `ControlBackend for GameHost` → `self.call`
  (`backend.rs:723-754`) → `rrx.recv()`.
- `http.rs:572-575`: `ws_session` calls `state.backend.submit(client, c)` in
  the async `command` arm → same blocking path.
- `backend.rs:677-690`: `inspect` uses the identical `rrx.recv()` pattern.

Impact claims verified: `drain_and_step` (`backend.rs:270-294`) runs up to 64
`step_frame` calls synchronously on the game thread per submit; an
`AdvanceTo(u64::MAX)`-class target or a `session.load` keeps both the game
thread and the parked worker busy for the duration. N concurrent blocking
calls park N workers; everything else starves. The blocking-recv DoS is real.

---

### [MEDIUM] Finding 5: `submit_batch` leaves a partial batch queued on error — CONFIRMED

`backend.rs:304-306`:
```rust
for c in commands {
    self.queue.push(client, c)?;   // early-returns; pushed prefix stays in pending
}
let (_, rejected) = self.drain_and_step()?;
```
On a mid-loop `ClientBudget`/`QueueFull`/`TooLarge`, the already-pushed prefix
remains in `self.queue.pending`; `drain_and_step` is never reached; the residue
drains on the next submit/pump from any client (`backend.rs:270-271` drains
whatever is pending). Applies identically to `LocalGame` (same `HostCore` path,
`backend.rs:495-507`) and `GameHost` (`backend.rs:623`).

Test-gap claim verified: `tests/e2e.rs:312-330`
(`bounded_ingress_returns_backpressure`) asserts `isError` + `"budget"` text
only — the residual queue is never inspected. A follow-up submit would
silently apply the first 2 commands of the failed batch. Confirmed.

---

### [MEDIUM] Finding 6: `vessel.control` submits each field as a separate drain+step — CONFIRMED

`mcp.rs:308-314`: the `submit` closure calls `self.backend.submit(client,
cmd)` per field — `steer_dir` (315-326), `attitude` (327-335),
`hold_attitude` (336-350), `throttle` (351-353). Both hosts' `submit` →
`submit_batch(vec![cmd])` → `drain_and_step` → clock +1 per call
(`backend.rs:491-507`, `719-733`, `299-309`). A combined
`vessel.control{steer_dir, throttle}` burns two ticks and lands in two
command-log boundaries — contradicting the doc at `mcp.rs:302-303`
("a combined call lands coherently"). `hold_attitude` reads the published
snapshot *after* earlier submits have stepped (`mcp.rs:339-344` — the
snapshot is republished after each drain, `backend.rs:504/624`). Confirmed.

---

### [LOW] Finding 7: Token file perms window + `/tmp` fallback dir — CONFIRMED

`serve.rs:204` `std::fs::write` creates `<pid>.token` under the process umask;
`serve.rs:209` `set_permissions(0o600)` runs after — a real (if small)
world-readable window. `runtime_dir()` (`serve.rs:65-70`) falls back to
`temp_dir()/csp` — pre-creatable by another user; `create_dir_all` accepts an
existing foreign-owned dir, pids are predictable, `fs::write` follows a
pre-planted symlink, and the subsequent chmod then either fails on the
attacker-owned target (startup DoS — confirmed: `set_permissions` error maps
to `ServeError::TokenFile`) or lands the token in an attacker file where
chmod is skipped. Confirmed.

---

### [LOW] Finding 8: `timewarp.set` truncates `u64 → u32` — CONFIRMED

`mcp.rs:413`: `SimCommand::SetWarp(level as u32)`; `level` comes from
`req_u64` (`mcp.rs:564-566`, any non-negative integer). Schema `u()` at
`mcp.rs:622` is `{"type":"integer","minimum":0}` — no maximum. `2^32+3` →
`SetWarp(3)` silently. Confirmed.

---

### [LOW] Finding 9: `ClientRegistry` grows unboundedly on attacker-controlled names — CONFIRMED

`backend.rs:160-168`: `id()` inserts `name.to_string()` into a `BTreeMap` with
no cap, no length bound, permanent for the process. Names arrive from
`?client=` query or `X-CSP-Client` header (`http.rs:432-441`, `483`) — fully
client-controlled for any caller that passes the guard. The `IngressQueue`
per-client ledger keys on the allocated id (`ingress.rs:95,152`). Confirmed.

---

### [LOW] Finding 10: WS `max_frame_size` left at default — CONFIRMED

`http.rs:484` sets only `.max_message_size(MAX_MESSAGE_BYTES)` — `max_frame_size`
is never set anywhere in the crate (grep). axum 0.8.9 → tungstenite default
frame cap = 16 MiB vs. the 64 KiB intended wire bound; oversized single frames
are buffered before the message cap trips. Confirmed.

---

### [LOW] Finding 11: `project`/`field` re-serialize the snapshot per field — CONFIRMED

`telemetry.rs:78`: `field()` calls `serde_json::to_value(self)` — full
snapshot → `Value`. `telemetry.rs:103`: `project()` calls `field()` once per
declared path. At the caps (64 fields, `VALID_RATES` up to 64 Hz,
`http.rs:78-81`), each WS connection drives up to ~4,096 full serializations/s,
and WS connection count is unbounded (Finding 18). The fix (serialize once per
`project()`) is trivially available. Confirmed.

---

### [LOW] Finding 12: Non-constant-time bearer compare; `?token=` on every endpoint — CONFIRMED

`http.rs:240-246`: `strip_prefix("Bearer ")` then `t == token` —
short-circuiting compare. `http.rs:223-227`: the `?token=` query check applies
to every guarded path (`/mcp`, `/api/*`, `/ws`) — only `/` and `/assets/` are
exempt upstream, not `/ws`-scoped as intended. Confirmed.

---

### [LOW] Finding 13: `/assets/{*path}` traversal on Windows — CONFIRMED (Windows-only)

`http.rs:293-298`: the check splits on `'/'` only —
`path.split('/').any(|seg| seg == ".." || seg.is_empty())`. A segment like
`a\..\b` (a single `/`-segment) passes; `dir.join("assets").join("a\\..\\b")`
treats `\` as a separator on Windows and traverses — e.g. `..\..\x` escapes
`assets/` (and `spa_dir` with enough levels). Verified: crate is "NATIVE
ONLY" (`lib.rs:2`), so the Windows case is in-scope. Unix unaffected.
Confirmed.

---

### [LOW] Finding 14: WS envelope asymmetries — CONFIRMED

- `http.rs:535`: `.take(MAX_FIELDS)` silently truncates `subscribe.fields` —
  past-64 paths never stream and never appear in `unknown_fields`
  (`http.rs:505-510` reports unknowns only from `project`, which sees only the
  truncated list). Confirmed.
- `http.rs:521-528`: `subscribe` enforces `proto == WIRE_PROTO`; the
  `command` arm (`http.rs:567-589`) performs no proto check. Confirmed.
- `http.rs:580,583`: `command_ack` shares the telemetry `seq` counter.
  Confirmed.

---

### [LOW] Finding 15: `drain_and_step` silently gives up after 64 partials — CONFIRMED

`backend.rs:281-292`: `for _ in 0..64 { ... if !r.partial { break; } target =
r.effective_target; }` — if all 64 iterations return `partial`, the loop ends
and `Ok((applied, rejected))` is returned with the clock short of the target
and no indication. The whole loop runs inside the request path
(`backend.rs:623` on the game thread; `backend.rs:460-466` under the
`LocalGame` lock). The `AdvanceTo(u64::MAX)` monopolization claim is accurate
per `MAX_TICKS_PER_FRAME = 69_120` (`csp-sim/src/lib.rs:214`). Confirmed.

---

### [LOW] Finding 16: Blocking `std::fs` I/O in async handlers — CONFIRMED

- `http.rs:259`: `std::fs::read_to_string` in `index` (per request).
- `http.rs:300-308`: `std::fs::metadata` + `std::fs::read` (≤16 MiB) in
  `spa_asset`.
- `dashboard.rs:183,204,231-233`: `read_dir`/`read`/`create_dir_all`/`write`
  in `DashboardStore`, reached from `dashboard_list`/`get`/`put`
  (`http.rs:323-372`).
All synchronous on tokio workers. Confirmed.

---

### [LOW] Finding 17: Dashboard store TOCTOU + non-atomic write — CONFIRMED

`dashboard.rs:226`: `!self.path(name).exists() && self.list()?.len() >=
MAX_DASHBOARDS` — a check-then-act pair with no lock; two concurrent PUTs for
distinct new names can both pass under the cap and both write (64→65+).
`dashboard.rs:233`: `std::fs::write` truncates in place — a crash mid-write
leaves torn JSON that `load` then surfaces as a 500 (`http.rs:345-346`).
Confirmed.

---

### [LOW] Finding 18: No WS keepalive/idle timeout or connection cap — CONFIRMED

`http.rs:493-595` (`ws_session`): no ping/pong anywhere; `sink.send(...).await`
at line 512 has no timeout — a client that stops reading stalls the session's
whole `select!` loop on the send; the ticker runs regardless of subscription
state (line 503 `continue`s only the payload, not the loop). `router()`
(`http.rs:123-138`) imposes no connection limit. Confirmed.

---

### [LOW] Finding 19: `spawn_full` panics on spawn failure; poison swallowed — CONFIRMED

- `backend.rs:660-666`: `panic!("csp-game thread spawn failed: {e}")` in a
  library — kills the host process rather than returning an error.
- `backend.rs:161,477,708`: `unwrap_or_else(|p| p.into_inner())` recovers
  poisoned mutexes on `ClientRegistry`, `LocalGame::inner`, and the join
  handle — continuing after a mid-drain panic with a half-drained
  `IngressQueue`/mid-step `World`. Confirmed (the recovery may be deliberate,
  but the report only asks that it be justified or propagated).

---

### [LOW] Finding 20: Misc contract/doc gaps — CONFIRMED except one sub-item

- **Doc/code mismatch on token file contents — CONFIRMED.** `serve.rs:199-200`
  says "contents `<token>\n<port>\n`"; `serve.rs:204` writes
  `format!("{token}\n")` — no port line.
- **Stale token files never removed — CONFIRMED.** `Server::shutdown`
  (`serve.rs:146-148`) only aborts the task.
- **`Bearer ` case-sensitive — CONFIRMED.** `http.rs:244`
  `strip_prefix("Bearer ")` requires the exact capitalized prefix.
- **No initialized-state machine — CONFIRMED.** `handle()` (`mcp.rs:119-156`)
  dispatches `tools/call` regardless of any prior `initialize`; the file's
  own `tool_call_round_trip` test (`mcp.rs:840-861`) calls a tool with no
  initialize handshake and it succeeds.
- **Non-object `attitude` → IDENTITY — CONFIRMED.** `mcp.rs:327-334`: on
  `{"attitude": 5}`, `get_quat(att, "rot")` hits the `None` arm (`get` on a
  non-object yields `None` → `Ok(None)` → `unwrap_or(IDENTITY)`); same for
  `ang_vel` → a `SetAttitude` with identity rotation is submitted instead of
  an Invalid-params error.
- **Server-path echo via `{e}` — DISMISSED (overstated).** `io::Error`'s
  `Display` does not embed the filesystem path — `format!("load {name}: {e}")`
  and `format!("save dir: {e}")` (`backend.rs:320,356,400`) echo the
  *client-supplied* name plus the OS error string ("No such file or directory
  (os error 2)"), not server paths. No server-path info leak exists here;
  this sub-item is dismissed.
- **Non-Unix fallback weak — CONFIRMED** (pid+nanos+stack-addr hash,
  `serve.rs:186-197`; also unreachable on Unix due to Finding 1's hang).

---

## Report-level verdicts on the headline questions

- **Does `generate_token` really hang on `fs::read("/dev/urandom")`?**
  **Yes — reproduced.** `read_to_end` never sees EOF; `timeout 5` killed the
  repro (exit 124). On the default `ServerConfig` path, `Server::start`
  deadlocks before `bind`. (Caveat: no shipped binary calls `Server::start`
  yet.)
- **Is the bearer token really served unauthenticated at `GET /`?**
  **Yes.** The `/` + `/assets/` exemption skips the bearer check; the only
  gates are client-asserted `Host`/omittable `Origin`; the placeholder page
  itself embeds the token. The repo's own e2e asserts it.
- **Is the blocking-recv DoS real?** **Yes.** `rrx.recv()` on a tokio worker
  in both the `/mcp` and `/ws` command paths; no `spawn_blocking`/timeout;
  N concurrent calls park N workers, and expensive drains (`advance_to`,
  `session.load`) hold them for the duration.
- **Is partial-batch atomicity violated?** **Yes.** `push` errors leave the
  already-pushed prefix in `queue.pending`; it drains silently on the next
  unrelated submit. The e2e that exercises the error path never checks the
  residue.

## Verification performed

- Read in full: `serve.rs` (213 lines), `http.rs` (653), `backend.rs` (785),
  `mcp.rs` (875), `telemetry.rs` (129), `ingress.rs` (241), `dashboard.rs`
  (449), `lib.rs` (45), `tests/e2e.rs` (relevant sections + all
  `ServerConfig`/`token:` sites).
- Empirical: `rustc -O` repro of `std::fs::read("/dev/urandom")` → hang
  confirmed (timeout, exit 124); `cargo test -p csp-server --lib` → 18/18
  pass; grep for `Server::start`/`--bind-lan`/`max_frame_size`/`token:` across
  the workspace to establish reachability.
