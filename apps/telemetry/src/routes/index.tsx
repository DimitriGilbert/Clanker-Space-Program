import { createFileRoute } from '@tanstack/react-router'
import { useEffect, useRef, useState } from 'react'
import { loadDashboard, subscriptionFields, type Dashboard } from '~/lib/dashboard'
import { subscribeTelemetry, type ConnectionState, type TelemetryFrame } from '~/lib/ws'
import { WIDGET_REGISTRY } from '~/lib/widgets/registry'
import { Card, CardContent, CardHeader } from '~/components/ui/card'

export const Route = createFileRoute('/')({ component: DashboardPage })

function DashboardPage() {
  const [dash, setDash] = useState<Dashboard | null>(null)
  const [conn, setConn] = useState<ConnectionState>('connecting')
  const frame = useRef<TelemetryFrame | null>(null)
  const [, force] = useState(0)

  useEffect(() => {
    let close: (() => void) | undefined
    let cancelled = false
    loadDashboard('default').then((d) => {
      if (cancelled) return
      setDash(d)
      close = subscribeTelemetry({
        fields: subscriptionFields(d),
        rateHz: d.rate_hz,
        onFrame: (f) => {
          frame.current = f
          force((n) => n + 1)
        },
        onState: (s) => setConn(s),
      })
    })
    return () => {
      cancelled = true
      close?.()
    }
  }, [])

  return (
    <div className="p-4 max-w-[1400px] mx-auto">
      <header className="flex items-center gap-3 mb-4">
        <h1 className="text-lg font-semibold">{dash?.title ?? 'Telemetry'}</h1>
        <span className={`status-dot ${conn === 'live' ? 'live' : 'dead'}`} />
        <span className="text-xs" style={{ color: 'var(--muted)' }}>
          {conn}
          {frame.current ? ` · tick ${frame.current.tick}` : ''}
        </span>
      </header>
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
                  <C def={w} frame={frame.current} />
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
