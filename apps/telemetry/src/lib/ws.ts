// M9d §2.12.2 — the WS subscription client. Implements the wire
// protocol the server's /ws endpoint speaks:
//   → {proto:1, kind:"subscribe", fields:[…], rate_hz:16}
//   ← {proto:1, kind:"subscribed"|"telemetry"|"error"|"command_ack", …}
// Backpressure is drop-to-latest — frames are consumed as they
// arrive and only the newest payload is ever surfaced.

export const WIRE_PROTO = 1
export const VALID_RATES = [64, 32, 16, 8, 4, 2, 1]

export interface TelemetryFrame {
  seq: number
  tick: number
  /** field path → value */
  payload: Record<string, unknown>
  unknown_fields: string[]
}

export type ConnectionState = 'connecting' | 'live' | 'closed' | 'error'

export interface TelemetryClientOptions {
  /** The declared field paths — the union of the dashboard's widgets. */
  fields: string[]
  /** A 64-Hz divisor (default 16). */
  rateHz?: number
  onFrame: (frame: TelemetryFrame) => void
  onState?: (state: ConnectionState, detail?: string) => void
}

/** The auth token the server injected into index.html. */
export function cspToken(): string {
  if (typeof document === 'undefined') return ''
  return (
    document
      .querySelector('meta[name="csp-token"]')
      ?.getAttribute('content') ?? ''
  )
}

/** Open the subscription; returns a close handle. Reconnects are the
 *  caller's decision (a dashboard re-subscribes on config change). */
export function subscribeTelemetry(opts: TelemetryClientOptions): () => void {
  const rate = opts.rateHz ?? 16
  const url = new URL('/ws', window.location.href)
  url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:'
  url.searchParams.set('token', cspToken())
  url.searchParams.set('client', 'telemetry-spa')

  const ws = new WebSocket(url)
  opts.onState?.('connecting')
  ws.onopen = () => {
    ws.send(
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
    if (msg?.kind === 'telemetry') {
      opts.onFrame({
        seq: msg.seq ?? 0,
        tick: msg.tick ?? 0,
        payload: msg.payload ?? {},
        unknown_fields: msg.unknown_fields ?? [],
      })
    } else if (msg?.kind === 'subscribed') {
      opts.onState?.('live')
    } else if (msg?.kind === 'error') {
      opts.onState?.('error', String(msg.error ?? 'server error'))
    }
  }
  ws.onerror = () => opts.onState?.('error', 'socket error')
  ws.onclose = () => opts.onState?.('closed')
  return () => ws.close()
}
