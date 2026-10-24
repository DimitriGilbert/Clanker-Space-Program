# csp-telemetry — the telemetry SPA (M9d, PLAN §2.12.2)

TanStack Start + Tailwind + shadcn-style components + a custom CSS layer,
consuming `csp-server`'s `/ws` subscription stream.

## Build

```bash
npm install        # exact pins — no carets, §2.12.2's upgrade rule
npm run build      # vite build + finalize → dist/client/{index.html,assets/*}
```

The output is **static**: `vite.config.ts` sets `spa: { enabled: true,
prerender }` so the client build emits `_shell.html` + `assets/`;
`scripts/finalize.mjs` promotes the shell to `index.html`. Point
`csp-server`'s `ServerConfig::spa_dir` at `dist/client` — the server
serves `index.html` at `/` **verbatim** and the bundle at `/assets/*`.
The shell is token-free by design: the SPA asks for csp-server's
per-run bearer token once (the gate reads `$XDG_RUNTIME_DIR/csp/
<pid>.token`, first line) and keeps it in the tab's sessionStorage.
`?csp_token=<token>` in the URL is the non-interactive channel.

## Dev

`npm run dev` (port 47901) proxies `/ws` + `/api` at `CSP_SERVER_URL`
(default `http://127.0.0.1:47900`) — run csp-server alongside it and
open `http://localhost:47901/?csp_token=<token>` once, or export
`VITE_CSP_TOKEN` before `npm run dev`.

## Shape

- `src/lib/ws.ts` — the §2.12.2 wire client: `{proto, kind:"subscribe",
  fields, rate_hz}` → telemetry frames; drop-to-latest rendering.
- `src/lib/dashboard.ts` — the sidecar dashboard schema (schema v1,
  mirror of `csp-server`'s `dashboard.rs`) + `/api/dashboards` fetch.
- `src/lib/widgets/` — the widget registry: `gauge`, `graph`, `table`,
  `orbit-map` (the kind set is mirrored server-side in
  `dashboard::WIDGET_KINDS`).
- `dashboards/default.json` — the bundled default layout, used when the
  store has no `default` dashboard.
- `src/components/ui/` — shadcn-convention vendored components.

## Conventions

- Field paths are the server's subscription paths
  (`"tick"`, `"vessels.0.altitude_m"`, …) — a widget's `fields` are its
  subscription slice; the page subscribes once with the union.
- `rate_hz` is a 64-divisor (16 default) — the §2.12.2 rate rule.
- Node toolchain is pinned by `package.json` exact versions (no carets)
  per §2.12.2; upgrades are deliberate commits.
