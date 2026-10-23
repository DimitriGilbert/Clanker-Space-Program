# Cluster C19 — Deep Review: `csp-server`

Scope: `crates/csp-server/src/{lib,backend,dashboard,http,ingress,mcp,serve,telemetry}.rs` +
`crates/csp-server/tests/e2e.rs`. All files read in full. One finding verified
empirically against the repo toolchain (rustc 1.97.1).

Findings are ordered by severity.

---

### [HIGH] Finding 1: `generate_token()` hangs forever on Unix — production server never binds

**File:** `crates/csp-server/src/serve.rs:180-184`

**Problem:** The token CSPRNG read is `std::fs::read("/dev/urandom")`.
`std::fs::read` calls `read_to_end`, which loops until `read(2)` returns 0.
`/dev/urandom` never returns EOF, so the read never completes — the buffer grows
until the process is killed or OOMs.

**Evidence:**
```rust
if let Ok(bytes) = std::fs::read("/dev/urandom")
    && bytes.len() >= 32
{
    return bytes[..32].iter().map(|b| format!("{b:02x}")).collect();
}
```
Verified: `rustc -O` on a 6-line `std::fs::read("/dev/urandom")` repro is killed
by `timeout 5` with no output (exit 124). The code path runs whenever
`config.token` is `None` — which is `ServerConfig::default().token`
(`serve.rs:54`), i.e. every production start. `Server::start` calls
`unwrap_or_else(generate_token)` at `serve.rs:103` **before** binding, so the
server deadlocks at startup. The e2e suite never exercises it — every test
passes `token: Some("e2e-token")`.

**Impact:** The shipped binary can never start on Unix with the default config.
This is a total-availability bug, not theoretical.

**Suggestion:** Use `std::fs::File::open("/dev/urandom")` + `read_exact(&mut
[0u8; 32])`, or the `getrandom`/`rand` crate. Add a test that actually invokes
`generate_token()` (e.g. `assert_eq!(generate_token().len(), 64)`) — it
currently has zero coverage, which is how this shipped.

---

### [HIGH] Finding 2: Bearer token is disclosed to any unauthenticated client via `GET /` — token model collapses to "anyone who can open a socket"

**File:** `crates/csp-server/src/http.rs:219-221` (token-free path),
`http.rs:256-280` (injection), `serve.rs:199-213` (0600 file)

**Problem:** `/` is exempt from the bearer check and the token is injected into
the served HTML as `<meta name="csp-token" content="...">`. The only gate on `/`
is the Origin/Host allowlist — and `Host` is a **client-asserted** header. A
non-browser client that omits `Origin` and sends `Host: localhost` (or the
loopback IP, which hyper/clients do automatically) passes the allowlist and
receives the token. Any local process — including processes owned by *other
users* on a multi-user machine — can `curl http://127.0.0.1:47900/` and read the
token from the response body, then fully authenticate to `/mcp`, `/ws`, and
`/api/*`.

**Evidence:**
```rust
// http.rs:219
let path = req.uri().path();
if path == "/" || path.starts_with("/assets/") {
    return next.run(req).await;
}
```
and the injection at `http.rs:261` / `http.rs:271`. Meanwhile the token file is
carefully written mode 0600 (`serve.rs:209`) — protection that `/` then gives
away to anyone with socket access.

**Impact:**
- On a shared/multi-user machine the 0600 file protection is moot: any local
  user gets the token over TCP and drives the sim (`sim.command` is a raw
  `SimCommand` passthrough, `session.load` replaces the world).
- Under `--bind-lan` (`ServerConfig::bind`), a *remote* attacker on the LAN just
  adds `Host: localhost` to its request (the allowlist cannot distinguish a
  self-asserted Host header), fetches the token from `/`, and gets
  unauthenticated remote control of the game. The allowlist stops
  *browser-driven* DNS rebinding only; it provides zero resistance to a
  deliberate client.

**Suggestion:** Do not serve the live token in a token-free page. Options:
require the SPA to read the token from the 0600 file via a launcher helper and
pass it explicitly; inject only into a page that itself required the token
(e.g. `/?setup=<one-time-bootstrap>` bootstrap URL printed to the console); or
at minimum treat serving the token in `/` as documented-accepted **and** drop
the 0600 claim in the header docs, since it misrepresents the actual secrecy
boundary. If `--bind-lan` is a real feature, it needs per-config origin
allowlists rather than a hardcoded localhost set (see Finding 3).

---

### [MEDIUM] Finding 3: Host/Origin allowlist both breaks and fails to protect `--bind-lan`

**File:** `crates/csp-server/src/http.rs:146-183`, `serve.rs:28-29`

**Problem:** The allowlist hardcodes `localhost`/`127.0.0.1`/`::1` and is
applied unconditionally. With `bind = 0.0.0.0` (the documented `--bind-lan`
choice, `serve.rs:29`), a legitimate LAN browser requests the page as
`http://192.168.x.y:47900/` — its `Host` and `Origin` authorities are the LAN
IP, so every request gets 403. Meanwhile a deliberate attacker bypasses the same
check trivially by asserting `Host: localhost` (Finding 2). The control fails
closed for the intended users and fails open for the adversary.

**Impact:** `--bind-lan` is non-functional for browsers and insecure for
non-browsers. If any shipping binary exposes the flag, it is broken both ways.

**Suggestion:** Condition the allowlist on the bind address (e.g.
`ServerConfig.bind.ip().is_loopback()` → localhost set; otherwise allow the
bound interface's authorities or a configured allowlist), and/or require the
bearer on `/` when not bound to loopback (which also fixes Finding 2 in the LAN
case).

---

### [MEDIUM] Finding 4: `GameHost::call` blocks a tokio worker thread on `std::sync::mpsc::recv()` inside async handlers

**File:** `crates/csp-server/src/backend.rs:692-702`; call sites reached from
`http.rs` `mcp_post` (line 443 via `Mcp::handle` → `submit`/`submit_batch`,
session tools) and `ws_session` (line 572).

**Problem:** Every `ControlBackend` mutation and every session verb on the axum
side does `rrx.recv()` — a *blocking* std-channel receive — on an async worker
thread. There is no `spawn_blocking`, no tokio oneshot, no timeout.

**Impact:**
- With `worker_threads = N` (default = cores), N concurrent MCP/WS callers doing
  submits or session ops park all workers; everything else (telemetry reads,
  WS ticks, new connections) starves behind them.
- A single expensive `drain_and_step` (up to 64 `step_frame` calls per submit —
  `backend.rs:281`, reachable via `timewarp.advance_to` with a huge tick, or
  `session.load` on a big save) serializes behind it *and* keeps a worker
  blocked for the duration. Repeated `advance_to` calls are an easy
  authenticated DoS of the whole HTTP surface.
- `inspect` (`backend.rs:677-690`) has the same blocking pattern (test surface).

**Suggestion:** Bridge with `tokio::sync::oneshot` for the reply (request over
`std::sync::mpsc` is fine) and `await` the reply, or wrap the call in
`tokio::task::spawn_blocking`. Consider a bounded timeout so a dead/hung game
thread returns 503 instead of an in-flight task hanging until TCP timeout.

---

### [MEDIUM] Finding 5: `HostCore::submit_batch` leaves a partial batch queued on error — failed commands apply on the next unrelated submit

**File:** `crates/csp-server/src/backend.rs:299-309`

**Problem:**
```rust
for c in commands {
    self.queue.push(client, c)?;   // early-returns on ClientBudget/QueueFull/TooLarge
}
let (_, rejected) = self.drain_and_step()?;
```
When `push` fails partway (e.g. command 33 hits the per-client budget, or any
command after the first is oversized), the `?` propagates the error but the
already-pushed prefix stays in `self.queue.pending`. `drain_and_step` is never
reached. The leftovers drain silently on the *next* `submit`/`pump` — possibly
triggered by a different client and a different drain window.

**Impact:** Atomicity violation + cross-request contamination. A client that
sends `sim.command {commands: […]}` oversized gets `ClientBudget` back yet the
first 32 commands still apply later — stamped under its `source_id` but at a
tick the client does not associate with that call. In `GameHost`, a subsequent
unrelated request from *any* client triggers the drain. The e2e
`bounded_ingress_returns_backpressure` asserts the error but never checks the
queue was emptied, so the leak is invisible to tests.

**Suggestion:** Pre-validate the batch before pushing (e.g. check
`commands.len() <= max_per_client` and per-command size up front, fail fast with
zero pushes), or drain whatever was pushed before returning the error so an
error never leaves silent residue.

---

### [MEDIUM] Finding 6: `vessel.control` submits each field as a separate drain+step — one tool call advances the clock multiple ticks

**File:** `crates/csp-server/src/mcp.rs:304-361`

**Problem:** The `submit` closure calls `self.backend.submit(client, cmd)` per
field (`steer_dir`, `attitude`, `hold_attitude`, `throttle`). Both shipped hosts
drain-and-step once per `submit` (`HostCore::submit_batch` → `drain_and_step`,
clock + 1). So a combined `vessel.control {steer_dir, throttle}` burns **two
sim ticks**, and the two commands land in different command-log boundaries — not
"coherently" in one, as the doc claims (`mcp.rs:302-303`). It also makes the
§2.2.1 per-drain budget trivially reachable-by-accident inside a single tool
call while `sim.command`'s `commands:` batch is the only path that lands
multiple commands on one boundary.

**Impact:** Determinism is preserved (everything is logged), but the API
contract misleads: `hold_attitude` reads the snapshot *after* steer/attitude
already stepped, so it "holds" an orientation a tick newer than the caller's
request context; and a client that thinks it's issuing an atomic control input
actually advances the world several ticks per call.

**Suggestion:** Collect the per-field `SimCommand`s and issue one
`submit_batch` so they drain in a single boundary.

---

### [LOW] Finding 7: Token file created with default permissions, then chmod'd — brief world-readable window; `/tmp` fallback dir is attacker-influenceable

**File:** `crates/csp-server/src/serve.rs:201-212`

**Problem:** `std::fs::write(&path, …)` creates `<pid>.token` under the process
umask (commonly 0644) and `set_permissions(0o600)` runs after — a small but real
window where another local user can read the token. Separately, the temp-dir
fallback (`runtime_dir()` → `/tmp/csp`, `serve.rs:65-69`) can be pre-created by
another user; `fs::write` follows a pre-planted symlink at `<pid>.token`
(pids are predictable). On Unix the subsequent chmod of the attacker-owned
target fails → startup DoS; on platforms where the chmod is skipped the token
lands in an attacker-controlled file.

**Suggestion:** Create with `OpenOptions::new().write(true).create_new(true)`
(+ `.mode(0o600)` on Unix) to get atomic perms and refusal on pre-existing
files/links. Prefer `0700` on the `csp` dir itself.

---

### [LOW] Finding 8: `timewarp.set` truncates `u64 → u32` (`level as u32`)

**File:** `crates/csp-server/src/mcp.rs:412-413`

**Problem:** `req_u64` accepts any non-negative integer; `level as u32` silently
wraps (`2^32 + 3` → `SetWarp(3)`). The declared schema (`{"type": "integer",
"minimum": 0}`) has no maximum, so the server itself is where a bound is needed.

**Suggestion:** `u32::try_from(level).map_err(|_| ToolFail::Invalid(…))`.

---

### [LOW] Finding 9: `ClientRegistry` grows unboundedly on attacker-controlled names

**File:** `crates/csp-server/src/backend.rs:160-168`; fed by `?client=` /
`X-CSP-Client` (`http.rs:432-441, 483`).

**Problem:** Every distinct client name inserts a permanent `BTreeMap` entry.
An authenticated (or, per Finding 2, unauthenticated-but-local) caller can POST
`/mcp?client=<random>` in a loop and grow server memory without bound; the
per-client budget ledger in `IngressQueue` also keys on the allocated id.

**Suggestion:** Cap registry size (refuse new names past N, reuse `default`), or
bound name length/count at the HTTP edge.

---

### [LOW] Finding 10: WS `max_frame_size` left at the tungstenite default while `max_message_size` is capped

**File:** `crates/csp-server/src/http.rs:484`

**Problem:** `ws.max_message_size(MAX_MESSAGE_BYTES)` caps the *assembled*
message at 64 KiB, but the per-frame cap stays at the default (16 MiB). A client
can send single frames far above the intended wire cap; they're buffered before
the message-size check trips.

**Suggestion:** Also set `.max_frame_size(MAX_MESSAGE_BYTES)`.

---

### [LOW] Finding 11: `project`/`field` re-serialize the entire snapshot once per field, per frame, per client

**File:** `crates/csp-server/src/telemetry.rs:77-91, 96-111`

**Problem:** `field()` calls `serde_json::to_value(self)` (full snapshot →
`Value`) and `project()` calls `field()` per declared path. At the caps — 64
fields, 64 Hz — each WS connection triggers up to 4,096 full snapshot
serializations/second, scaling with vessel count. Combined with no cap on WS
connections, this is an easy CPU-bound DoS for a token holder.

**Suggestion:** Serialize once per `project()` call (`let value =
serde_json::to_value(self)` then resolve every path against it) — same public
API, ~64× less work. Longer-term, cache the `Value` alongside the `ArcSwap`
payload.

---

### [LOW] Finding 12: Bearer check uses non-constant-time compare; `?token=` accepted on every endpoint

**File:** `crates/csp-server/src/http.rs:223-231, 240-246`

**Problem:** `t == token` short-circuits — a timing side channel (theoretical
over loopback, more relevant once `--bind-lan` exists). And `?token=`
authenticates `/mcp`, `/api/*` — anywhere, not just the WS upgrade that
justified it — putting the token into URLs that land in browser history, proxy
logs, and (via the SPA's own fetches) `Referer` leakage risk.

**Suggestion:** Constant-time compare (e.g. `subtle::ConstantTimeEq` or
length-equal + byte-xor fold). Restrict `?token=` to `/ws`.

---

### [LOW] Finding 13: `/assets/{*path}` traversal hole on Windows (`\` separators)

**File:** `crates/csp-server/src/http.rs:293-299`

**Problem:** The segment check splits on `'/'` only. `a\..\b` is a single
`/`-segment containing no `..` segment, passes the check, and
`dir.join("assets").join("a\\..\\b")` traverses on Windows where `\` is a
separator. (Also note assets follow symlinks — acceptable since `spa_dir` is
operator-controlled.)

**Suggestion:** Reject any segment containing `'\\'`, or canonicalize the joined
path and verify it stays under `dir.join("assets")`.

---

### [LOW] Finding 14: WS envelope asymmetries — silent `fields` truncation; `command` ignores `proto`

**File:** `crates/csp-server/src/http.rs:520-589`

**Problem:** `subscribe.fields` is truncated with `.take(MAX_FIELDS)` rather
than rejected — the client believes all its fields subscribed but past-64 paths
silently never stream (they don't even appear in `unknown_fields`). Separately,
`subscribe` enforces `proto == WIRE_PROTO` while `command` accepts any/absent
`proto` — inconsistent version gating of the same envelope. And `command_ack`
shares the telemetry `seq` counter, so `seq` jumps are ambiguous to clients.

**Suggestion:** Reject (or report) over-cap field lists explicitly; apply the
same `proto` check to `command`.

---

### [LOW] Finding 15: `drain_and_step` silently gives up after 64 partials; each submit can synchronously burn 64 frame budgets on the game thread

**File:** `crates/csp-server/src/backend.rs:270-294`

**Problem:** After 64 `partial` results the loop exits and returns `Ok` with the
clock short of the `AdvanceTo` target — no error, no flag. Also, the whole loop
runs synchronously inside the request (GameHost channel / LocalGame lock), so
`timewarp.advance_to {tick: u64::MAX}` monopolizes the game thread for a large
bounded-but-expensive run per call.

**Suggestion:** Return/report the shortfall (e.g. include `effective_target`
reached vs requested) so the client knows the target wasn't met; consider a
smaller cap or explicit refusal for extreme targets. The blocking aspect is
covered by Finding 4.

---

### [LOW] Finding 16: Blocking `std::fs` I/O in async handlers

**File:** `http.rs:259` (`index` reads `index.html` per request), `http.rs:300-308`
(`spa_asset` `metadata` + `read` up to 16 MiB), `dashboard_list`/`get`/`put`
(`http.rs:323-372` → `std::fs` in `DashboardStore`).

**Problem:** Synchronous filesystem calls on tokio worker threads. Bounded and
small today, but a 16 MiB asset read or a slow filesystem stalls a worker.

**Suggestion:** `tokio::fs` or `spawn_blocking`; for the SPA, cache
`index.html`/assets in memory at startup.

---

### [LOW] Finding 17: Dashboard store — TOCTOU on the count cap; non-atomic write

**File:** `crates/csp-server/src/dashboard.rs:226-233`

**Problem:** `!self.path(name).exists() && self.list()?.len() >= MAX_DASHBOARDS`
— two concurrent PUTs for new names both see len < 64 and both write, exceeding
the cap. And `std::fs::write` is not atomic: a crash mid-write leaves truncated
JSON that then fails `load` with a JSON error (surfaced as 500, not 404).

**Suggestion:** Write to `<name>.json.tmp` + `rename`; hold a `Mutex` (or accept
the cap is soft and document it).

---

### [LOW] Finding 18: No WS keepalive/idle timeout or connection cap

**File:** `crates/csp-server/src/http.rs:493-595`

**Problem:** A subscribed (or even un-subscribed — the loop runs a ticker
regardless) WS session lives until TCP fails; a client that stops reading stalls
`sink.send` indefinitely with no write timeout, and there's no bound on the
number of concurrent sessions each spawning a ticker task.

**Suggestion:** Periodic ping/expect-pong, a send timeout, and a connection
limit.

---

### [LOW] Finding 19: `spawn_full` panics on thread-spawn failure; mutex poisoning silently swallowed

**File:** `crates/csp-server/src/backend.rs:660-666` (panic),
`backend.rs:161, 476-477` (`unwrap_or_else(|p| p.into_inner())`).

**Problem:** A library that `panic!`s on `spawn` failure takes down the host
process instead of returning an error. The poisoned-mutex recovery on
`ClientRegistry`/`LocalGame` is deliberate-looking but means a panic mid-drain
(a `HostCore` left with a half-drained queue or mid-step world) is silently
continued-from — for a determinism-sensitive sim, continuing after poison is
questionable.

**Suggestion:** Return `io::Result`/an error from `spawn_full`; for the mutexes,
either propagate poison or document why continued use is safe.

---

### [LOW] Finding 20: Misc contract/doc gaps

- `serve.rs:199` doc says the token file contains `<token>\n<port>\n`; the write
  at `serve.rs:204` emits only `<token>\n` — the port line clients may look for
  is absent. Stale `<pid>.token` files are also never removed on `shutdown`.
- `Authorization` scheme match requires the exact string `"Bearer "`
  (`http.rs:244`); RFC 7235 makes the scheme case-insensitive (`bearer:` rejects
  some conforming clients).
- `mcp.rs`: no initialized-state machine — `tools/call` succeeds before
  `initialize`/`notifications/initialized`. Probably deliberate for a stateless
  transport, but worth a comment since it's a spec deviation.
- `vessel.control`: a non-object `attitude` (e.g. `{"attitude": 5}`) silently
  yields `IDENTITY` rotation instead of an Invalid-params error
  (`mcp.rs:327-334`).
- `IngressError`-leaking messages echo server filesystem paths (`{e}` on io
  errors) into client-visible strings — minor info exposure.
- `generate_token`'s non-Unix fallback (`serve.rs:187-196`) hashes pid+time —
  weak, but documented; noted for completeness alongside Finding 1 (which makes
  the fallback unreachable-but-also-broken).

---

## Not-findings (checked, deliberate, or spec-correct)

- **`?token=` on WS upgrade**: required since browser WS can't set headers —
  intended (though scope should be `/ws`-only; Finding 12).
- **Origin/Host parsing**: `rsplit('@')` handles userinfo, `[v6]` bracket
  handling is correct, `localhost.evil.example`/`127.0.0.1.evil.example`
  correctly denied, non-browser clients simply omit Origin — verified by the
  unit tests.
- **`save_name`/`check_name`**: `.`-prefixed names produce `.<x>.json` / cannot
  traverse because the extension is appended by the code, not the caller.
- **Dashboard `name` vs path `name` mismatch**: correctly rejected.
- **MCP 202/405 semantics, batch rejection, notification silence, per-request
  SSE**: match revision 2026-07-28.
- **`!Send` `World` boundary**: `World` is built inside the game thread,
  `Reconfigure`/`Inspect` closures are `Send`, replies carry `Send` types —
  clean.
- **ArcSwap publish ordering**: `GameHost` publishes before `reply.send`, so
  post-submit `telemetry()` reads see the post-command state.
- **Poisoned mutex recovery**: consistent `into_inner()` pattern — flagged in
  Finding 19 only as a judgment call, not a bug.
- **`sim.command` exposing raw `SimCommand`s**: documented escape hatch;
  `SpawnVessel`/`SetAlarm` etc. are sim-internal, no host-fs reach — fine under
  the token auth model (but see Finding 2).
