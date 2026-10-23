# csp-server protocol pins (M9c, PLAN §2.12)

## MCP transport

**Targeted protocol revision: `2026-07-28`.** The server also accepts
`2025-11-25` on `initialize` / the `MCP-Protocol-Version` header. It
must **never** implement the 2024-11-05 HTTP+SSE transport.

`mcp::PROTOCOL_REVISION` / `SUPPORTED_REVISIONS` are the code-side pins;
`mcp::tests::protocol_revision_is_the_pinned_one` asserts them. Bumping
the revision is a deliberate commit: update this file and the constants
together.

### Streamable HTTP shape

- **One endpoint**: `POST /mcp`. Exactly one JSON-RPC message per POST
  (JSON-RPC batching was removed in revision 2025-03-26).
- **Responses**: `application/json` by default; a per-request
  `text/event-stream` upgrade when the client's `Accept` includes it —
  the response rides `event: message` frames and the stream closes.
- **No `GET` stream, no `DELETE`, no protocol-level sessions** —
  revision 2026-07-28 removed them; both verbs return 405.
- **No `Mcp-Session-Id`** is issued or required. Client identity for the
  §2.12.3 `source_id` is the declared `X-CSP-Client` name (or
  `?client=`), allocated in connection order by `ClientRegistry`.
- `MCP-Protocol-Version` is validated when present (400 on an unknown
  revision); `initialize` negotiates per the spec — echo a supported
  offer, else answer `2026-07-28`.

### stdio (`csp-mcp` bin in csp-bench)

Newline-delimited JSON-RPC 2.0: one message per line on stdin,
responses as one line each on stdout. Notifications get no output.
The bin owns its own headless sim (`LocalGame`); it never attaches to
a running game — that path is HTTP-only by §2.12.1's decision.

## Tools — `csp.mcp.tools.v1`

Additive-only within the major version; a tool never silently changes
semantics. Session-class tools declare `"class": "session"` in their
descriptor and are **not replay-relevant**: `session.list_saves`,
`session.save`, `session.load`, `session.snapshot`. Everything else is
sim-class and maps to `SimCommand`s (stamped `source_class=External`,
one-tick latency, §2.2.1 control lease) or telemetry reads.

## Security (normative, not optional)

- `Origin` allowlist on **every** endpoint: `localhost` / `127.0.0.1` /
  `[::1]` authorities only — DNS-rebinding protection.
- Bearer token on every endpoint except `/` (the token is injected
  into the served HTML `<meta>`); `$XDG_RUNTIME_DIR/csp/<pid>.token`,
  mode 0600, rotated per start, never in a save.
- Bind `127.0.0.1` by default; LAN binds are the app's explicit opt-in.
- 64 KiB max message (HTTP body limit, WS frame cap, ingress encoded
  size); bounded ingress queue; ≤32 commands per client per drain.

## WS telemetry (`GET /ws`, §2.12.2)

- Envelope `{proto: 1, kind, seq, tick, payload}`.
- Client declares `fields` (dotted paths into the snapshot) — there is
  no "send me everything". Server reports unresolvable paths in
  `unknown_fields`.
- `rate_hz` ∈ {64, 32, 16, 8, 4, 2, 1}, default 16.
- Backpressure: drop to latest — each send re-reads the `ArcSwap`;
  nothing queues.
- `{kind: "command", command: <SimCommand>}` is the opt-in command
  channel under the same token auth.
