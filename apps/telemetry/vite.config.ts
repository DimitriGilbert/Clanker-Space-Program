import { tanstackStart } from '@tanstack/react-start/plugin/vite'
import { defineConfig } from 'vite'
import viteReact from '@vitejs/plugin-react'
import tailwindcss from '@tailwindcss/vite'

// M9d §2.12.2 — the telemetry SPA. Built as a *static* bundle
// (`spaMode` — no SSR/server functions; csp-server serves the
// output's index.html + /assets/* verbatim — the shell is token-free,
// the SPA prompts for the bearer token once).
//
// `vite dev` needs a path to csp-server: the dev server proxies `/ws`
// (the subscription socket) and `/api/*` (the sidecar dashboard API)
// at `CSP_SERVER_URL` (default 127.0.0.1:47900, csp-server's fixed
// port). `changeOrigin` rewrites `Host`/`Origin` to the target so the
// server's DNS-rebinding allowlist passes. The bearer token is still
// required — the SPA appends `?token=` on `/ws` and `Authorization`
// on `/api`; supply it via `?csp_token=<token>` in the dev URL once,
// `VITE_CSP_TOKEN`, or the in-page gate (the token file is
// `$XDG_RUNTIME_DIR/csp/<pid>.token`, first line).
const cspServerUrl = process.env.CSP_SERVER_URL ?? 'http://127.0.0.1:47900'

export default defineConfig({
  resolve: {
    tsconfigPaths: true,
  },
  server: {
    proxy: {
      '/ws': { target: cspServerUrl, ws: true, changeOrigin: true },
      '/api': { target: cspServerUrl, changeOrigin: true },
    },
  },
  plugins: [
    tailwindcss(),
    tanstackStart({
      srcDirectory: 'src',
      // The client env's HTML entry — csp-server serves dist/client as
      // static files (index.html + assets/*), so the SPA gets a real
      // entry document, not just chunks.
      spa: { enabled: true, prerender: { enabled: true } },
    }),
    viteReact(),
  ],
})
