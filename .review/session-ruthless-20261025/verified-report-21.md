# Verified Report 21 — apps/telemetry (TS telemetry dashboard)

Verification agent re-check of review-report-21.md against actual source.
Status: IN PROGRESS (checkpoints appended per finding).

### Finding 1: Unbounded window_s x rate → Math.min(...data) spread crash + O(cap) per-render polyline — CONFIRMED
**Original**: graph.tsx derives the rolling-buffer cap from user-authored `window_s` x negotiated rate with no bound, spreads the buffer into Math.min/Math.max, no error boundary → server-accepted config bricks the UI.
**Verification**:
- apps/telemetry/src/lib/widgets/graph.tsx:12 `windowS = num(def.options.window_s) ?? 60`; :15-16 `const cap = Math.max(16, Math.floor(windowS * rate))` — no upper clamp anywhere in the file.
- graph.tsx:42-43 `const lo = data.length ? Math.min(...data) : 0` / `Math.max(...data)` — spread of the full buffer on every render; :45-47 `data.map(...).join(' ')` rebuilds the polyline string per render; :30 splice trims once over cap.
- Server accepts it: crates/csp-server/src/dashboard.rs:68-69 — options are "`unit`/`min`/`max`/`window_s` … free-form JSON, validated only for shape" (:84-87 `serde_json::Map<String, Value>`). `validate()` (dashboard.rs:281+) never inspects option values (grep of the validate body: zero matches for options/window_s) — it bounds widgets/fields/rects/rate only. So `window_s: 3600` is a storable, served, client-accepted config; 3600 x 64 = 230,400 as claimed.
- Empirical on this machine's V8 (node -e): `Math.min(...arr)` OK at 125,000 elements, **throws `RangeError: Maximum call stack size exceeded` at 200,000 and at 230,400** — matching the report's threshold claim exactly.
- No error boundary: grep for ErrorBoundary/componentDidCatch/getDerivedStateFromError/onError across apps/telemetry/src returns nothing; __root.tsx is a bare component with no `errorComponent`. A render throw unmounts the tree.
- Nuance (does not change the verdict): the buffer fills at `rate` samples/s, so on this desktop V8 the crash needs ~50 min of live streaming at 64 Hz/3600 s to cross 200k (a reload resets the buffer and the clock); the report's mobile ~30-60k figure would recur within ~8-16 min. The freeze half is immediate: O(cap) map/join per render at up to 64 renders/s (Finding 2) long before the throw. Still a real, reproducible UI brick from legitimate config on a long-running session — which is what telemetry dashboards are for.

### Finding 2: Every WS message force()s a full tree re-render at up to 64 Hz — CONFIRMED
**Original**: index.tsx calls `force(n => n+1)` per inbound frame; ws.ts's "drop-to-latest" contract is not implemented on the render side; no widget is memoized.
**Verification**:
- apps/telemetry/src/routes/index.tsx:38-41 — `onFrame: (f) => { frame.current = f; force((n) => n + 1) }`, verbatim.
- apps/telemetry/src/lib/ws.ts:5-6 header claims "Backpressure is drop-to-latest — … only the newest payload is ever surfaced"; ws.ts:188-194 forwards every `telemetry` message to `onFrame` with no coalescing — each WS message event is a distinct macrotask, so React 18 automatic batching cannot coalesce them; render cadence equals message cadence (max rate 64, dashboard.rs:62).
- No memoization: grep for `memo`/`shouldComponentUpdate` in apps/telemetry/src/lib/widgets/ → none; registry.tsx:24-29 registers plain function components; index.tsx:90-110 re-renders every Card/widget per `force`, each receiving a fresh `frame` object.

### Finding 3: `let msg: any` at the WS boundary; payload/unknown_fields trusted — CONFIRMED
**Original**: ws.ts:167 parses to `any`; `msg.payload ?? {}` / `msg.unknown_fields ?? []` pass non-record/non-array values through into widget render code.
**Verification**:
- ws.ts:167 `let msg: any`; :192 `payload: msg.payload ?? {}` (a string/number payload survives — `??` is nullish-only); :193 `unknown_fields: msg.unknown_fields ?? []` (a string survives).
- Downstream indexing confirmed: graph.tsx:34-35, gauge.tsx:11/:19, table.tsx:17/:27 (`frame.payload[f]`, `frame.unknown_fields.includes(...)`), orbit-map.tsx:34-40 — a non-record payload or non-array unknown_fields throws mid-render, and per Finding 1 there is no error boundary. Latent (today's server serde-types the envelope), exactly as the report scopes it.

### Finding 4: 200 + schema_version:1 but missing widgets → throw inside .then, silent blank — CONFIRMED
**Original**: loadDashboard returns a version-matched JSON body with no shape check; subscriptionFields throws `d.widgets.flatMap` inside the effect promise → unhandled rejection, empty grid, conn stuck 'connecting', no notice.
**Verification**:
- apps/telemetry/src/lib/dashboard.ts:99-101 — `if (dash?.schema_version === DASHBOARD_SCHEMA_VERSION) return { dashboard: dash, source: 'server' }`, no widgets/rect check (the function carefully handles non-JSON :90-98, version mismatch :102-108, 401/403/404/503 :110-117 — this is the one unguarded branch).
- dashboard.ts:40 `d.widgets.flatMap((w) => w.fields)` throws TypeError on missing/null `widgets`; index.tsx:36 calls it inside `loadDashboard('default').then(...)` → unhandled promise rejection, `subscribeTelemetry` never runs → `conn` stays 'connecting' (initial state, index.tsx:21), no notice.
- Secondary claim verified: index.tsx:96 `w.rect.x` throws at render for a widget missing `rect` (no error boundary).
- Mechanism nuance (symptom unchanged): `setDash` (index.tsx:33) runs *before* the throwing line, so `dash` is set, not null — but with `widgets` absent the grid maps over `dash?.widgets ?? []` (index.tsx:90) and renders empty, so the observable outcome is exactly as reported: header + empty grid, stuck 'connecting', no notice.

### Finding 5: VALID_RATES exported, never used — CONFIRMED
**Original**: dead export in ws.ts; invalid rate_hz only caught via server error after a wasted connect cycle.
**Verification**:
- ws.ts:17 `export const VALID_RATES = [64, 32, 16, 8, 4, 2, 1]`; grep across apps/telemetry/src and apps/telemetry/scripts finds zero other references. `subscribeTelemetry` (ws.ts:136-139) uses `opts.rateHz ?? 16` unchecked; the value arrives from `loaded.dashboard.rate_hz` (index.tsx:37), which the *bundled* default.json path (dashboard.ts:66-68) does not validate. Server pins the same set at dashboard.rs:62 — LOW severity as scoped.

### Spot-check of clean claims — CONFIRMED CLEAN
- ws.ts backoff/liveness: reconnect delay `Math.min(baseDelayMs * 2**attempts, maxDelayMs)` with attempts incremented after computing the delay and reset only on `subscribed` (ws.ts:196-200, 219-229) — sound. csp-server/src/http.rs:113 `WS_IDLE_TIMEOUT = 90s`; http.rs:755-770 — any inbound frame, pings'-pongs included ("Pongs to our pings included"), resets `last_seen`, so the client's post-subscribe silence (browser auto-pongs) is safe. Matches the report.
- orbit-map projection: apps/telemetry/src/lib/widgets/orbit-map.tsx:96-105 implements R3(−Ω)·R1(−i)·R3(−ω) — hand-checked against the active-rotation formulas; the screen mapping reduces to (x, −y) for i = Ω = 0 as the comment at :81-82 states, and the ellipse-center offset (orbit-map.tsx:76-77) is consistent with the focus-at-(100,80) + center-at−c·periapsis construction. crates/csp-sim/src/orbit.rs:627 confirms the stated perifocal→inertial convention `R3(Ω)·R1(i)·R3(ω)`.

## Summary: 5 confirmed, 0 dismissed.
