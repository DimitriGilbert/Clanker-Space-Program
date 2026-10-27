# Review report C28 — Telemetry SPA (`apps/telemetry`)

Scope: `apps/telemetry/src/{router.tsx, routes/{index,__root}.tsx, lib/{dashboard,utils,ws}.ts, lib/widgets/{gauge,graph,orbit-map,registry,table}.tsx, components/ui/card.tsx, vite-env.d.ts}` + `vite.config.ts`, `tsconfig.json`, `package.json`, `dashboards/default.json`. Cross-read: `csp-server/src/{telemetry,dashboard,http,serve,backend}.rs`, `csp-sim` (`VesselTelemetry`, `VesselId` allocation, orbit rotation), `csp-app/src/lib.rs` (`--serve`). Verified empirically: `npx tsc --noEmit` passes clean; orbit-map math re-derived against `perifocal_to_inertial` (orbit.rs:639) — correct; all CSS vars/classes used by widgets exist in `styles.css`.

Positives worth recording: the WS client is genuinely well-built (proto gate on inbound frames, stale-frame discipline on every drop path, retry budget reset only on a live `subscribed` ack, rate validated locally against the pinned divisor set), token bootstrap matches §2.12.3 exactly (`?csp_token=` → sessionStorage → stripped from the address bar; bearer on `/api`, `?token=` only on `/ws`), package.json is pinned exactly per §2.12.2, rAF render coalescing honors drop-to-latest, and `loadDashboard` surfaces *why* the bundled fallback engaged for every failure class. The widget field-path names (`elements.semi_major_axis_m`, `pos_m.{x,y,z}`, `apoapsis_altitude_m`, …) all match the server's serialized shapes exactly (`KeplerElements` in csp-data/src/system.rs:43, `DVec3` {x,y,z} in csp-core/src/vec3.rs:28, `VesselTelemetry` in csp-sim/src/scripts.rs:356).

---

### [SEVERITY: CRITICAL] Finding 1: The default dashboard goes permanently dark after the first vessel recovery — `vessels.0.*` is hardcoded and the `focus.*` alias that fixes it is never published
**File**: apps/telemetry/dashboards/default.json:11-72 (every widget), apps/telemetry/src/routes/index.tsx:39-41; root cause server-side: crates/csp-server/src/backend.rs:643,710,856-886 vs crates/csp-server/src/telemetry.rs:33-40,122-128

**Problem/Evidence**: Every widget in the bundled default subscribes `vessels.0.*`. `VesselId`s are allocated from a monotonic counter starting at 0 (`csp-sim/src/lib.rs:2504`, `:5834-5840`) and are **never reused** — `RecoverVessel`/`TerminateVessel`/breakup remove the vessel from the map (`lib.rs:5016`, `:11573`) and the counter only advances. The server snapshot duplicates the focused vessel precisely so "a dashboard can name `focus.altitude_m` instead of hard-coding `vessels.0`" (telemetry.rs:33-40) — but `from_world_focus` has **zero callers**: every publish point (`backend.rs:643,710,856-886`, incl. the `GameHost` that `csp --serve` uses via `csp-app/src/lib.rs:2978-2985`) calls `from_world` with `focus: None`, so `focus.*` resolves as unknown on every live stream. The SPA uses neither mechanism and has no field picker to recover.

**Impact**: Normal play loop is launch → fly → **recover** (recovery credit is the science loop's endpoint, `csp-ui/src/program.rs:2385`). After the first recovery, the second launch is `vessels.1` and **7 of 8 widgets show "unknown field" forever** — including the Orbit map, both traces, and every gauge. There is no in-SPA recovery: subscription fields are fixed at subscribe time (index.tsx:39-41), no vessel picker exists, and the live vessel id is not even discoverable from the dashboard (no widget subscribes `focus_vessel_id`/`vessels`). Out-of-band workaround only: `PUT /api/dashboards/default` with rewritten paths, blind to the actual id. This is the "stale dashboards after world reset/load" failure mode the cluster brief names, triggered by the *standard* progression loop, not an edge case.

**Suggestion**: (a) Server: have the hosted `GameHost` publish focus (the session's focused vessel where one exists, newest-vessel fallback in headless mode) through `from_world_focus` — the API already exists and is dead code. (b) SPA: switch `default.json` to `focus.*` paths. (c) Cheap defense-in-depth: a table widget on `focus_vessel_id` so the id is at least visible.

### [SEVERITY: HIGH] Finding 2: Progression data is on the wire but invisible on the hosted surface — zero `program.*` widgets, dashboard name hardcoded to `default`, stored dashboards unreachable
**File**: apps/telemetry/dashboards/default.json:1-77, apps/telemetry/src/routes/index.tsx:35; wire side: crates/csp-server/src/telemetry.rs:41-73

**Problem/Evidence**: `ProgramSnapshot` (telemetry.rs:52-73) was added explicitly so the write surface's blind spot ("an agent could `HireCrew`/`FoundColony` and never read a byte of it back — report-19 F3") is closed: `funds_micros`, `science_banked`, `contract_offers`, `contracts_active`, `crew_roster`, `colonies[]`, `scripts[]`, `autopilots[]` are all subscribable via dotted paths. The bundled default dashboard subscribes **none of them** — it is still flight-only (title: "Flight Telemetry"). Meanwhile the Game UI shows exactly this progression (funds/contracts/crew/colonies panels, program.rs). Compounding: `loadDashboard('default')` is hardcoded (index.tsx:35); `GET /api/dashboards` (list) is never called; the store validates and serves up to 64 dashboards (dashboard.rs:49,190-206) that the SPA can render but never load — any stored name other than `default` is UNREACHABLE from the hosted surface. There is also no field browser, so an agent authoring a dashboard cannot discover `program.*` paths from the UI at all.

**Impact**: The hosted `--serve` surface shows a strictly smaller world than the game does, and §2.12's "Telemetry web screens" product row (§4: "Telemetry screens + **user-defined widget dashboards** — v1 (M9d)") is degraded: user-defined dashboards are possible only via out-of-band curl/MCP, and only ever one of them (`default`) is displayable. No PLAN §6.4 cut-list entry sanctions leaving progression off the dashboard.

**Suggestion**: Add a "Program" table widget to `default.json` (`program.funds_micros`, `program.science_banked`, `program.contract_offers`, `program.contracts_active`, `program.crew_roster`, `program.tech_researched`) — one widget, ~6 fields, well under the 64-field cap. Add a minimal dashboard switcher in the header driven by `GET /api/dashboards`. A field-path discovery affordance (even a collapsible "raw paths" list from `/api/telemetry`) would complete the authoring loop.

### [SEVERITY: MEDIUM] Finding 3: `VITE_CSP_TOKEN` can bake the bearer token into the token-free served shell
**File**: apps/telemetry/src/lib/ws.ts:111

**Problem/Evidence**: `return import.meta.env.VITE_CSP_TOKEN ?? ''` is unconditional. Vite embeds any `VITE_*` var present at **build** time into the bundle. The channel is documented as a `vite dev` convenience (ws.ts:13, README), but nothing stops it from running during `npm run build`: a developer who exports `VITE_CSP_TOKEN` in their shell for dev and then builds embeds that run's bearer into `dist/client/assets/*.js`, which csp-server serves token-free at `/assets/*` (http.rs:348).

**Impact**: Directly violates §2.12.3: the token is "**never injected into the served shell** (`/` and `/assets/*` are token-free; a served page carrying the secret would leak the bearer to any same-origin script)". Requires operator error to trigger, hence MEDIUM — but the spec names this exact leak shape, and the failure is silent.

**Suggestion**: Gate on `import.meta.env.DEV` (or `if (import.meta.env.MODE === 'development')`) so the var is dead-stripped from production builds; alternatively rename to `DEV_CSP_TOKEN` and assert absence in `scripts/finalize.mjs`.

### [SEVERITY: MEDIUM] Finding 4: Zero tests and no typecheck/build gate for the SPA anywhere in CI
**File**: apps/telemetry/package.json:5-8, /home/didi/workspace/Clanker-Space-Program/.github/workflows/ci.yml (no node step)

**Problem/Evidence**: `apps/telemetry` contains real logic — reconnect backoff math with budget reset semantics (ws.ts:151-246), the inbound proto gate (ws.ts:186-196), rate-set validation (ws.ts:142-145), dashboard fallback classification across five failure classes (dashboard.ts:74-119), the graph rolling-buffer cap/stack-safe min-max (graph.tsx:17-65) — and has **no test files, no test runner, no `typecheck` script**. `npm run build` is `vite build` (esbuild), which does **not** typecheck; `tsc --noEmit` exists in tsconfig but nothing invokes it. CI (`.github/workflows/ci.yml`, 95 lines) has no node/npm step at all. AGENTS.md is normative: "**No code lands without its test** (§8.7)". (`npx tsc --noEmit` passes today — verified — so this is about the missing gate, not a latent error.)

**Impact**: A type error or a regression in the reconnect/fallback logic reaches the packaged `--serve` surface with no signal; the first human to notice is whoever opens the hosted page after a server restart. The SPA bundle is also a manual build step (`docs/BUILD-AND-DEV.md:336` documents the placeholder-page symptom), so breakage surfaces as "telemetry shows the placeholder" triage.

**Suggestion**: Add `"typecheck": "tsc --noEmit"` and a vitest suite for the pure logic (subscribeTelemetry against a mock WebSocket — the module structure is already test-friendly; loadDashboard against fetch stubs; graph buffer math). One CI job: `npm ci && npm run typecheck && npm test && npm run build`.

### [SEVERITY: LOW] Finding 5: Orbit-map clips near-circular orbits at the top and bottom of the view box
**File**: apps/telemetry/src/lib/widgets/orbit-map.tsx:72 (`const scale = 88 / ra`), :110 (viewBox 200×160, focus at cy=80)

**Problem/Evidence**: The scale is chosen to fit apoapsis horizontally (100 ± 88 ⊂ [12,188]) but the vertical budget from center `cy = 80` is only 80 px in a 160-tall viewBox. For eccentricities below ~0.1 — i.e. **parking orbits**, the common case — `rx = ry = 88/(1+e) > 80`, so the drawn ellipse (and the vessel marker near its vertical extremes) is clipped by 4-8 px top and bottom. A circular orbit renders as a visibly cut ring.

**Impact**: Cosmetic but systematic: the schematic every player sees first (a circular parking orbit after ascent) is the one that renders clipped. The projection math itself is correct (verified against `perifocal_to_inertial` orbit.rs:639: the inverse rotation chain R3(−Ω)·R1(−i)·R3(−ω) plus the `R3(−ω)`-then-flip screen mapping puts the marker exactly on the drawn ellipse).

**Suggestion**: `const scale = 80 / ra` (fits vertically; horizontal slack shrinks harmlessly), or widen the viewBox to `0 0 200 180` and center at 90.

### [SEVERITY: LOW] Finding 6: Reconnect gives up permanently with no manual retry
**File**: apps/telemetry/src/lib/ws.ts:228-244, apps/telemetry/src/routes/index.tsx:100-110

**Problem/Evidence**: After 5 attempts (~15 s of backoff) the client reports `closed — gave up reconnecting` and never tries again; `subscribeTelemetry`'s handle offers no `retry()`. The only in-UI path back is "reset token" → token gate → re-paste the same still-valid token → fresh subscribe. A transient network drop or laptop sleep longer than the window kills the dashboard until the user performs a credential ceremony that wasn't necessary. (The server-restart case genuinely needs re-entry — the token rotates — and the code comments this correctly; the transient case does not.)

**Impact**: A recoverable blip is indistinguishable from a rotated token in the UI; agents keeping the dashboard open during long sessions will hit this.

**Suggestion**: Keep the bounded auto-retry, but render a "reconnect" button in the header whenever `conn === 'closed'` (and for `error` after a protocol refusal) that calls a re-exposed `connect()`; optionally one slow liveness poll (`HEAD /`) to distinguish "server gone" from "token rotated".

### [NIT] Finding 7: `any` at the WS boundary and unsound `as` casts on wire payloads
**File**: apps/telemetry/src/lib/ws.ts:176 (`let msg: any`), apps/telemetry/src/lib/widgets/orbit-map.tsx:35-40 (`as Elements`, `as {x?,y?,z?}`)

**Problem/Evidence**: The single `any` in the codebase is exactly where untrusted JSON enters; downstream narrowing is runtime-guarded (`msg?.proto`, `msg.kind === …`, and every widget `num()`s its fields), so there is no live bug — but a one-line discriminator (`const msg = JSON.parse(...) as { proto?: number; kind?: string; ... }`) or an `unknown` + shape-check would honor the `strict: true` tsconfig at the trust boundary. The orbit-map `as Elements` casts are unsound-but-guarded (a string payload degrades to "open conic", benign).

**Impact**: Type-safety theater at the edge; a future field consumer copying the cast pattern without the `num()` guard would compile clean and crash dirty.

**Suggestion**: Type the inbound envelope minimally; keep `num()` guards.

### [NIT] Finding 8: Inconsistent config-gap messaging in orbit-map; dead export in registry
**File**: apps/telemetry/src/lib/widgets/orbit-map.tsx:42-47 vs :129-131, apps/telemetry/src/lib/widgets/registry.tsx:31

**Problem/Evidence**: A missing `fields[1]` renders the explanatory "position: no fields[1] configured — dashboard gap", but a missing `fields[0]` renders "unknown field undefined" (the template interpolates `undefined`). `WIDGET_KINDS` (registry.tsx:31) is exported and never used in the SPA (the server mirrors the set independently in dashboard.rs:58).

**Suggestion**: Mirror the pos branch's message for `fields[0]`; drop or consume the export.

### [NIT] Finding 9: `num()` helper triplicated across widgets
**File**: apps/telemetry/src/lib/widgets/{gauge.tsx:5-7, graph.tsx:6-8, orbit-map.tsx:19-21}

**Problem/Evidence**: Three byte-identical copies of the finite-number guard; `lib/utils.ts` exists (7 lines) and is the natural home.

**Suggestion**: Move to `~/lib/utils` and import. (DRY report: this is the only duplication of note — ws.ts and dashboard.ts are single-sourced and clean; no CRAP/complexity concerns anywhere in the SPA.)

---

Follow-ups (out of C28 scope, noted once): `http.rs:52-56` doc comment claims "`spa_dir` has no binary wiring yet … `/` serves the placeholder in production today" — stale since e37369e; `csp-app/src/lib.rs:2995-2997` sets it under `--serve` (C27's file). `SimEvent`/notice toasts (PartFailed, IgnitionMisfire) have no wire representation, so the dashboard cannot show *why* a flight went wrong — a plausible §2.12.2 extension, not a v1 violation. MessagePack-for-arrays deferral is documented in-code with the `ws_subscription_stays_within_byte_budget` e2e tripwire (e2e.rs:988) — acceptable while no widget subscribes per-part vectors, but a dashboard subscribing `vessels.0.parts` on a large vessel ships it as JSON today.

## PRD coverage table (§2.12.2/§2.12.3 requirements → SPA status)

| §2.12 requirement | Status | Evidence |
|---|---|---|
| TanStack Start SPA in `apps/telemetry/` | **WIRED** | static `spa` build + `finalize.mjs` → served verbatim by `--serve` (csp-app lib.rs:2995) |
| Exact version pins, no carets | **WIRED** | package.json — every dep an exact pin (verified) |
| Dashboards = sidecar config, `$config_dir/csp/dashboards`, `schema_version`, never in save | **WIRED** | server store + SPA loader with per-failure-class notices (dashboard.ts:74-119) |
| Subscription model: client declares paths, no "send everything" | **WIRED** | union of widget fields, dedup+sort matching server BTreeSet; 64-field cap both sides |
| Rate: 64-divisors only, default 16 | **WIRED** | VALID_RATES mirrored client+server; bad stored `rate_hz` degrades locally before send |
| Encoding: JSON scalars / MessagePack arrays | **PARTIAL (documented deferral)** | JSON-only; no widget reads bulk arrays; budget tripwire e2e exists |
| Backpressure: drop-to-latest, never queue | **WIRED** | server drops sends; SPA paints only `frame.current` via one rAF per burst |
| Token bootstrap: `?csp_token=` → sessionStorage → stripped; bearer `/api`; `?token=` `/ws` only | **WIRED** (spec-exact) | ws.ts:70-132, http.rs guard matches |
| Vessel addressing robust to id churn | **MISSING** | Finding 1 — `vessels.0` hardcoded; `focus.*` never published (dead server code) |
| Progression mirrored on the dashboard (funds/contracts/crew/colonies per Game UI) | **MISSING** | Finding 2 — `program.*` subscribable, zero widgets |
| User-defined dashboards displayed (§4 M9d row) | **DEGRADED** | Finding 2 — store holds 64, SPA can load only hardcoded `default` |
| Opt-in command channel (§2.12 table) | **WIRED (server) / unused (SPA)** | `kind:"command"` → SimCommand on `/ws`; SPA never sends — acceptable, MCP is the control surface |

## Top 3 issues
1. **[CRITICAL] Finding 1** — dashboard hardcodes `vessels.0.*`; ids never reused; after the first `RecoverVessel` the hosted surface is permanently "unknown field", and the `focus.*` fix is dead code on the server.
2. **[HIGH] Finding 2** — `program.*` (funds/science/contracts/crew/colonies) exists on the wire (added for exactly this, report-19 F3) and the dashboard renders none of it; only `default` is ever loadable.
3. **[MEDIUM] Finding 3** — `VITE_CSP_TOKEN` present at build time bakes the bearer into the token-free served shell, the exact leak §2.12.3 forbids.
