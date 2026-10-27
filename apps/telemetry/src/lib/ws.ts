// M9d §2.12.2 — the WS subscription client. Implements the wire
// protocol the server's /ws endpoint speaks:
//   → {proto:1, kind:"subscribe", fields:[…], rate_hz:16}
//   ← {proto:1, kind:"subscribed"|"telemetry"|"error"|"command_ack", …}
// Backpressure is drop-to-latest — frames are consumed as they
// arrive and only the newest payload is ever surfaced.
//
// Auth (§2.12.3): csp-server serves the SPA shell **token-free** — the
// token is never injected into what it serves. The SPA therefore
// obtains the per-run bearer token out of band: the `?csp_token=`
// URL param (stored in this tab's sessionStorage, then stripped from
// the address bar), sessionStorage from a prior entry, the legacy
// `csp-token` meta tag, or `VITE_CSP_TOKEN` under `vite dev`. When
// none is present the page renders a first-boot token gate.

export const WIRE_PROTO = 1
/** The 64-Hz divisors — pinned to the server's `VALID_RATES`
 *  (csp-server/src/dashboard.rs); a stored `rate_hz` outside the set
 *  is refused there, so it is refused here too rather than sent. */
export const VALID_RATES = [64, 32, 16, 8, 4, 2, 1]

export interface TelemetryFrame {
  seq: number
  tick: number
  /** field path → value */
  payload: Record<string, unknown>
  unknown_fields: string[]
}

export type ConnectionState =
  | 'connecting'
  | 'reconnecting'
  | 'live'
  | 'closed'
  | 'error'

export interface ReconnectPolicy {
  /** Extra socket attempts after the first drop (default 5). */
  maxAttempts: number
  /** First retry delay, ms — doubles per attempt (default 500). */
  baseDelayMs: number
  /** Retry-delay ceiling, ms (default 8000). */
  maxDelayMs: number
}

export interface TelemetryClientOptions {
  /** The declared field paths — the union of the dashboard's widgets. */
  fields: string[]
  /** A 64-Hz divisor (default 16). */
  rateHz?: number
  /** The latest frame — or `null` when the stream goes stale (the
   *  socket closed/errored and no live frame remains). */
  onFrame: (frame: TelemetryFrame | null) => void
  /** The server's negotiated rate from the `subscribed` ack. */
  onRate?: (rateHz: number) => void
  onState?: (state: ConnectionState, detail?: string) => void
  /** Bounded reconnect for transient drops. NOTE: a server *restart*
   *  rotates the bearer token, so reconnecting can only recover a
   *  transient socket drop — after a restart the token gate asks for
   *  the new token. `false` disables retries. */
  reconnect?: Partial<ReconnectPolicy> | false
}

const DEFAULT_RECONNECT: ReconnectPolicy = {
  maxAttempts: 5,
  baseDelayMs: 500,
  maxDelayMs: 8000,
}

// --- the token channel -----------------------------------------------------

const TOKEN_STORAGE_KEY = 'csp-token'
const TOKEN_URL_PARAM = 'csp_token'

/** `?csp_token=` in the page URL: stash per-tab, then strip so the
 *  credential does not linger in the address bar or history. */
function urlToken(): string | null {
  try {
    const url = new URL(window.location.href)
    const t = url.searchParams.get(TOKEN_URL_PARAM)
    if (!t) return null
    window.sessionStorage.setItem(TOKEN_STORAGE_KEY, t)
    url.searchParams.delete(TOKEN_URL_PARAM)
    window.history.replaceState(null, '', url)
    return t
  } catch {
    return null
  }
}

/** `VITE_CSP_TOKEN` — a `vite dev` convenience only. The
 *  `import.meta.env.DEV` gate is static: Vite defines it to `false`
 *  in production builds, the minifier drops the dead arm, and the
 *  served bundle resolves to `''` unconditionally. `/` and
 *  `/assets/*` are served token-free (§2.12.3, csp-server's
 *  http.rs), so a baked-in bearer would be a public secret —
 *  `scripts/finalize.mjs` fails the build if one still leaks. */
function devEnvToken(): string {
  if (!import.meta.env.DEV) return ''
  return import.meta.env.VITE_CSP_TOKEN ?? ''
}

/** The bearer token for `/ws`'s `?token=` and `/api/*`'s
 *  `Authorization` header — `''` when none has been provided (the
 *  page then renders the token gate). */
export function cspToken(): string {
  if (typeof window === 'undefined' || typeof document === 'undefined') {
    return ''
  }
  const fromUrl = urlToken()
  if (fromUrl) return fromUrl
  try {
    const stored = window.sessionStorage.getItem(TOKEN_STORAGE_KEY)
    if (stored) return stored
  } catch {
    /* sessionStorage can be disabled — fall through */
  }
  // Legacy: a served shell that still injects the meta tag.
  const meta = document
    .querySelector('meta[name="csp-token"]')
    ?.getAttribute('content')
  if (meta) return meta
  return devEnvToken()
}

/** Store a token entered at the first-boot gate (this tab only — the
 *  secret rotates per server start and must never outlive it). */
export function setCspToken(token: string): void {
  try {
    window.sessionStorage.setItem(TOKEN_STORAGE_KEY, token)
  } catch {
    /* best effort — an unset token just re-asks */
  }
}

/** Drop the stored token — the "reset token" affordance, and the path
 *  back to the gate after a server restart rotated the secret. */
export function clearCspToken(): void {
  try {
    window.sessionStorage.removeItem(TOKEN_STORAGE_KEY)
  } catch {
    /* ignore */
  }
}

/** Open the subscription; returns a close handle. On a socket drop the
 *  frame is reported stale (`onFrame(null)`), then bounded reconnect
 *  re-opens and re-subscribes with the same fields — see
 *  [`TelemetryClientOptions.reconnect`] for the server-restart caveat. */
export function subscribeTelemetry(opts: TelemetryClientOptions): () => void {
  // A persisted/hand-assembled `rate_hz` outside the pinned divisor
  // set would just earn a server refusal — degrade to the default
  // locally so a bad stored value still subscribes.
  const rate =
    opts.rateHz !== undefined && VALID_RATES.includes(opts.rateHz)
      ? opts.rateHz
      : 16
  const url = new URL('/ws', window.location.href)
  url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:'
  url.searchParams.set('token', cspToken())
  url.searchParams.set('client', 'telemetry-spa')

  const policy: ReconnectPolicy | null =
    opts.reconnect === false
      ? null
      : { ...DEFAULT_RECONNECT, ...(opts.reconnect ?? {}) }

  let stopped = false
  let attempts = 0
  let timer: ReturnType<typeof setTimeout> | undefined
  let ws: WebSocket | undefined

  const connect = () => {
    if (stopped) return
    ws = new WebSocket(url)
    opts.onState?.(attempts > 0 ? 'reconnecting' : 'connecting')
    ws.onopen = () => {
      ws?.send(
        JSON.stringify({
          proto: WIRE_PROTO,
          kind: 'subscribe',
          fields: opts.fields,
          rate_hz: rate,
        }),
      )
    }
    ws.onmessage = (ev) => {
      let msg: any
      try {
        msg = JSON.parse(String(ev.data))
      } catch {
        return
      }
      // Inbound `proto` gate: the server stamps `proto` on every
      // envelope and enforces it client→server; a mismatch is a
      // wire-version break (the `proto` bump contract), never a frame
      // to dispatch on `kind` alone.
      if (msg?.proto !== WIRE_PROTO) {
        // A wire-version break means no usable stream — mark the frame
        // stale too, so the UI doesn't paint live-looking data under
        // the error state.
        opts.onFrame(null)
        opts.onState?.(
          'error',
          `wire proto ${String(msg?.proto)} — client speaks ${WIRE_PROTO}`,
        )
        return
      }
      if (msg.kind === 'telemetry') {
        opts.onFrame({
          seq: msg.seq ?? 0,
          tick: msg.tick ?? 0,
          payload: msg.payload ?? {},
          unknown_fields: msg.unknown_fields ?? [],
        })
      } else if (msg.kind === 'subscribed') {
        // A live ack, not `open`, resets the retry budget — a socket
        // that dies mid-handshake must not buy back attempts.
        attempts = 0
        opts.onRate?.(Number(msg.rate_hz) || rate)
        opts.onState?.('live')
      } else if (msg.kind === 'error') {
        // A protocol-level refusal (bad fields/rate/proto) means no
        // stream follows — mark the frame stale, don't keep painting.
        opts.onFrame(null)
        opts.onState?.('error', String(msg.error ?? 'server error'))
      }
    }
    ws.onerror = () => {
      // Same stale rule as onclose — an errored socket has no live
      // frame to keep painting.
      opts.onFrame(null)
      opts.onState?.('error', 'socket error')
    }
    ws.onclose = () => {
      // The last frame is stale the moment the socket drops — a dead
      // stream must not keep painting live-looking data.
      opts.onFrame(null)
      if (stopped) return
      if (policy && attempts < policy.maxAttempts) {
        const delay = Math.min(
          policy.baseDelayMs * 2 ** attempts,
          policy.maxDelayMs,
        )
        attempts += 1
        opts.onState?.(
          'closed',
          `retry ${attempts}/${policy.maxAttempts} in ${Math.round(delay / 100) / 10}s`,
        )
        timer = setTimeout(connect, delay)
      } else {
        opts.onState?.(
          'closed',
          policy ? 'gave up reconnecting' : undefined,
        )
      }
    }
  }

  connect()
  return () => {
    stopped = true
    if (timer !== undefined) clearTimeout(timer)
    ws?.close()
  }
}
