import { createFileRoute } from '@tanstack/react-router'
import { useEffect, useRef, useState } from 'react'
import {
  listDashboards,
  loadDashboard,
  subscriptionFields,
  type Dashboard,
} from '~/lib/dashboard'
import {
  clearCspToken,
  cspToken,
  setCspToken,
  subscribeTelemetry,
  type ConnectionState,
  type TelemetryFrame,
  type TelemetryHandle,
} from '~/lib/ws'
import { WIDGET_REGISTRY } from '~/lib/widgets/registry'
import { Card, CardContent, CardHeader } from '~/components/ui/card'

export const Route = createFileRoute('/')({ component: DashboardPage })

function DashboardPage() {
  const [token, setToken] = useState(() => cspToken())
  const [dashName, setDashName] = useState('default')
  const [dashList, setDashList] = useState<string[]>([])
  const [dash, setDash] = useState<Dashboard | null>(null)
  const [notice, setNotice] = useState<string | null>(null)
  const [conn, setConn] = useState<ConnectionState>('connecting')
  const [connDetail, setConnDetail] = useState('')
  const [rateHz, setRateHz] = useState(16)
  const frame = useRef<TelemetryFrame | null>(null)
  // The live subscription handle — the "retry" affordance's target.
  // A ref (not state): the handle's identity never drives render.
  const client = useRef<TelemetryHandle | null>(null)
  // The pending-rAF handle for render coalescing (C21-F2): one
  // animation frame collapses a burst of WS messages into a single
  // re-render — see `onFrame` below.
  const raf = useRef<number | null>(null)
  const [, force] = useState(0)

  // The dashboard store's name list — powers the switcher. `[]`
  // (unconfigured/refused store) just hides it; `default` is always
  // selectable regardless.
  useEffect(() => {
    if (!token) return
    let cancelled = false
    listDashboards().then((names) => {
      if (!cancelled) setDashList(names)
    })
    return () => {
      cancelled = true
    }
  }, [token])

  useEffect(() => {
    if (!token) return
    let handle: TelemetryHandle | undefined
    let cancelled = false
    loadDashboard(dashName).then((loaded) => {
      if (cancelled) return
      setDash(loaded.dashboard)
      setNotice(loaded.notice ?? null)
      handle = subscribeTelemetry({
        fields: subscriptionFields(loaded.dashboard),
        rateHz: loaded.dashboard.rate_hz,
        onFrame: (f) => {
          frame.current = f
          // Coalesce renders to the display's cadence (C21-F2): each
          // WS message is a distinct macrotask, so React batching
          // can't merge them — a 64 Hz subscription forced a full
          // dashboard re-render per message. The stream's contract is
          // drop-to-latest: only `frame.current` ever paints, so one
          // queued rAF per burst is the honest rendering of it. A
          // hidden tab simply skips renders (rAF doesn't fire) and
          // paints the newest frame on return.
          if (raf.current === null) {
            raf.current = requestAnimationFrame(() => {
              raf.current = null
              force((n) => n + 1)
            })
          }
        },
        onRate: (hz) => setRateHz(hz),
        onState: (s, detail) => {
          setConn(s)
          setConnDetail(detail ?? '')
        },
      })
      client.current = handle
    })
    return () => {
      cancelled = true
      client.current = null
      handle?.close()
      // Cancel any queued coalesced render — a rAF that outlives the
      // subscription would force one last setState for a stream that
      // is already closed.
      if (raf.current !== null) {
        cancelAnimationFrame(raf.current)
        raf.current = null
      }
    }
  }, [token, dashName])

  if (!token) {
    return (
      <TokenGate
        onSubmit={(t) => {
          setCspToken(t)
          setToken(t)
        }}
      />
    )
  }

  return (
    <div className="p-4 max-w-[1400px] mx-auto">
      <header className="flex items-center gap-3 mb-4">
        <h1 className="text-lg font-semibold">{dash?.title ?? 'Telemetry'}</h1>
        <span className={`status-dot ${conn === 'live' ? 'live' : 'dead'}`} />
        <span className="text-xs" style={{ color: 'var(--muted)' }}>
          {conn}
          {connDetail ? ` — ${connDetail}` : ''}
          {frame.current ? ` · tick ${frame.current.tick}` : ' · no live frame'}
        </span>
        {/* The manual way back after the bounded reconnect gives up
            (C28-F6) — "reset token" → re-paste is still there for the
            rotated-secret case, but a transient drop shouldn't need
            the ceremony. Also a "retry now" while a backoff timer
            is pending. */}
        {(conn === 'closed' || conn === 'error') && (
          <button
            type="button"
            className="text-xs"
            style={{ color: 'var(--muted)', textDecoration: 'underline' }}
            onClick={() => client.current?.retry()}
          >
            retry
          </button>
        )}
        {/* The stored-dashboard switcher — `listDashboards` answers
            `[]` for an unconfigured/refused store, which still leaves
            the bundled 'default' selectable. Switching re-runs the
            load+subscribe effect via `dashName`. */}
        <select
          className="text-xs ml-auto"
          style={{ color: 'var(--muted)', background: 'transparent' }}
          value={dashName}
          onChange={(e) => setDashName(e.target.value)}
        >
          {['default', ...dashList.filter((n) => n !== 'default')].map((n) => (
            <option key={n} value={n}>
              {n}
            </option>
          ))}
        </select>
        <button
          type="button"
          className="text-xs"
          style={{ color: 'var(--muted)', textDecoration: 'underline' }}
          onClick={() => {
            clearCspToken()
            setToken('')
          }}
        >
          reset token
        </button>
      </header>
      {notice && <div className="dash-notice">{notice}</div>}
      <div className="dash-grid">
        {(dash?.widgets ?? []).map((w) => {
          const C = WIDGET_REGISTRY[w.kind]
          return (
            <Card
              // Keyed by dashboard AND widget id — a same-id widget in
              // a different dashboard must remount (graph buffers, or
              // any per-kind state, belong to the loaded document).
              key={`${dash?.name ?? ''}:${w.id}`}
              style={{
                gridColumn: `${w.rect.x + 1} / span ${w.rect.w}`,
                gridRow: `${w.rect.y + 1} / span ${w.rect.h}`,
              }}
            >
              <CardHeader>{w.title}</CardHeader>
              <CardContent>
                {C ? (
                  <C def={w} frame={frame.current} rateHz={rateHz} />
                ) : (
                  <div className="widget-unknown">unknown kind {w.kind}</div>
                )}
              </CardContent>
            </Card>
          )
        })}
      </div>
    </div>
  )
}

/** First-boot token gate — csp-server's per-run bearer token lives at
 *  `$XDG_RUNTIME_DIR/csp/<pid>.token` (first line); the served shell is
 *  token-free by design, so the SPA asks once and keeps the token in
 *  this tab's sessionStorage. `?csp_token=<token>` and `VITE_CSP_TOKEN`
 *  (vite dev) are the non-interactive channels. */
function TokenGate({ onSubmit }: { onSubmit: (token: string) => void }) {
  const [value, setValue] = useState('')
  return (
    <div className="p-4 max-w-md mx-auto mt-24">
      <h1 className="text-lg font-semibold mb-2">
        Clanker Space Program — Telemetry
      </h1>
      <p className="text-sm mb-3" style={{ color: 'var(--muted)' }}>
        csp-server requires its per-run bearer token. Read it from{' '}
        <code>$XDG_RUNTIME_DIR/csp/&lt;pid&gt;.token</code> (first line) and
        paste it below — it is kept in this tab's sessionStorage only. The
        token rotates on every server start; a restart means re-entering it.
      </p>
      <form
        className="flex gap-2"
        onSubmit={(e) => {
          e.preventDefault()
          const t = value.trim()
          if (t) onSubmit(t)
        }}
      >
        <input
          className="token-input"
          value={value}
          onChange={(e) => setValue(e.target.value)}
          placeholder="paste bearer token"
          autoFocus
        />
        <button type="submit" className="token-button">
          Connect
        </button>
      </form>
      <p className="text-xs mt-3" style={{ color: 'var(--muted)' }}>
        Non-interactive: open <code>?csp_token=&lt;token&gt;</code> once, or
        export <code>VITE_CSP_TOKEN</code> under <code>vite dev</code>.
      </p>
    </div>
  )
}
