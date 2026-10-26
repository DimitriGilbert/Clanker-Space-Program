# PRD-20 Review — Telemetry SPA (`apps/telemetry`)

**Files reviewed:** `apps/telemetry/src/**` (ws.ts 254, dashboard.ts 119, routes/index.tsx 183, __root.tsx 24, router.tsx 12, widgets/registry.tsx 31, gauge.tsx 50, graph.tsx 91, table.tsx 39, orbit-map.tsx 136, card.tsx 22, styles.css 142, routeTree.gen.ts 68), `apps/telemetry/dashboards/default.json` (77), `package.json`, `vite.config.ts`, `tsconfig.json`, `scripts/finalize.mjs`, `README.md` — plus the server side of the contract: `crates/csp-server/src/{telemetry.rs, http.rs (/ws + /api/dashboards), dashboard.rs, serve.rs}`, `crates/csp-server/PROTOCOL.md`, `crates/csp-server/tests/e2e.rs`, `crates/csp-sim/src/scripts.rs` (`VesselTelemetry`), `crates/csp-data/src/system.rs` (`KeplerElements`), and consumer/wiring greps across `csp-app`, `csp-view`, `csp-ui`, `csp-bench`.

**Verdict:** The SPA is a small, unusually honest telemetry client — the wire protocol mapping is field-for-field faithful to `csp-server`, staleness/unknown-field/type-mismatch states are all surfaced rather than papered over, and the widgets carry real "don't draw what isn't true" discipline. Two structural problems dominate: **(1) nothing in any shipped binary starts `csp-server`, so the entire M9d surface — SPA included — is unreachable dead code for players**, and **(2) the wire snapshot is strictly flight telemetry (`{tick, warp_level, vessels}`), so the answer to the PRD's worry — "could the telemetry SPA accidentally be a better program view?" — is no: it is strictly narrower than the game UI.** Colonies/crew/contracts/funds/science/alarms are not merely unwidgeted, they have no field paths at all. The shipped dashboard additionally hard-codes `vessels.0`, which can silently paint the wrong vessel's telemetry as live truth.

---

## Findings

### F1 — [HIGH] No shipped binary ever starts `csp-server` — the whole telemetry surface is unreachable

**Where:** `crates/csp-server/src/serve.rs:28–59` (`ServerConfig`; `spa_dir` defaults `None`); `crates/csp-server/tests/e2e.rs:39,64,100,234,322,378,1187` (the only `Server::start` call sites in the repo); `crates/csp-app/Cargo.toml:17–28` (deps: `csp-core/render/sim/view` — **no `csp-server`**); `crates/csp-bench/src/bin/csp-mcp.rs:27–28,103` (uses `backend`/`mcp`/`dashboard` modules, never `serve::Server`); `crates/csp-app/src/settings_io.rs:401` (only mention of csp-server in the game app — a comment).

**Problem:** `Server::start` is invoked only from e2e tests. The `csp` game binary does not depend on `csp-server` at all; `csp-mcp` builds its own headless sim and speaks stdio, never binding HTTP. `csp-server`'s own test comment concedes the narrower version: boot_spa is "the M9d SPA-serving path no binary wires up yet" (`e2e.rs:77–79`). Even if a binary did call `Server::start`, `ServerConfig::default().spa_dir == None` serves the placeholder page, not `apps/telemetry/dist/client`.

**Evidence:** `grep -rn "Server::start" --include="*.rs"` outside `crates/csp-server/{src,tests}`: zero hits. `grep -rln "csp_server" crates/csp-app crates/csp-view`: zero hits.

**Impact:** §2.12's four-client table lists "Telemetry web screens" as a v1 client (§4 row "KSP web-server mods → v1 (M9d)"), and the SPA + server + protocol are fully implemented — ~2,600 lines of working code with a built `dist/` — yet there is no sequence of player actions that opens a telemetry page. Same reachability disease as PRD-18's scripts/autopilots, one layer up: the feature exists end-to-end and is connected to nothing.

**Suggestion:** Native `csp-app` builds should host `Server::start` behind a settings/CLI flag (default `ServerConfig` is already correct: `127.0.0.1:47900`, token file under `$XDG_RUNTIME_DIR/csp/`, `config_dashboard_dir()`), with `spa_dir` pointed at a packaged `dist/client` (or a build step that copies it next to the binary). The `ControlBackend` the server needs is the `GameHost` the app already owns. Until wired, mark M9d as not-player-reachable in any status tracking — it currently reads as done.

---

### F2 — [MEDIUM] `vessels.0` is hard-coded everywhere and the wire has no focused/active-vessel path — the dashboard can confidently display the wrong vessel

**Where:** `apps/telemetry/dashboards/default.json:11,19,27,36–39,48,56,64,72` (every widget field is `vessels.0.*`); `crates/csp-server/src/telemetry.rs:32` (`vessels: BTreeMap<u64, SnapshotVessel>` — keyed by `VesselId.0`, allocated in creation order); `crates/csp-sim/src/lib.rs:4251,4336,4566` (vessels are *removed* from the arena on crash/dock-absorb/recover); `crates/csp-view/src/lib.rs:1919` (`MapInput.focus_vessel` — the game's focus concept, with no mirror in `TelemetrySnapshot`).

**Problem:** `vessels` serializes as a JSON object keyed by vessel id, so `vessels.0.altitude_m` resolves only when a vessel with id 0 exists — and id 0 is the *first-created* vessel, not "the player's vessel". Three failure shapes:

- **Wrong vessel, silently live:** in a multi-vessel world (debris splits, a second launch, docking spawns), if the player flies vessel 3, every gauge/graph/orbit-map paints vessel 0's telemetry in perfect health. Nothing on the page says which vessel is being watched.
- **Dead vessel, all-unknown:** if vessel 0 is destroyed or recovered, every widget shows "unknown field" — honest, but the dashboard is bricked until someone edits JSON.
- **No fix exists:** there is no `active_vessel`/`focus` field in `TelemetrySnapshot` to subscribe to, no vessel list exposed except subscribing to the whole `vessels` object into a table cell, and no picker in the UI. `vessels.0.found` (a real field — `scripts.rs:333`) could at least flag "this path names a live vessel" but the default dashboard doesn't use it.

**Impact:** A telemetry dashboard whose core contract is "this is the ship you're flying" can display a different ship's state, live, with zero indication — the worst honesty failure available to this codebase's stated values. The game-side `focus_vessel` exists; the wire simply doesn't export it.

**Suggestion:** Add `focus_vessel_id` (mirroring `MapInput.focus_vessel`/flight focus) to `TelemetrySnapshot`, plus either a `vessels.<focus>.…` alias resolved server-side or a per-page vessel selector bound from a `vessels` listing subscription. Minimum viable honesty patch: subscribe `vessels.0.found` in the default dashboard and paint "vessel 0 not present — edit field paths" when it's absent/false.

---

### F3 — [MEDIUM] Program data is structurally unreachable — the SPA cannot be an accidental better program view (it's strictly narrower)

**Where:** `crates/csp-server/src/telemetry.rs:25–33` (`TelemetrySnapshot = {tick, warp_level, vessels}` — the entire wire vocabulary); `crates/csp-sim/src/scripts.rs:328–382` (`VesselTelemetry` — pos/vel/rot/throttle/apsides/elements/maneuver_node only); `crates/csp-sim/src/lib.rs:1421` (`World.colonies` — a separate map, never serialized into telemetry).

**Problem:** The subscription mechanism is a generic dotted-path projection over the serialized snapshot, so reachability is exactly the snapshot's shape. Colonies, crew, contracts, funds, science, tech, alarms, script/autopilot state are **not in the snapshot** — no field path can name them, and no dashboard JSON can compensate. Compare the game UI, which since `crates/csp-ui/src/program.rs` covers funds/science/contracts/research/reliability/vessel-science. So the PRD question inverts: rather than risking an accidental better program view, the telemetry SPA is missing every program surface the game UI *has*, and cannot surface the ones it lacks (colonies, crew, alarms, script/autopilot status — the same hidden-command set PRD-18/19 flagged).

The two subscribable fields that do exceed the game UI:
- `vessels.N.control_lease` — in the shipped default dashboard's "World" table (`default.json:72`). `csp-ui`/`csp-view` never display the lease (grep: only `csp-view/src/lib.rs:1572,1915` internals). An agent/script silently holding control is visible *only* on this tablet — the one honest program-adjacent surface, and it's accidental.
- `vessels.N.maneuver_node` — subscribable (`scripts.rs:381`) but not in the shipped dashboard.

**Impact:** §2.12.2's promise is "telemetry screens", so this is arguably in-scope-by-omission — but it means M9d shipped is a *flight* dashboard only, and the "user-defined dashboards" escape hatch can never grow program widgets without a `TelemetrySnapshot` extension. Given the game UI's gaps (colonies/crew/alarms/autopilot/scripts per PRD-18/19), extending the snapshot is the cheapest honest way to give players *any* view of that state — the plumbing already exists.

**Suggestion:** Extend `TelemetrySnapshot` with a `program` object (funds, science, contract summary, tech state), `colonies` (id, body, modules, stockpiles), and `vessels.N.crew`/alarm/script status. Every addition is immediately subscribable — the generic projection is the one part of this stack with no reachability problem.

---

### F4 — [LOW–MEDIUM] Graph sparkline joins samples contiguously across a hidden-tab gap — the dropout the code promises never to draw

**Where:** `apps/telemetry/src/lib/widgets/graph.tsx:28–41` (samples appended in `useEffect`), `apps/telemetry/src/routes/index.tsx:52–57` (render coalescing via `requestAnimationFrame`).

**Problem:** The widget clears its buffer on `frame === null` precisely so "the sparkline must not draw a continuous line across a dropout" (`graph.tsx:31–33`). But samples are appended per *render*, and renders are rAF-gated. In a hidden/backgrounded tab `requestAnimationFrame` does not fire while WS frames keep arriving into `frame.current`; the socket never closes, so `frame === null` never triggers. On tab return, exactly one new sample appends — minutes of missing data drawn as a continuous line, the exact artifact the buffer-clear exists to prevent. Secondary effect at any visibility: at a 64 Hz subscription on a ~60 Hz display, render coalescing silently downsamples the sparkline to render cadence (documented drop-to-latest, but unlabelled).

**Suggestion:** Append to the buffer at frame receipt (in the `onFrame` path / a subscription keyed outside React render), or keep the render-time append but detect gaps via the frame's `seq`/`tick` deltas (`TelemetryFrame.seq` is already carried — `ws.ts:24`) and insert a break point (NaN/segment split) before joining post-gap samples.

---

### F5 — [LOW] Orbit-map clips near-circular orbits off the top and bottom of its viewBox

**Where:** `apps/telemetry/src/lib/widgets/orbit-map.tsx:70–77,109` (`scale = 88 / ra`; `viewBox="0 0 200 160"`, focus at y=80).

**Problem:** The scale fits *apoapsis* horizontally (apoapsis always lands at x = 12 of the 200-wide box), but the minor radius `b·scale = 88·√((1−e)/(1+e))` exceeds the box's 80-unit half-height for `e ≲ 0.1` — an `e = 0` orbit draws a radius-88 circle in an 80-half-height box, clipping ~8 units top and bottom. Near-circular orbits are the most common family the widget will ever draw. (The ellipse/vessel-marker projection math itself checks out — the `R3(Ω)·R1(i)·R3(ω)` inverse map reduces correctly to `(x, −y)` for equatorial orbits and stays consistent with the ellipse's `rotate(−ω)` for `ω ≠ 0`; the documented dropped-out-of-plane caveat is honest.)

**Suggestion:** `const scale = Math.min(88 / ra, 76 / b)` (leave headroom for the body disc and the marker radius).

---

### F6 — [LOW] No dashboard picker and no save path — "user-defined widget dashboards" (§4) can't be authored in the app that renders them

**Where:** `apps/telemetry/src/routes/index.tsx:35` (`loadDashboard('default')` — hard-coded), `apps/telemetry/src/lib/dashboard.ts:74` (GET only; no `PUT` anywhere in the client), `crates/csp-server/src/http.rs:202–205,505–554` (`GET /api/dashboards` list and `PUT /api/dashboards/{name}` both exist and are exercised by e2e).

**Problem:** The server supports up to 64 named dashboards (`dashboard.rs:46`, `MAX_DASHBOARDS`) and a validated write path; the SPA reads exactly `default`, forever. A stored `flight`/`evacomms` dashboard is unreachable from the UI without renaming files. There is also no editor: `rect` is display-only (no drag/resize), and widgets can't be added — "user-defined" means hand-editing `$config_dir/csp/dashboards/*.json` or driving the MCP `dashboard.save` session tool (`PROTOCOL.md` tools section). The router has a single `/` route (`routeTree.gen.ts`), so there's no `?dashboard=`/`/d/:name` surface either.

**Impact:** §4's v1 row is "Telemetry screens + user-defined widget dashboards". The "+" half is a file-system affordance, not an app feature — likely an accepted v1 cut, but it is undocumented as one.

**Suggestion:** Cheap version: a `GET /api/dashboards` dropdown + `?dashboard=name` route param (both endpoints already exist). Editor: defer, but write it down in PLAN §6.4's cut list if that's the intent.

---

### F7 — [LOW] The opt-in `command` channel is implemented server-side but no client of this SPA uses it

**Where:** `crates/csp-server/src/http.rs:851–889` (`{kind:"command"}` → `command_ack` fully implemented); `apps/telemetry/src/lib/ws.ts` (never sends `kind:"command"`; `command_ack` is parsed-then-ignored by the kind dispatch at `ws.ts:197–215`).

**Problem:** §2.12 says the telemetry screens are "mostly read; opt-in command channel". The opt-in exists on the wire; nothing in the SPA can send even a `SetWarp`/`SetThrottle` — it's read-only in practice. Consistent with "mostly read", so a scope note rather than a defect: worth stating in the README that the command channel is intentionally unused by this SPA.

---

### F8 — [LOW] e2e "mirror" of `default.json` is a hand-maintained superset — default-dashboard drift cannot be caught

**Where:** `crates/csp-server/tests/e2e.rs:1024–1048` — the field array is commented "the field set the shipped default dashboard subscribes — apps/telemetry's default.json in mirrored form", but it contains `horizontal_speed_mps`, `vel_mps`, and `maneuver_node`, which `apps/telemetry/dashboards/default.json` does **not** subscribe, and omits nothing. It asserts those paths resolve on the wire (valuable), but nothing verifies the shipped `default.json` actually parses, fits caps, or resolves — the file is bundled client-side and never passes through `DashboardStore::validate`.

**Suggestion:** Add a test that reads `apps/telemetry/dashboards/default.json` (or embeds it), runs `Dashboard::validate` + `subscription_fields`, and projects it against a populated world asserting `unknown_fields.is_empty()`. Also worth adding `time_to_periapsis_s` to the default "Orbit" table — `time_to_apoapsis_s` alone is an odd authoring asymmetry (`default.json:38`).

---

## Honest-representation audit (what the code gets right — verified, not skimmed)

- **Wire protocol fidelity** (`ws.ts` vs `http.rs:685–895`): envelope `{proto, kind, fields, rate_hz}` matches; `subscribed`/`telemetry`/`error`/`command_ack` shapes match; `unknown_fields` is carried per frame; the inbound `proto` gate marks the frame stale *and* errors (`ws.ts:186–196`) rather than dispatching on `kind` alone; `rate_hz` is client-clamped to `VALID_RATES` before send (`ws.ts:142–145`) matching the server's refusal set.
- **Staleness discipline**: `onFrame(null)` on close/error/proto-break/refusal (`ws.ts:190,213,220,226`) — a dead stream can never keep painting live-looking data; widgets render `—`/blank/explicit-stale, never last-values. `orbit-map.tsx:54–58` distinguishes "stale — no live frame" from "no conic — vessel not on rails".
- **Missing-data honesty**: `unknown_fields` → red "unknown field" labels in all three scalar widgets; `gauge.tsx:25–30` and `graph.tsx:78–84` distinguish *non-numeric* (type mismatch) from *absent* — a subtle and correct distinction.
- **Domain honesty**: hyperbolic conics refused with reason (`orbit-map.tsx:62–66`); `mass_kg`/`altitude_m` `Option` fields render `—`; the control-lease enum renders verbatim (`{"Agent":3}`).
- **Robustness already fixed**: `MAX_POINTS` buffer cap + loop min/max (`graph.tsx:17,55–65` — C21-F1), rAF render coalescing (C21-F2), reconnect budget reset only on live ack (`ws.ts:204–208`), token stripped from the address bar via `replaceState` and kept to sessionStorage (`ws.ts:77–89`), token-free served shell honored end-to-end (`http.rs:407–426`), exact-pinned deps with no carets (`package.json` — §2.12.2's pinning rule ✓), server-validated dashboard schema makes the client's `as Dashboard` cast safe for any `/api/dashboards` 200 (`dashboard.rs:256–339`).

## Minor notes (no action needed)

- `graph.tsx:17–25`: at `rate_hz=64`, `window_s:60` silently shows ~32 s (the 2048-point cap clamps the buffer, not the option) — documented in the comment; fine.
- Sparkline x-axis is frame-index, not sim-time — under timewarp `window_s` wall-seconds covers wildly different sim spans; acceptable for a sparkline, no labels to mislead.
- `README.md` says `npm install`; `npm ci` against the committed `package-lock.json` is the §2.12.2-honest instruction.
- `vite.config.ts:24` uses `resolve.tsconfigPaths: true` for the `~/*` alias — works (the committed `dist/` proves the build), just flagging that it's the non-obvious resolution path for `~/lib/…` imports.

## Summary of severities

| # | Severity | Finding |
|---|---|---|
| F1 | HIGH | No binary starts `csp-server` — M9d telemetry surface is unreachable dead code |
| F2 | MEDIUM | `vessels.0` hard-coded; no focused-vessel path — can silently show the wrong vessel |
| F3 | MEDIUM | Program data unreachable by construction — SPA is narrower than the game UI, not a secret better one |
| F4 | LOW–MED | Graph joins a contiguous line across hidden-tab gaps |
| F5 | LOW | Orbit-map clips `e ≲ 0.1` ellipses vertically |
| F6 | LOW | No dashboard picker/PUT — "user-defined" is file-editing only |
| F7 | LOW | `command` channel exists server-side, unused by the SPA |
| F8 | LOW | e2e field list is a stale-able mirror superset; `default.json` never validated |
