import { tanstackStart } from '@tanstack/react-start/plugin/vite'
import { defineConfig } from 'vite'
import viteReact from '@vitejs/plugin-react'
import tailwindcss from '@tailwindcss/vite'

// M9d §2.12.2 — the telemetry SPA. Built as a *static* bundle
// (`spaMode` — no SSR/server functions; csp-server serves the
// output's index.html + /assets/* and injects the auth token).
export default defineConfig({
  resolve: {
    tsconfigPaths: true,
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
