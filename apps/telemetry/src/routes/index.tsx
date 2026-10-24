import { createFileRoute } from '@tanstack/react-router'
import { useEffect, useRef, useState } from 'react'
import { loadDashboard, subscriptionFields, type Dashboard } from '~/lib/dashboard'
import {
  clearCspToken,
  cspToken,
  setCspToken,
  subscribeTelemetry,
  type ConnectionState,
  type TelemetryFrame,
} from '~/lib/ws'
import { WIDGET_REGISTRY } from '~/lib/widgets/registry'
import { Card, CardContent, CardHeader } from '~/components/ui/card'

export const Route = createFileRoute('/')({ component: DashboardPage })

function DashboardPage() {
  const [token, setToken] = useState(() => cspToken())
  const [dash, setDash] = useState<Dashboard | null>(null)
  const [notice, setNotice] = useState<string | null>(null)
  const [conn, setConn] = useState<ConnectionState>('connecting')
  const [connDetail, setConnDetail] = useState('')
  const [rateHz, setRateHz] = useState(16)
  const frame = useRef<TelemetryFrame | null>(null)
  const [, force] = useState(0)

  useEffect(() => {
    if (!token) return
    let close: (() => void) | undefined
    let cancelled = false
    loadDashboard('default').then((loaded) => {
      if (cancelled) return
      setDash(loaded.dashboard)
      setNotice(loaded.notice ?? null)
      close = subscribeTelemetry({
        fields: subscriptionFields(loaded.dashboard),
        rateHz: loaded.dashboard.rate_hz,
        onFrame: (f) => {
          frame.current = f
          force((n) => n + 1)
        },
        onRate: (hz) => setRateHz(hz),
        onState: (s, detail) => {
          setConn(s)
          setConnDetail(detail ?? '')
        },
      })
    })
    return () => {
      cancelled = true
      close?.()
    }
  }, [token])

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
        <button
          type="button"
          className="text-xs ml-auto"
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
              key={w.id}
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
