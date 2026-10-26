# Phase 7 — telemetry soak note (C21-F1 / C21-F2)

Status: **code complete; live soak pending** — the fix gate requires a
long-running soak (plan: ≥1 h) against a live `csp-server`, which needs a
running server + browser session and cannot be executed as part of this
implementation pass. This note records what changed, the expected soak
behaviour, and the pass criteria.

## What changed

- `apps/telemetry/src/lib/widgets/graph.tsx`
  - Rolling buffer is hard-bounded at `MAX_POINTS = 2048`
    (`cap = Math.min(MAX_POINTS, Math.max(16, floor(window_s * rate)))`).
    A stored `window_s: 3600` at 64 Hz previously asked for 230,400
    samples — V8 throws `RangeError` on `Math.min(...data)` past ~125k
    (verified-report-21, empirically reproduced). No stored config can
    now grow the buffer past 2k points.
  - Min/max is a single loop pass (`O(n)` work, `O(1)` stack) instead of
    `Math.min(...data)`/`Math.max(...data)` spreads — safe at any cap.
- `apps/telemetry/src/routes/index.tsx`
  - `onFrame` no longer calls `force()` per WS message. Each message
    stores into `frame.current` and schedules one `requestAnimationFrame`
    if none is pending — a 64 Hz subscription now renders at display
    cadence (≤60 fps, typically one render per vsync covering up to
    ~1 frame per message) instead of one full dashboard re-render per
    message. Effect cleanup cancels a pending rAF.
  - Drop-to-latest (the ws.ts contract) is now honoured on the render
    side: `frame.current` always holds the newest frame; intermediate
    frames in a burst are never painted.
  - Hidden-tab behaviour: rAF does not fire in background tabs, so a
    backgrounded dashboard does zero render work; the newest frame paints
    on return. Previously renders kept queuing while hidden.

## Soak protocol (for the validating run)

1. `cargo run -p csp-server -- serve` (or the packaged binary); note the
   per-run token at `$XDG_RUNTIME_DIR/csp/<pid>.token`.
2. Serve the built SPA (`apps/telemetry/dist`, or `npm run preview` /
   `vite dev`); open `?csp_token=<token>` once.
3. Dashboard at the default 16 Hz; also run one pass at `rate_hz: 64`
   with a graph widget carrying `window_s: 3600` (the previously-bricking
   config from verified-report-21 F1).
4. Leave running ≥1 h. Pass criteria:
   - No blank page / unmounted tree (no `RangeError` in devtools console).
   - Sparkline keeps advancing; tick counter in the header updates.
   - Tab responsiveness stays normal (no long-task warnings dominating;
     Chrome devtools Performance tab should show ≤1 React commit per
     vsync at 64 Hz ingress, not 64 commits/s).
   - After backgrounding the tab ≥5 min and returning, the dashboard
     paints the *current* tick (no backlog catch-up burst).

## Static verification already done

- `npx tsc --noEmit` — clean.
- `npm run build` (vite build + finalize) — clean.
- The `frame === null` stale-frame path is unchanged: `onFrame(null)`
   lands in `frame.current` and the same coalesced rAF paints it, so the
   graph's pre-gap buffer clear still runs on the stale render.
