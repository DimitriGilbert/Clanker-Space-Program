# Cluster 17 Review — HTTP/WS/MCP telemetry server (csp-server)

Scope: mcp.rs, http.rs, backend.rs, dashboard.rs, serve.rs, ingress.rs, telemetry.rs, lib.rs, tests/e2e.rs, PROTOCOL.md, Cargo.toml
Lens: hostile local network peer — security first.
Status: COMPLETE (3 findings: 1 MEDIUM, 2 LOW)


### [SEVERITY: MEDIUM] Finding 1: `session.save` is uncapped and `list_saves` full-decodes every save — authenticated disk-fill + game-thread stall amplifier
**File**: /home/didi/workspace/Clanker-Space-Program/crates/csp-server/src/backend.rs:[458] (store_save), :[397-433] (list_saves), contrast crates/csp-server/src/dashboard.rs:[243-247]
**Problem**: `HostCore::store_save` has no count/size cap on the save directory, and `HostCore::list_saves` reads and bincode-decodes the **entire** `SaveFile` — including the full `WorldSnapshot` payload — for every `*.cspsave` file, on the single game thread.
**Evidence**:
```rust
// backend.rs:409-415 — full decode per file, per list call, on the game thread
let bytes = match std::fs::read(&path) { ... };
let Ok((file, _)) = bincode::serde::decode_from_slice::<SaveFile, _>(&bytes, bincode_cfg()) else { continue; };
```
`store_save` (backend.rs:458-486) enforces only `safe_name`; nothing bounds how many saves exist. The sibling `DashboardStore::save` deliberately caps at `MAX_DASHBOARDS = 64` under its write lock ("a config dir is curated, not a database") — the save dir got no equivalent.
**Impact**: Any holder of the bearer token — precisely the population this server exists to serve (every attached MCP agent / WS client) — can (a) loop `session.save {"name": N}` with unlimited distinct names to fill the user's save dir with full world snapshots (size grows with vessel count), and (b) turn each `session.list_saves` into O(total-save-bytes) CPU on the *single* game thread, stalling every `submit`/`AdvanceTo`/telemetry publish behind it; HTTP callers burn their 30 s `BACKEND_CALL_TIMEOUT` while the work continues. A buggy agent that saves in a loop, or one hostile client among several legitimate ones, degrades the whole host.
**Suggestion**: Mirror the dashboard store's discipline: cap distinct saves (or total bytes) checked under a lock in `store_save`, refusing past the cap with an explicit `BackendError::Session`. For `list_saves`, stop materializing snapshots — restructure `SaveFile` so `schema`/`tick`/`hash` precede the snapshot and read only the fixed-size header (or keep a small sidecar index), making list O(#saves) not O(bytes).

### [SEVERITY: LOW] Finding 2: PROTOCOL.md's session-class tool enumeration omits the `dashboard.*` family — pin-file drift
**File**: /home/didi/workspace/Clanker-Space-Program/crates/csp-server/PROTOCOL.md:[48-53] vs crates/csp-server/src/mcp.rs:[843-858]
**Problem**: PROTOCOL.md's Tools section enumerates the session-class surface exhaustively — "`session.list_saves`, `session.save`, `session.load`, `session.snapshot`" — but `tools_v1()` declares `dashboard.list`, `dashboard.get`, `dashboard.save` as `"class": "session"` as well (asserted by `tools_list_marks_session_class`).
**Evidence**: PROTOCOL.md:50-51: "Session-class tools declare `\"class\": \"session\"` ... : `session.list_saves`, `session.save`, `session.load`, `session.snapshot`." vs mcp.rs:844-858 where the three `dashboard.*` tools are emitted via the `ses(...)` constructor.
**Impact**: The file is the normative revision pin ("Bumping the revision is a deliberate commit: update this file and the constants together") and is the artifact an external MCP client author reads to decide which tools are replay-irrelevant. As written it under-states the session-class surface: a client treating the list as exhaustive may assume `dashboard.save` is sim-class/replay-relevant. Doc-only, but it is the designated source of truth.
**Suggestion**: Add `dashboard.list`/`dashboard.get`/`dashboard.save` to the session-class enumeration (or reword to "at least:" / point at `tools_v1()` as the canonical list).

### [SEVERITY: LOW] Finding 3: WS `subscribe` silently drops malformed entries — contradicts the crate's own "explicit error, never silent truncation" doctrine
**File**: /home/didi/workspace/Clanker-Space-Program/crates/csp-server/src/http.rs:[793-800] (fields), :[826] (rate_hz)
**Problem**: Two silent-failure paths in the subscribe parser:
1. Non-string field entries are filtered out without complaint: `a.iter().filter_map(|f| f.as_str().map(str::to_string)).collect()` — `{"fields": ["tick", 5, "warp_level"]}` subscribes two fields, the garbage entry vanishing with no `error` frame and no `unknown_fields` mention.
2. A non-u64 `rate_hz` (e.g. the string `"64"`, a float, `null`-shaped `0.5`) silently becomes the default 16 via `.and_then(Value::as_u64).unwrap_or(16)` — the client asked for something and got a different cadence with zero feedback.
**Evidence**: The code directly above both sites enforces the opposite policy — an over-cap fields list is "an explicit error, never a silent truncation ... paths past the cap would vanish without even an `unknown_fields` mention (R19-F14)" (http.rs:810-814). The WS envelope rules elsewhere refuse proto-less envelopes explicitly.
**Impact**: A mis-authored client (or a proxy mangling JSON types) gets a silently different subscription — 16 Hz instead of 64 Hz, missing fields — and debugs nothing, since the server answers `{"kind":"subscribed"}` as if the request were honored. Not exploitable; it is a correctness/honesty gap on the exact axis (R19-F14) the crate holds up as its standard.
**Suggestion**: Reject non-string field entries and non-u64 `rate_hz` with the same `{kind: "error"}` frame shape used for the cap and proto violations (e.g. "fields must be an array of strings", "rate_hz must be one of {VALID_RATES:?}").

Follow-ups (not findings): `/assets/*` responses carry no `X-Content-Type-Options: nosniff` and read whole files (≤16 MB) into memory per unauthenticated request — dormant today (no shipped binary sets `spa_dir`, verified by grep) but worth nosniff + `tokio::fs::File` streaming before the SPA wiring lands; `GameHost`'s std mpsc has unbounded backlog depth (bounded only by per-request 64 KiB and caller timeouts — authenticated-only); an operator-supplied empty `token: Some("")` would make `?token=` trivially satisfiable (serve never generates one; a startup assert would close it).

## Verdict (honest)

The security core of this crate is unusually strong and evidently battle-tested against a previous review round (R19-* regression tags everywhere): Origin/Host allowlisting correctly derives from the bind with fail-closed parsing, the 256-bit token is constant-time compared and never served or logged, the token file is created atomically 0600/0700 with symlink/pre-plant refusal verified by e2e, ingress is bounded three ways with atomic batch rollback, the WS loop has connection cap + frame cap + ping/idle/send timeouts, and the e2e suite genuinely attacks itself (foreign Origin/Host, raw and percent-encoded traversal, query-token scoping, symlinked token paths, WS overflow, ingress budget atomicity) rather than happy-pathing. The real gaps are all in the authenticated-client resource-exhaustion family: the save store lacks every cap the dashboard store has (Finding 1 is the one item I'd fix before shipping), plus two honesty drifts (PROTOCOL.md enumeration, silent subscribe coercion). No blocker-level vulnerability found against a hostile local network peer.

