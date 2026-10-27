# Verified report C28 — Telemetry SPA (`apps/telemetry`)

Verification of `review-report-C28.md` against the working tree (2026-10-26). Every finding was
re-checked at the cited `file:line`, traced through surrounding code, and where possible verified
empirically (`npx tsc --noEmit` re-run; orbit-map clip math recomputed; VesselId allocation traced
through sim tests and `apply_recover_vessel`; `from_world_focus` caller search run across
`crates/`, `apps/`, `xtask/`).

**Verdict: 9 confirmed, 0 dismissed.** The CRITICAL stands.

---

### Finding 1: Default dashboard goes dark after first vessel recovery — `vessels.0.*` hardcoded, `focus.*` never published — CONFIRMED

**Original summary**: Every widget subscribes `vessels.0.*`; ids are monotonic and never reused;
after the first `RecoverVessel` the second launch is `vessels.1` and the widgets show "unknown
field" forever; the server's `focus.*` alias (`from_world_focus`) has zero callers.

**Verification** — all four legs independently confirmed:

1. **Hardcoded refs.** `apps/telemetry/dashboards/default.json` — all 8 widgets reference
   `vessels.0.*` (lines 11, 19, 27, 36-39, 48, 56, 64, 72). No widget subscribes `focus.*`,
   `focus_vessel_id`, or `vessels`.
2. **Id monotonicity.** `crates/csp-sim/src/lib.rs:1402` (`pub next_vessel_id: u64`),
   `:2504` (`next_vessel_id: 0` in `World`'s initializer), allocation sites `:5834-5840`,
   `:6121-6145`, `:7405-7407` plus `life_support.rs:514`, `colonies.rs:4236`, `economy.rs:1073`,
   `manufacture.rs:343` — all do `VesselId(next_vessel_id)` then `checked_add(1)`. Nothing ever
   decrements or reuses it (grep over the whole crate). Test `spawn_vessel_creates_and_schedules`
   (lib.rs:12626-12633) confirms the first vessel is `VesselId(0)`.
3. **Removal on recovery.** `SimCommand::RecoverVessel` (lib.rs:5387) →
   `economy.rs:880 apply_recover_vessel` — it credits funds/science (the recovery loop's payoff)
   and ends with `self.remove_vessel(vessel, shadow)` (economy.rs:921), which removes the vessel
   from the arena (lib.rs:4997 `remove_vessel` → `self.vessels.remove(&vessel)` at :5016).
   Breakup removes too (lib.rs:11573 `vessels.remove(&vid)`). So launch → recover → next launch
   yields `vessels.1`, and every `vessels.0.*` path resolves to `None`.
4. **`focus.*` is dead server code.** `rg "from_world_focus|from_world\b"` across
   `crates/`, `apps/`, `xtask/`: the ONLY caller of `from_world_focus` (telemetry.rs:128) is
   `from_world` (telemetry.rs:121-122) passing `focus: None`. Every publish point —
   `backend.rs:643` (`LocalGame::new`), `:710` (`LocalGame::publish`), `:856-886`
   (`GameHost::spawn_full`'s loop, the host `csp --serve` actually uses per
   `csp-app/src/lib.rs:2978-2997` `GameHost::spawn_full` + `run_blocking`), and `csp-bench` —
   calls `from_world`. No `focus`-setting code exists anywhere in `csp-server` (grep for "focus").
   The struct doc (telemetry.rs:33-40) literally says focus exists "so a dashboard can name
   `focus.altitude_m` instead of hard-coding `vessels.0`" — and it is never populated.

**Failure mode verified end-to-end**: http.rs:745-749 `snap.project(fs)` returns unresolvable
paths in `unknown_fields`, shipped on every telemetry frame; ws.ts:197-203 surfaces them;
gauge.tsx:19-24, table.tsx:17-25, graph.tsx:43-75, orbit-map.tsx:42-48 all render
"unknown field …". Subscription fields are fixed at subscribe time (index.tsx:39-41); no vessel
picker, no field picker, no dashboard-name routing exists in the SPA; the only recovery is
out-of-band `PUT /api/dashboards/default` (http.rs:529) with paths rewritten blind to the live id.

**One count nuance (immaterial)**: 8 of 8 widgets reference `vessels.0.*`; 7 go fully dark, the
8th ("World") keeps `tick`/`warp_level` and shows "unknown field" on its 2 vessel rows. The
report's "7 of 8" is accurate for fully-dark widgets; "every widget" (its File line) is the
stricter truth.

Severity CRITICAL is justified: recovery is the standard progression endpoint
(`ProgramCommand::Recover` in csp-ui/src/program.rs:2385, recovery credit in economy.rs), not an
edge case, and the hosted `--serve` dashboard has no in-band recovery.

### Finding 2: `program.*` on the wire but zero widgets; `loadDashboard('default')` hardcoded; stored dashboards unreachable — CONFIRMED

**Original summary**: `ProgramSnapshot` fields are subscribable but the bundled default is
flight-only; `GET /api/dashboards` is never called; only `default` is loadable; §4 M9d degraded.

**Verification**:
- `crates/csp-server/src/telemetry.rs:52-73` — `ProgramSnapshot` carries `funds_micros`,
  `science_banked`, `contract_offers`, `contracts_active`, `tech_researched`, `crew_roster`,
  `colonies[]`, `scripts[]`, `autopilots[]`, `sites`, all projected through the same dotted-path
  `project()`. Comment cites "report-19 F3" as the reason it was added.
- `apps/telemetry/dashboards/default.json` — zero `program.*` paths; title "Flight Telemetry".
- `apps/telemetry/src/routes/index.tsx:35` — `loadDashboard('default')`, the only call site of
  `loadDashboard` in the SPA. `grep -rn "api/dashboards" apps/telemetry/src` shows only
  `dashboard.ts:77` (`/api/dashboards/${name}`); the list route (`http.rs:202` → `dashboard_list`
  at :505) is never called from the SPA.
- `crates/csp-server/src/dashboard.rs:49` `MAX_DASHBOARDS: usize = 64`; `list()` at :189-207 —
  the store can hold and list 64 dashboards the SPA can render but never load (no URL param, no
  switcher; `DashboardSource`/notice plumbing supports it, nothing invokes it).
- PLAN.md §4 line 1362 (verified inside "## 4. KSP-mod-equivalent features", header at :1349):
  "| KSP web-server mods | Telemetry screens + user-defined widget dashboards | **v1 (M9d)** |"
  — the report's quote is exact. M9d row at :1508 includes "sidecar dashboard config".
- §6.4 cut list (reordered) read in full: the five items are R3, life support, Duva, docking,
  web-tier polish — nothing sanctions leaving progression off the dashboard. Confirmed.
- No field browser exists anywhere in the SPA (all of `src/` read: token gate + dashboard render
  only), so `program.*` paths are undiscoverable from the UI. Confirmed.

Severity HIGH as a spec-degradation finding is consistent with the PLAN's normative rows.

### Finding 3: `VITE_CSP_TOKEN` can bake the bearer into the token-free served shell — CONFIRMED

**Original summary**: ws.ts:111 reads `import.meta.env.VITE_CSP_TOKEN` unconditionally; if the
var is set at `vite build` time the token lands in `dist/client/assets/*.js`, which csp-server
serves token-free — the exact §2.12.3 leak.

**Verification**:
- `apps/telemetry/src/lib/ws.ts:111` — `return import.meta.env.VITE_CSP_TOKEN ?? ''`; no
  `import.meta.env.DEV`/MODE gate anywhere in the file. The access is the statically-analyzable
  dotted form Vite define-replaces at build, so a value present in the build environment becomes
  a string literal in the bundle.
- No guard exists downstream: `scripts/finalize.mjs` only promotes `_shell.html` → `index.html`
  (12 lines, no content assertion); nothing else checks the built assets.
- Token-free serving confirmed: `crates/csp-server/src/http.rs:346-349` — `if path == "/" ||
  path.starts_with("/assets/") { return next.run(req).await; }` (skips the token guard).
- §2.12.3 quote verified at PLAN.md:883-886: "**never stored in the save and never injected into
  the served shell** (`/` and `/assets/*` are token-free; a served page carrying the secret would
  leak the bearer to any same-origin script)". The report's quotation is verbatim.
- Exploit shape holds: README.md:28 instructs devs to export `VITE_CSP_TOKEN` for
  `npm run dev`; `npm run build` (package.json:7) runs `vite build` in the same shell
  environment. If the dev's server instance is still running when they build, the baked token is
  live and readable by any process fetching the unauthenticated `/assets/*.js`.

Confirmed at MEDIUM (conditional on operator error, silent, spec-named leak shape).

### Finding 4: Zero SPA tests, no typecheck/build gate in CI — CONFIRMED

**Original summary**: Real logic (reconnect backoff, proto gate, rate validation, fallback
classification, graph buffer) with no tests, no test runner, no `typecheck` script; CI has no
node step; AGENTS.md §8.7 is normative.

**Verification**:
- `apps/telemetry/package.json:5-9` — scripts are exactly `dev`, `build`, `preview`. No
  `test`, no `typecheck`. `find apps/telemetry -name "*.test.*" -o -name "*.spec.*" -o -name
  "vitest.config.*"` (node_modules excluded): no hits.
- `.github/workflows/ci.yml` (96 lines, read in full): `check` job is fmt/clippy/test/wasm/
  xtask-lints/cargo-deny/goldens; `perf` job is csp-perf. Zero node/npm/actions-setup-node
  steps; grep for npm across workflows and `xtask/src` shows nothing but a tree-drawing comment.
- AGENTS.md:80-81: "No code lands without its test (§8.7)"; PLAN.md:1862: "1. **No code lands
  without its tests.**" — norm confirmed.
- `npx tsc --noEmit` in `apps/telemetry` re-run during this verification: exit 0, no output —
  the report's "passes today" claim reproduces. `vite build` uses esbuild transpile-only, so a
  type error indeed ships silently; docs/BUILD-AND-DEV.md:336 troubleshooting row ("Telemetry SPA
  shows the placeholder page … npm run build") confirms the manual-build symptom the report cites.

### Finding 5: Orbit-map clips near-circular orbits top/bottom — CONFIRMED

**Original summary**: `scale = 88/ra` fits apoapsis horizontally (12..188 in 0..200) but the
vertical budget from `cy=80` is 80px; for e < ~0.1 the ellipse (and marker) clips by up to 8px;
a circular parking orbit renders as a cut ring.

**Verification** (math recomputed):
- `orbit-map.tsx:72` `const scale = 88 / ra`; `:109` `viewBox="0 0 200 160"`; focus at
  (100, 80). `rx = ry = 88/(1+e)`: e=0 → 88.00px (clip 8.00px); e=0.02 → 86.27 (6.27);
  e=0.05 → 83.81 (3.81); e=0.08 → 81.48 (1.48); e=0.1 → 80.00 (0). SVG default overflow is
  hidden, so the drawn ellipse is visually clipped. The vessel marker rides the ellipse, so it
  clips at the vertical extremes too.
- Minor numeric nit (immaterial): the report's "clipped by 4-8 px" describes e ≲ 0.05; between
  0.05 and 0.1 the clip is 0-4px. The substance — circular parking orbits, the first thing every
  player sees, clip by a full 8px — is exact.
- The projection-math exoneration also holds: the marker chain in orbit-map.tsx:91-106 is the
  correct inverse of `perifocal_to_inertial` (crates/csp-sim/src/orbit.rs:639,
  R3(Ω)·R1(i)·R3(ω)): `x1 = co·x + so·y, y1 = −so·x + co·y` is R3(−Ω); `y2 = ci·y1 + si·z` keeps
  the in-plane row of R1(−i); `xp = cos·x2 + sin·y2, yp = −sin·x2 + cos·y2` is R3(−ω); and the
  screen mapping `vx,vy` equals R3(−ω)·(xp, −yp), matching the ellipse's `rotate(−omega)`
  transform with y-down screen axes. Spot-check passed.

### Finding 6: Reconnect gives up permanently, no manual retry — CONFIRMED

**Original summary**: After 5 attempts (~15 s) the client reports "gave up reconnecting" and
never retries; no `retry()` on the handle; the only UI path back is "reset token" → re-paste.

**Verification**: ws.ts:228-244 — once `attempts === maxAttempts` (5, DEFAULT_RECONNECT:64-68)
the else arm emits `('closed', 'gave up reconnecting')` and no further timer is set; the
returned close handle (ws.ts:249-253) only stops. Backoff sum: 0.5+1+2+4+8 = 15.5s ≈ "~15 s"
(confirmed). index.tsx:100-110 — the only header affordance is the "reset token" button, which
clears the token and re-runs the effect via the `[token]` dependency → token gate → re-paste →
fresh subscribe. The server-restart caveat comment (ws.ts:57-60) correctly notes token rotation;
a transient drop does not need the ceremony. Confirmed as described.

### Finding 7: `any` at the WS boundary, unsound `as` casts in orbit-map — CONFIRMED

**Original summary**: ws.ts:176 `let msg: any` at the untrusted-JSON entry; orbit-map.tsx:35-40
`as Elements` / `as {x?,y?,z?}` casts; downstream narrowing is runtime-guarded so no live bug.

**Verification**: ws.ts:176 is literally `let msg: any`; the only other `any` in `apps/telemetry/src`
is routeTree.gen.ts:18 (TanStack-generated, not hand-written), so "the single any in the
codebase" is fair. The casts are at orbit-map.tsx:35-40 exactly as cited; every consumer runs
`num()` guards (orbit-map.tsx:49-53, 87-90), and a non-object payload degrades to the "open
conic"/"no conic" branches. No live defect; nit-grade as reported. Confirmed.

### Finding 8: Inconsistent config-gap messaging; dead export — CONFIRMED

**Original summary**: Missing `fields[1]` renders the explanatory "position: no fields[1]
configured — dashboard gap" but missing `fields[0]` renders "unknown field undefined";
`WIDGET_KINDS` (registry.tsx:31) is exported and unused in the SPA.

**Verification**: orbit-map.tsx:42-48 — when `elementsPath === undefined` the template renders
`unknown field undefined` (interpolated `undefined`), vs the deliberate branch at :129-131 for
`posPath === undefined`. grep for `WIDGET_KINDS` in `apps/telemetry/src`: only the declaration
(registry.tsx:31) and a comment reference — zero uses; the server keeps its own independent list
(dashboard.rs:58 `pub const WIDGET_KINDS: &[&str]`). Confirmed.

### Finding 9: `num()` helper triplicated — CONFIRMED

**Original summary**: Three byte-identical finite-number guards across gauge/graph/orbit-map;
`lib/utils.ts` is the natural home.

**Verification**: gauge.tsx:5-7, graph.tsx:6-8, orbit-map.tsx:19-21 — all three are
`return typeof v === 'number' && Number.isFinite(v) ? v : null`, byte-identical.
`lib/utils.ts` is the 7-line `cn()` helper. Confirmed.

---

## Follow-ups (from the report's closing note) — verified

- `http.rs:52-56` doc comment ("`spa_dir` has no binary wiring yet … `/` serves the placeholder
  in production today") is confirmed stale: `csp-app/src/lib.rs` `serve_cli` sets
  `spa_dir: Some(packaged_spa_dir())` under `--serve`.
- `ws_subscription_stays_within_byte_budget` e2e exists at
  `crates/csp-server/tests/e2e.rs:988` (16 KiB/frame at 16 Hz assertion) — the MessagePack
  deferral note is accurate.

## WIRED-claim spot-checks (requested)

- **Wire protocol spec-exact**: §2.12.2 (PLAN.md:844-866) specifies envelope
  `{ proto: 1, kind, seq, tick, payload }`, client-declared field paths, 64-divisor rates
  default 16, drop-to-latest. ws.ts sends `{proto: WIRE_PROTO, kind:"subscribe", fields,
  rate_hz}` (ws.ts:166-173), `VALID_RATES = [64,32,16,8,4,2,1]` (ws.ts:20) matches, the server
  mirrors (http.rs:745-749 frame shape, MAX_FIELDS=64 at http.rs:89, VALID_RATES in
  dashboard.rs). Spec-exact — the report's WIRED rows hold.
- **Token bootstrap**: ws.ts:77-89 (`?csp_token=` → sessionStorage → `replaceState` strip),
  Bearer on `/api` (dashboard.ts:43-52), `?token=` only on `/ws` (ws.ts:148; http.rs:351-356
  permits `?token=` on `/ws` alone) — matches PLAN.md:886-888 verbatim.
- One imprecision in the coverage table (not a numbered finding): "64-field cap both sides" —
  both enforcement points are server-side (dashboard store `MAX_SUBSCRIPTION_FIELDS=64` at
  dashboard.rs:63-65 on PUT, and `MAX_FIELDS=64` at http.rs:89 on subscribe); the SPA itself has
  no local cap. Any dashboard fetched through the store already passed the cap, so the claim is
  true for every loadable dashboard; only a hand-edited *bundled* default could exceed it (the
  shipped one is 14 fields) and would then hit a clean server refusal.

## Empirical checks run

- `npx tsc --noEmit` in `apps/telemetry`: PASS (exit 0).
- `rg from_world_focus|from_world` across `crates/ apps/ xtask/`: only `from_world` callers;
  `from_world_focus` dead.
- Orbit clip math recomputed (see Finding 5).
- No test files / no node CI step (find + grep across `.github/`, `xtask/`).
