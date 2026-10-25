# Code Review Report — Cluster 21: apps/telemetry (TS telemetry dashboard)

Reviewer: ruthless code reviewer, static analysis only (no cargo/npm per instruction)
Date: 2026-10-25
Scope: apps/telemetry — src/routes/index.tsx, src/routes/__root.tsx, src/lib/ws.ts, src/lib/dashboard.ts, src/lib/utils.ts, src/lib/widgets/{orbit-map,graph,gauge,table,registry}.tsx, src/components/ui/card.tsx, scripts/finalize.mjs, vite.config.ts, package.json, dashboards/default.json

Findings appended below as confirmed.

---

### [SEVERITY: HIGH] Finding 1: Unbounded `window_s` x negotiated rate crashes GraphWidget render (`Math.min(...data)` spread) and freezes the tab
**File**: apps/telemetry/src/lib/widgets/graph.tsx:[16], [42-47]
**Problem**: The rolling-buffer cap is derived from two values neither side bounds: `const cap = Math.max(16, Math.floor(windowS * rate))` where `windowS` comes from the user-authored dashboard JSON and `rate` is the server-negotiated rate (up to 64 Hz). Autoscale then spreads the whole buffer into `Math.min`/`Math.max` on every render.
**Evidence**:
```ts
const cap = Math.max(16, Math.floor(windowS * rate))      // graph.tsx:16
const lo = data.length ? Math.min(...data) : 0            // graph.tsx:42
const hi = data.length ? Math.max(...data) : 1            // graph.tsx:43
```
csp-server's `dashboard.rs::validate()` bounds widgets, fields, rects, and rate — but options are explicitly "free-form JSON, validated only for shape" (`dashboard.rs:68-69`), so `window_s: 3600` (an hour-long trace) is a *valid* stored dashboard. At `rate_hz: 64` that makes `cap = 230,400`. Verified on this machine's V8: `Math.min(...a)` throws `RangeError: Maximum call stack size exceeded` at 200k+ args (Node, desktop stack); mobile browsers throw at far smaller counts (~30-60k, i.e. a mere `window_s: 600` at 64 Hz). The throw happens during component render; there is no error boundary, so React unmounts the whole tree — and reload re-fetches the same dashboard and crashes again.
Even below the crash threshold the tab freezes: every frame (up to 64/s, see Finding 2) rebuilds the polyline via `data.map(...).join(' ')` — O(cap) string construction per render per graph — plus a per-frame `splice(0, 1)` memmove of the whole buffer once at cap (graph.tsx:30).
**Impact**: A legitimate, server-accepted dashboard config bricks the telemetry UI — first an unresponsive tab as the buffer grows, then an unrecoverable blank page that reproduces on every reload until the dashboard is edited server-side.
**Suggestion**: Clamp the cap (`const cap = Math.min(8192, Math.max(16, ...))`, decimating if a longer window is requested) and compute `lo`/`hi` with a loop/`reduce` instead of spread. Optionally surface a notice when `window_s` is clamped.

### [SEVERITY: MEDIUM] Finding 2: Every WS message force-renders the entire tree at up to 64 Hz — the claimed drop-to-latest backpressure is not implemented on the render side
**File**: apps/telemetry/src/routes/index.tsx:[38-41], apps/telemetry/src/lib/ws.ts:[5-6]
**Problem**: `onFrame` calls `force((n) => n + 1)` for *every* inbound telemetry frame. WS `message` events are separate macrotasks, so React's automatic batching never coalesces them: at a negotiated 64 Hz the full dashboard tree — every Card, gauge, table, graph polyline, and the orbit map — re-renders 64 times per second, even though only the newest frame is ever visible.
**Evidence**:
```ts
onFrame: (f) => {
  frame.current = f
  force((n) => n + 1)   // index.tsx:38-41
},
```
ws.ts's header states the contract — "Backpressure is drop-to-latest — frames are consumed as they arrive and only the newest payload is ever surfaced" (ws.ts:5-6) — but ws.ts just forwards every frame; nothing drops intermediate frames before render. No widget is memoized (`registry.tsx` components are plain functions receiving a fresh `frame` object each time), so nothing escapes the storm. Combined with Finding 1's O(cap) per-render polyline work this is the difference between a smooth 60 fps dashboard and a pinned CPU core.
**Impact**: Wasted CPU proportional to `widgets x rate` — visible jank at 64 Hz with several graph widgets; on weak hardware the UI falls behind the stream and interaction (token reset button) lags.
**Suggestion**: Coalesce renders to display cadence: in `onFrame`, store the frame and schedule `force` via `requestAnimationFrame` with a pending flag (cancel/skip if one is already scheduled). That finally implements drop-to-latest where it matters and caps renders at ~60/s regardless of negotiated rate.

### [SEVERITY: LOW] Finding 3: `let msg: any` at the WS boundary — envelope fields flow into widgets untyped and unvalidated
**File**: apps/telemetry/src/lib/ws.ts:[167-194]
**Problem**: The parsed inbound envelope is declared `any`, and the constructed `TelemetryFrame` trusts `msg.payload`/`msg.unknown_fields` to be an object/array respectively. Downstream widgets index `frame.payload[path]` and call `frame.unknown_fields.includes(f)` (table.tsx:17, graph.tsx:34, gauge.tsx:19, orbit-map.tsx:34) — a payload that is not a record or an `unknown_fields` that is not an array would throw mid-render (again with no error boundary).
**Evidence**:
```ts
let msg: any                       // ws.ts:167
...
payload: msg.payload ?? {},        // ws.ts:192 — '' or 42 pass through as-is
unknown_fields: msg.unknown_fields ?? [],  // ws.ts:193 — a string passes through
```
Today's csp-server always sends well-typed envelopes, so this is latent rather than live — but the `proto` gate exists precisely because a version-skew is possible; the same skew could also change field shapes, and this code would paint garbage or crash instead of erroring cleanly.
**Impact**: Type-safety hole at the one trust boundary the SPA has; a protocol evolution or a non-conforming server turns into a render crash or silently wrong data instead of a wire error state.
**Suggestion**: Parse into a narrow type: `const msg: unknown = JSON.parse(...)`, then a small type guard validating `kind`, `payload` is a record, and `unknown_fields` is an array, routing failures to the existing `error`/stale path.

### [SEVERITY: LOW] Finding 4: A 200 dashboard with `schema_version: 1` but a missing/malformed `widgets` array crashes `subscriptionFields` inside the effect promise — silent permanent blank with no notice
**File**: apps/telemetry/src/lib/dashboard.ts:[99-101], [40]; apps/telemetry/src/routes/index.tsx:[31-36]
**Problem**: `loadDashboard` carefully handles non-JSON bodies and schema-version mismatch, but a 200 whose JSON parses and carries `schema_version: 1` is returned unchecked. `index.tsx` then calls `subscriptionFields(loaded.dashboard)` → `d.widgets.flatMap(...)`, which throws `TypeError` when `widgets` is absent/null. The throw happens inside the `loadDashboard(...).then(...)` callback — an unhandled promise rejection: `dash` stays `null`, the page renders the header with an empty grid, `conn` stays `'connecting'` forever, and no notice is shown.
**Evidence**:
```ts
if (dash?.schema_version === DASHBOARD_SCHEMA_VERSION) {
  return { dashboard: dash, source: 'server' }   // dashboard.ts:99-101 — no shape check
}
export function subscriptionFields(d: Dashboard): string[] {
  return [...new Set(d.widgets.flatMap((w) => w.fields))].sort()  // dashboard.ts:40 — throws
}
```
csp-server currently serde-deserializes and `validate()`s every stored dashboard before serving (dashboard.rs:208-222), so this exact server won't emit it — but the client is a separately-versioned static bundle talking to an HTTP API, and the code already defends every *other* failure mode in the same function. Similarly unguarded: a widget missing `rect` throws at `w.rect.x` in the render (index.tsx:96).
**Impact**: Any future/alternative server (or a version-skewed one) that returns shape-invalid JSON converts a recoverable condition into a silent, notice-free dead page.
**Suggestion**: Add a cheap shape check alongside the version check — `Array.isArray(dash?.widgets) && dash.widgets.every(w => w && Array.isArray(w.fields) && w.rect)` — falling back to the bundled default with a notice, matching the function's existing pattern.

### [SEVERITY: LOW] Finding 5: `VALID_RATES` is exported but never used — an invalid `rate_hz` is only caught after a wasted connect/reconnect cycle
**File**: apps/telemetry/src/lib/ws.ts:[17], [136-139]
**Problem**: `VALID_RATES = [64, 32, 16, 8, 4, 2, 1]` is dead in the SPA (grep: defined, never referenced). `subscribeTelemetry` sends whatever `rateHz` it is given; the server's explicit `error` frame surfaces in the UI, so it is not silent — but the client already knows the set and could reject before opening a socket.
**Evidence**: `export const VALID_RATES = [64, 32, 16, 8, 4, 2, 1]` has zero usages under `apps/telemetry/src/`. The server pins the same set (PROTOCOL.md: `rate_hz in {64, 32, 16, 8, 4, 2, 1}`) and `dashboard.rs::validate()` enforces it for stored dashboards, so in practice the value arriving here is server-validated — hence LOW.
**Impact**: Minor: dead export plus a round-trip-to-error instead of an immediate client-side refusal if the value ever arrives unvalidated (e.g. from a hand-edited bundled default.json, which is *not* validated at load).
**Suggestion**: Use it — `if (!VALID_RATES.includes(rate)) throw` (or clamp to 16) in `subscribeTelemetry` — or delete the export.

Follow-ups (out of scope, one line): `VALID_RATES` duplication between ws.ts and dashboard.rs is convention-dependent (server is source of truth); vite.config.ts / package.json / finalize.mjs / __root.tsx / card.tsx / utils.ts / default.json reviewed clean — no findings; ws.ts reconnect state machine verified sound (attempts reset only on `subscribed`, close-handle race-free, server counts auto-pongs for its 90 s liveness so the client's post-subscribe silence is safe — verified against csp-server/src/http.rs:763-769).

---

## Verdict

Honest quality assessment of Cluster 21 (apps/telemetry, ~1,080 LOC):

1. This is carefully written, unusually well-commented code — stale-frame discipline, retry-budget reset on `subscribed` (not `open`), proto gating, and error-notice plumbing all show real protocol thought, and the orbit-map projection math checks out against its stated convention.
2. The one systemic weakness is scale discipline on the render path: nothing bounds `window_s x rate` (Finding 1, HIGH — verified crash + freeze) and nothing coalesces renders to display cadence (Finding 2), together breaking the dashboard exactly when the data gets interesting (long window, high rate).
3. The WS boundary is functionally correct against today's server (reconnect, backoff, token lifecycle, liveness all verified against csp-server source), but it leans on the server being perfectly typed — the `any` parse and missing shape checks (Findings 3, 4) are latent, cheap-to-fix gaps at the only trust boundary this SPA has.
4. Malformed-config handling is 80% excellent (non-JSON, version mismatch, 401/403/404/503 all surfaced with distinct notices) — the remaining 20% (shape-validity, Finding 4) is exactly where it would fail silently.
5. Net: no blockers for the current paired-server deployment; the HIGH graph finding is a real, reproducible UI-brick from a server-accepted config and should be fixed before users author their own dashboards.

**REQUEST CHANGES** — fix Finding 1 (clamp + loop-reduce) before user-authored dashboards; Findings 2-5 are quality hardening.
