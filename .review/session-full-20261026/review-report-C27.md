# Review report C27 — csp-server: HTTP/WS/MCP (session-full-20261026)

Scope: `crates/csp-server/src/{backend,mcp,http,dashboard,serve,ingress,telemetry,lib}.rs`,
`tests/e2e.rs`, `PROTOCOL.md`, plus `csp-app`'s `--serve` wiring for context.
All 22 e2e tests + 31 lib unit tests pass (`cargo test -p csp-server`). PRD: PLAN §2.12
(normative), §6.4 cut list. Parallel verified findings (not re-derived): `--serve` is
headless-only despite `backend.rs:823-825`'s claim; hosted world starts unseeded; saves-dir
divergence (`--serve` config dir vs `<cwd>/saves`).

The security core is solid: constant-time token compare, Origin/Host allowlist derived from
the bind on every endpoint (incl. token-free `/` + `/assets/*`), 0600/0700 token file with
symlink/pre-plant refusal, `?token=` scoped to `/ws` only, 64 KiB body/frame/message caps,
bounded ingress (256 deep, 32/client/drain), 16-connection WS cap with ping/idle/send
timeouts, traversal-proof asset gate, `safe_name`/`check_name` path-safe save/dashboard
names, capped save/dashboard stores. `sim.command` reaches every `SimCommand` but everything
funnels through `World::enqueue` → apply-time validation — no sim-side validation is
bypassed. No token leakage in logs or served pages (e2e asserts it).

### [HIGH] Finding 1: `--serve` ships with zero flag surface — `--port` and `--bind-lan` (PLAN §2.12.3, normative) are missing, and unknown flags are silently ignored
**File**: crates/csp-app/src/main.rs:16, crates/csp-app/src/lib.rs:2996-2999 (also crates/csp-server/src/serve.rs:29-31, 118-128)
**Problem / Evidence / Impact / Suggestion** — PLAN §2.12.3: "Default bind `127.0.0.1` on a fixed default port (`--port` to override)" and "LAN binds require an explicit `--bind-lan` flag, print a warning, and require the token on every endpoint." `serve_cli` builds `ServerConfig { spa_dir: Some(..), ..Default::default() }` and parses nothing; `main.rs` merely checks `args().any(|a| a == "--serve")`, so `csp --serve --port 5000` / `--bind-lan` launch with the defaults and the user's flags are silently dropped. The server-side machinery is fully coded and tested — `BindAllowlist::Wildcard` (http.rs:234-241), the LAN e2e (`e2e.rs:1181-1219`), and `ServerConfig::bind` — i.e. coded but not wired; §6.4 does not sanction the cut (DATA-AND-SCRIPTING §8.2 documents the `--bind-lan` absence, which is not PLAN sanction). Impact: no LAN serving at all, no second instance, and — worst — a silently ignored `--port` that looks honored. Fix: parse `--port <u16>` / `--bind-lan` in `serve_cli` (bind `0.0.0.0`, eprintln the PLAN warning), and reject unknown `--serve`-mode args instead of ignoring them.

### [MEDIUM] Finding 2: `session.save` writes non-atomically, violating the project's own atomic-rename rule that the Game menu and the dashboard store follow
**File**: crates/csp-server/src/backend.rs:607
**Problem / Evidence / Impact / Suggestion** — `HostCore::store_save` ends in `std::fs::write(self.save_path(name), bytes)` — a bare, in-place write. Contrast the windowed app's save path (csp-app/src/lib.rs:1135-1139: "The atomic-rename rule — a killed writer never leaves a half file", temp + `rename`) and `DashboardStore::save` (dashboard.rs:250-254, temp + rename under a write lock). `serve_cli` deliberately shares the game's saves dir and overwriting existing names is the intended flow (cap binds new entries only, backend.rs:569), so a kill/crash/disk-full mid-write over a previously good save destroys it, and `list_saves`' metadata-only index (backend.rs:484) will keep listing the torn file until `load` fails at decode. Fix: extract one temp+rename helper and use it here too (three save writers, two styles — also a DRY smell).

### [MEDIUM] Finding 3: quaternion wire order for `vessel.control` is undocumented, asymmetric with telemetry output, and the code comment justifying it is wrong
**File**: crates/csp-server/src/mcp.rs:720-733 (comment at :728), tools_v1 at :769
**Problem / Evidence / Impact / Suggestion** — `get_quat` accepts `attitude.rot` as `[w, x, y, z]`; the comment claims "DQuat's serde order is (w, x, y, z)", which is false — `DQuat` is a serde struct `{x, y, z, w}` (csp-core/src/quat.rs:33-39), so everything the server *emits* (`telemetry.get`'s `rot`, mission payloads) is an `{x,y,z,w}` object while the *input* is a `w`-first array. The tool schema declares `rot: v4()` with no order, and DATA-AND-SCRIPTING's tool table doesn't state one. An agent doing the natural round trip (read `rot` object → feed `[x,y,z,w]`) gets a `w↔x`-permuted quaternion; permutation preserves the norm, so the unit-ness gate passes and the wrong attitude applies silently. No test pins the order. Fix: state `[w,x,y,z]` in the tool description and DATA-AND-SCRIPTING, correct the comment, and add a round-trip test echoing a telemetry `rot` through `vessel.control`.

### [MEDIUM] Finding 4: `--serve` with a broken data dir panics the game thread and leaves a zombie server serving a stale empty snapshot
**File**: crates/csp-app/src/lib.rs:2981-2984 (with crates/csp-server/src/backend.rs:856-865)
**Problem / Evidence / Impact / Suggestion** — the `GameHost::spawn_full` world builder runs `panic!("data/ startup load failed: {e}")` on the game thread. When it panics, the thread dies before the serve loop: the pre-spawn `World::new()` snapshot published at backend.rs:856-865 stays live forever, `run_blocking` has already printed "csp-server: listening on http://127.0.0.1:47900", `/` and `/api/telemetry` keep 200-ing an empty tick-0 world, and every mutation answers "game thread gone" until the user notices and Ctrl-Cs. A startup failure never produces a non-zero exit. Fix: have the builder signal failure back (e.g. a `Result`-carrying handshake channel, or set a dead flag the `call` path checks at startup) and make `run_blocking` exit with an error instead of serving a corpse.

### [LOW] Finding 5: the MCP read surface exposes counts, not ids — the worked example's `ManufactureCraft` launch step is undiscoverable over the protocol
**File**: crates/csp-server/src/telemetry.rs:72, :173
**Problem / Evidence / Impact / Suggestion** — `ProgramSnapshot.sites` is a `usize` (likewise `contract_offers`/`contracts_active`). DATA-AND-SCRIPTING §5.8's launch step needs `ManufactureCraft {site: SiteId, craft: CraftDef, build_resources}` (csp-sim lib.rs:1025) — no MCP tool or resource exposes site ids or craft/part definitions, so an agent must mine `data/*.ron` offline and guess ids. With the fresh `--serve` world having no vessels (parallel verified finding), the "drive a full loop via MCP" story holds end-to-end only for an agent that already knows the repo. Fix (additive, tools.v1-compatible): widen `ProgramSnapshot` with site/contract identity rows, or add a `csp://data/…` resource family.

### [LOW] Finding 6: DATA-AND-SCRIPTING's claim that "every step" of the worked example is exercised in e2e.rs overstates
**File**: docs/DATA-AND-SCRIPTING.md:748
**Problem / Evidence / Impact / Suggestion** — e2e covers connect (initialize/ordering), launch (`SpawnVessel`), lease handshake, advance, and hash parity, but the science verbs (`RunExperiment`/`TransmitData`/`RecoverData`) and `session.save`/`session.load` over the wire are absent (the save round-trip is unit-tested in backend.rs tests only, and `ManufactureCraft` is driven nowhere). PROTOCOL.md itself checked out against the code on every claim I verified (revisions, 405s, `-32600` gate, caps, liveness constants, token-file contract). Fix the sentence to name what e2e actually covers.

### [LOW] Finding 7: stale in-tree comments claim the SPA is not wired into any binary
**File**: crates/csp-server/src/http.rs:52-56, crates/csp-server/tests/e2e.rs:78-79
**Problem / Evidence / Impact / Suggestion** — both say `spa_dir` "has no binary wiring yet" / "`/` serves the placeholder in production today", but `serve_cli` sets `spa_dir: Some(packaged_spa_dir())` (csp-app/src/lib.rs:2995-2997) and `apps/telemetry/dist/client/index.html` exists. DATA-AND-SCRIPTING §8.8 already declares the comment stale ("the tree wins") — delete the stale half of the http.rs deferral note so the security-relevant doc block is accurate.

### [LOW] Finding 8: every WS telemetry frame re-serializes the entire snapshot, per client, per tick
**File**: crates/csp-server/src/telemetry.rs:216-237 (and :190-192 for the diagnostic `field()`)
**Problem / Evidence / Impact / Suggestion** — `project` pays one `serde_json::to_value` of the whole snapshot per call; the WS loop calls it per client per tick — 16 clients × 16 Hz = 256 full-snapshot serializations/s even when every client subscribes the same default dashboard. Fine at current snapshot sizes (the budget e2e measures KB-sized frames), but it scales with vessel count for no reason. Fix: derive the serialized root once per published `Arc<TelemetrySnapshot>` (e.g. cache it alongside the ArcSwap payload) so N clients × M ticks pay one serialize per publish.

Follow-ups: WS and MCP clients share one `ClientRegistry` (http.rs:625/:703), so a same-named pair shares an ingress budget and MCP handshake record — documented as diagnostic-tag semantics (PROTOCOL.md:24-28); `BindAllowlist::Wildcard` accepts any IP-literal Host including addresses that are not bound (http.rs:264) — no practical bypass since a browser-set Host/Origin requires controlling that address, and non-browser callers need the token; `BACKEND_CALL_TIMEOUT` (30 s) < `GameHost::REPLY_TIMEOUT` (120 s), so a 503'd call's drain still lands server-side — a retrying client can double-advance (`AdvanceTo` is idempotent-ish, other commands are not).

## PRD coverage table (PLAN §2.12)

| §2.12 requirement | Status | Evidence |
|---|---|---|
| Revision `2026-07-28` target, `2025-11-25` accepted, never 2024-11-05 | **WIRED** | mcp.rs:43-48, pin test :903, PROTOCOL.md |
| Origin/Host allowlist on every endpoint incl. read-only | **WIRED** | http.rs:315-339; e2e origin/host/lan tests |
| Bind `127.0.0.1` by default | **WIRED** | serve.rs:54 |
| `--port` to override | **MISSING** | main.rs:16, serve_cli defaults only (Finding 1) |
| `--bind-lan` flag + warning + token everywhere | **MISSING** (machinery WIRED + e2e-tested) | http.rs:234-241, e2e.rs:1181 (Finding 1) |
| Token `<pid>.token` 0600, dir 0700, rotated, removed on shutdown, never in save/shell | **WIRED** | serve.rs:260-325; e2e lifecycle + symlink-refusal tests |
| Bearer everywhere; `?token=` only on `/ws` | **WIRED** | http.rs:348-366; e2e :273 |
| Every sim tool → SimCommand, `source_class=External`, one-tick latency, control lease | **WIRED** | ingress.rs, backend drain; e2e round-trip + lease tests |
| Session-class tools declared `"class": "session"` | **WIRED** | mcp.rs:741-742, tools_list test |
| HTTP = attach-to-a-running-game | **MISSING** (fresh headless world; windowed app hosts nothing) | serve_cli; parallel verified finding |
| stdio `csp-mcp` with own headless sim | **WIRED** | csp-bench/src/bin/csp-mcp.rs |
| `craft.load`/`editor.build` (and `camera.*`/`map.focus`) named tools | **MISSING** (sim-side reachable via `sim.command`; editor is not a `SimCommand` at all) | mcp.rs:749-859; DATA-AND-SCRIPTING §8.1 |
| Wire envelope `{proto,kind,seq,tick,payload}`; ≤64 fields; rates 64..1; no send-everything | **WIRED** | http.rs:86-92, ws_session; e2e envelope/cap tests |
| Backpressure drop-to-latest; ArcSwap reads never block | **WIRED** | http.rs:738-755, WS_SEND_TIMEOUT |
| MessagePack for arrays | **DEGRADED** (JSON-only; documented deferral + budget tripwire; not on §6.4 cut list) | http.rs:30-40 |
| Dashboards: sidecar `*.json`, versioned, never in save/log/hash | **WIRED** | dashboard.rs |
| Dashboards "mutated via SessionCommand" | **DEGRADED** (direct store write; the actual invariant — never in save/log/hash — holds) | http.rs:41-51 |
| 64 KiB message caps (body/frame/ingress), ≤32 cmds/client/drain, bounded queue | **WIRED** | ingress.rs, http.rs:210/:704-705 |
| Lockfile for multiple instances + discovery | **PARTIAL** (per-pid token file = discovery; instance collision surfaces as bind failure) | serve.rs:143-146 |
| WS liveness (30 s ping / 90 s idle), ≤16 connections | **WIRED** | http.rs:104-119; e2e cap test |

## Top 3 issues
1. Finding 1 (HIGH) — `--serve` ignores all flags: PLAN-normative `--port`/`--bind-lan` unwired, unknown flags silently dropped.
2. Finding 2 (MEDIUM) — `session.save` bypasses the project's atomic-rename rule; a killed overwrite can destroy a good shared save.
3. Finding 3 (MEDIUM) — undocumented `[w,x,y,z]` quaternion order on `vessel.control` + false comment + object-shaped telemetry output = silently wrong attitudes for agents.
