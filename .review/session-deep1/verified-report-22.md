# Verification — review-report-22.md (Cluster C22: `apps/telemetry`)

Verifier's method: read every cited file in `apps/telemetry/src/**`, the
csp-server wire/schema mirrors (`crates/csp-server/src/{http,dashboard,telemetry}.rs`),
the orbit math (`crates/csp-sim/src/orbit.rs`, `lib.rs::telemetry_at`,
`scripts.rs::VesselTelemetry`), `crates/csp-core/src/vec3.rs` (DVec3 serde
shape), `crates/csp-data/src/system.rs` (`KeplerElements` field names), the
shipped home-system data (`data/systems/home/*/body.ron`), and the build
artifacts in `apps/telemetry/dist/`. No code was modified.

**Verdict summary: 8/8 findings CONFIRMED.** The two headline claims both hold
up — the orbit-map really does project the out-of-plane axis, and there really
is no reconnect path.

---

## Finding 1 (MEDIUM): orbit-map projects `pos.z` — CONFIRMED

Verified chain:

- `apps/telemetry/src/lib/widgets/orbit-map.tsx:43-46` reads `pos.x` for screen-x
  but `pos.z` (`pz`) for screen-y: `vy = 80 + pz * scale`. Comment at `:39`
  literally claims "in-plane projection (x→right, z→down)".
- `pos_m` is the **parent-centred inertial** position
  (`crates/csp-sim/src/scripts.rs:329-330`, `lib.rs:7280`), produced for
  on-rails vessels by `perifocal_to_inertial` (`orbit.rs:598-607`) whose
  z-component is `si·sw·x + si·cw·y + ci·z` — identically 0 when `i = 0`
  (perifocal vectors are planar, `z = 0`; `periapsis_state` at `orbit.rs:563`
  returns `DVec3(r_p, 0, 0)`).
- Inclination is measured from **+z**: `orbit.rs:742`
  `atan2(√(hx²+hy²), hz)`. So `i = 0` ⇒ orbital plane = x–y plane ⇒ `pos.z ≡ 0`.
- The shipped system confirms "the overwhelmingly common case": Terrin
  `inclination_rad: 0.0`, Rask `0.0`, Duva `0.001 rad` (0.06°) — and any
  equatorial launch yields `pos.z ≈ 0`.
- The ellipse's own convention is internally consistent with **inertial y →
  screen-up**: centre `cx = 100 − c·cos ω·s`, `cy = 80 + c·sin ω·s` plus
  `rotate(−ω)` puts the periapsis vertex at `(100 + r_p·cos ω·s, 80 −
  r_p·sin ω·s)` — verified algebraically (unrotated +x vertex rotated by
  `R(−ω)` gives direction `(cos ω, −sin ω)`, matching an `(x, −y)` screen map
  of inertial `(cos ω, sin ω)`).
- Consequence confirmed: for an equatorial orbit the marker sits at
  `(100 + pos.x·s, 80)` — it slides along a horizontal diameter and can only
  ever coincide with the ellipse where that diameter crosses it (two points).
  Even for inclined orbits it paints the out-of-plane component on the axis
  the ellipse reserves for in-plane y.

Minor amplifications the report understates slightly: the widget ignores
`inclination_rad`/`ascending_node_rad` entirely, so even the suggested fix
(project `pos.x`/`pos.y`) only makes the marker consistent for equatorial
orbits — for inclined orbits the marker would show the reference-plane
projection while the ellipse is drawn in-plane. That is a deeper (still
acceptable for "schematic") limitation, not a counter-argument.

**CONFIRMED** — every cited line number checks out (`orbit-map.tsx:39-46`,
`orbit.rs:563`, `:598-607`, `:742`).

## Finding 2 (MEDIUM): no reconnect/resubscribe — CONFIRMED

- `subscribeTelemetry` is called exactly once, in the `[]`-deps effect at
  `index.tsx:16-36`; grep confirms it is the only call site.
- `ws.ts:81-82`: `onerror`/`onclose` only invoke `onState` — no retry, no
  backoff, and the returned handle is just `ws.close`.
- On `'closed'`/`'error'`, `frame.current` is never cleared
  (`index.tsx:29` `onState: (s) => setConn(s)` also drops the `detail`
  string), so the header keeps rendering `· tick N` (`index.tsx:45`) and all
  widgets keep painting the last frame. The status dot does turn red
  (`conn` → `dead`), which the report doesn't deny — it is the *data* that
  goes silently stale.
- The report's nuance is accurate: a reconnect helper could recover a
  transient drop on a live server (token still valid), but after a *server
  restart* the per-process token rotates (`http.rs:14-19`), so in-place
  recovery is impossible anyway; clearing/marking the stale frame is the
  real gap.

**CONFIRMED.**

## Finding 3 (LOW): `.dash-grid` has no row sizing — CONFIRMED

- `styles.css:66-70` declares only `display:grid; grid-template-columns:
  repeat(12,1fr); gap:12px` — grep confirms no `grid-auto-rows` or
  `grid-template-rows` anywhere in the file.
- `index.tsx:55-56` emits `gridRow: {y+1} / span {h}` over implicit
  (content-sized) rows, while the server schema documents `rect` as
  "column units on a 12-col grid, **row units of the SPA's tile height**"
  (`dashboard.rs:89-90` — the report's cite of `:90-101` is off by one line,
  trivial).
- The chrome is indeed built for fixed tiles: `Card` → `.widget-tile`
  (`card.tsx:10`) with `min-height:0; flex column` (`styles.css:27-36`) and
  `CardContent` → `flex-1 min-h-0` (`card.tsx:21`) — dead structure while
  rows are auto-sized.

**CONFIRMED.**

## Finding 4 (LOW): no inbound `proto` check — CONFIRMED

- `ws.ts:68-79` dispatches on `msg.kind` alone; `msg.proto` is never read and
  `TelemetryFrame` (`ws.ts:11-17`) doesn't carry it.
- The server stamps `proto` on every outbound frame
  (`http.rs:507, 523-525, 540-542, 548-551, 562-565, 578-585`) and enforces it
  inbound (`http.rs:522`); `http.rs:82-84` documents `proto` as the
  breaking-change bump mechanism. A proto-2 payload would be consumed
  silently.

**CONFIRMED.**

## Finding 5 (LOW): `unknown_fields` only surfaced by gauge — CONFIRMED

- Server collects unresolved paths into `unknown_fields`
  (`telemetry.rs:96-111`, "never silently dropped").
- `gauge.tsx:19` checks `frame?.unknown_fields.includes(path)` → "unknown
  field …". `table.tsx:22` renders `—` for missing values with no check;
  `graph.tsx:19-22` silently drops non-numbers; `orbit-map.tsx:25-26`
  collapses an unknown `elements` field into "no conic — vessel not on
  rails", mislabeling an authoring error as sim state.

**CONFIRMED.**

## Finding 6 (LOW): graph hardcodes 16 Hz — CONFIRMED

- `graph.tsx:14` `const rate = 16` → `cap = window_s × 16`; at `rate_hz: 8` a
  `window_s: 60` sparkline really does span 120 s.
- `WidgetProps` is `{def, frame}` only (`registry.tsx:14-17`) — no rate
  channel exists.
- The fix path the report suggests exists: the server's `subscribed` ack
  carries `rate_hz` (`http.rs:562-565`) and the client discards it
  (`ws.ts:75-76` maps the whole message to `'live'`).

**CONFIRMED.**

## Finding 7 (LOW): `vite dev` has no path to csp-server — CONFIRMED

- `vite.config.ts` (24 lines, read in full) has no `server` block at all — no
  proxy for `/ws` or `/api`.
- `ws.ts:44` builds the WS URL same-origin (`new URL('/ws',
  window.location.href)`); the dev script is `vite dev --port 47901`
  (`package.json`), so the socket targets the Vite server. Nothing there
  answers `/ws` (Vite's own upgrade handling only services its HMR channel —
  not runtime-tested here, but irrelevant: with no proxy rule the request
  provably never reaches csp-server).
- Additional support the report didn't mention: under `vite dev` there is no
  injected `<meta name="csp-token">`, so `cspToken()` returns `''` — even a
  hypothetical manual proxy would 401 without extra work.
- `/api/dashboards` 404s are then masked by the bundled-default fallback
  (Finding 8), exactly as reported.

**CONFIRMED.**

## Finding 8 (LOW): `loadDashboard` swallows failures; `saveDashboard` dead — CONFIRMED

- `dashboard.ts:56-68`: any non-`res.ok` (503 unconfigured, 404 `Invalid`,
  500 serde/`Json` error from a `deny_unknown_fields` rejection — see
  `dashboard_get` mapping at `http.rs:341-347`) or a thrown fetch all fall
  through to the bundled `default.json`, with no logging and no distinction
  surfaced.
- `saveDashboard` (`dashboard.ts:71-81`) returns a bare `bool`; grep
  confirms **zero callers** in `src/` — the PUT write path is dead code and
  would discard the server's detailed 400 messages
  (`http.rs:361-369` → `"invalid dashboard: …"`, `dashboard.rs:154`).

**CONFIRMED.**

---

## Spot-check of the report's "verified-correct" / "non-finding" claims

All independently re-verified, all hold:

- Wire envelope `{proto:1, kind:"subscribe", fields, rate_hz}` ↔
  `ws_session` (`http.rs:521-565`); `VALID_RATES` identical on both ends
  (`ws.ts:9`, `http.rs:78`, `dashboard.rs:61`).
- `?token=` + `?client=telemetry-spa` ↔ `TokenQuery`/`McpQuery`
  (`http.rs:223-227, 235-238, 388-392, 480-483`); `cspToken()` name matches
  the injected `<meta name="csp-token">` (`http.rs:261`).
- `Dashboard`/`Widget`/`Rect` TS interfaces mirror the
  `#[serde(deny_unknown_fields)]` Rust structs field-for-field;
  `subscriptionFields()` matches `subscription_fields()` (dedupe+sort).
- `vessels.0` resolves because `BTreeMap<u64,_>` serializes as an object and
  `field()` walks `Object`/`Array` (`telemetry.rs:77-91`); every path in
  `dashboards/default.json` exists in `VesselTelemetry`.
- `DVec3` derives `Serialize` as `{x,y,z}` struct (`csp-core/src/vec3.rs:17-19`);
  `KeplerElements` field names match the widget's reads
  (`csp-data/src/system.rs:42-63`).
- Build contract holds in the present `dist/`: `_shell.html` + `index.html` +
  `assets/` exist, `/assets/...` URLs, real `</head>` for injection;
  `finalize.mjs` does exactly the documented promote.
- `routeTree.gen.ts` exists and is consistent with the single route.
- MessagePack deferral (`http.rs:27-35`) and `spa_dir` unwiring
  (`http.rs:47-51`) are documented deferrals, not bugs. `unsubscribe` is
  indeed implemented by neither side (`ws_session` matches only
  `subscribe`/`command`).

### One inaccuracy in a "verified-correct" bullet (not a finding)

The bullet "inbound `telemetry`/`subscribed`/`error`/`command_ack` handling
match" overstates: the client **does not handle `command_ack`** — `ws.ts`
has no branch for it, so an ack would fall through `onmessage` silently.
Benign today (the client never sends `kind:"command"`), but the mirror is
not as exact as claimed. Worth noting for a future command channel.

## Caveats / not runtime-tested

- Build was not re-run; verification of the build contract rests on the
  current `dist/` artifacts, which match the claims.
- Vite's exact rejection behaviour for a foreign `/ws` upgrade was reasoned
  from the missing proxy config, not exercised — the finding stands either
  way since nothing forwards the request to csp-server.
