// graph — a rolling sparkline of fields[0] over the last
// `window_s` seconds of frames (default 60 s of samples).
import { useRef, useEffect } from 'react'
import type { WidgetProps } from './registry'

function num(v: unknown): number | null {
  return typeof v === 'number' && Number.isFinite(v) ? v : null
}

export function GraphWidget({ def, frame, rateHz }: WidgetProps) {
  const path = def.fields[0]
  const windowS = num(def.options.window_s) ?? 60
  // The server's negotiated rate — the `subscribed` ack's rate_hz,
  // plumbed through WidgetProps — sizes the rolling window.
  const rate = rateHz > 0 ? rateHz : 16
  const cap = Math.max(16, Math.floor(windowS * rate))
  const buf = useRef<number[]>([])

  useEffect(() => {
    if (frame === null) {
      // Stale/reconnect boundary: drop pre-gap samples so post-gap
      // samples don't join them contiguously — the sparkline must not
      // draw a continuous line across a dropout.
      buf.current = []
      return
    }
    const v = num(frame.payload[path])
    if (v !== null) {
      buf.current.push(v)
      if (buf.current.length > cap) buf.current.splice(0, buf.current.length - cap)
    }
  }, [frame, path, cap])

  const unknown = frame?.unknown_fields.includes(path) ?? false
  const raw = frame?.payload[path]
  const nonNumeric = !unknown && raw !== null && raw !== undefined && num(raw) === null

  // Render-time guard as well: the effect above clears the buffer
  // *after* this render commits, so without it the frame===null render
  // would still paint the stale pre-gap line.
  const data = frame === null ? [] : buf.current
  const lo = data.length ? Math.min(...data) : 0
  const hi = data.length ? Math.max(...data) : 1
  const span = hi - lo || 1
  const pts = data
    .map((v, i) => `${(i / Math.max(1, data.length - 1)) * 100},${30 - ((v - lo) / span) * 28}`)
    .join(' ')

  return (
    <div className="flex-1 flex flex-col">
      <div className="text-sm" style={{ color: 'var(--muted)' }}>
        {unknown ? (
          <span className="widget-unknown">unknown field {path}</span>
        ) : data.length ? (
          data[data.length - 1].toFixed(2)
        ) : nonNumeric ? (
          // The path resolves but isn't numeric — a type mismatch,
          // not silence.
          <span className="widget-unknown">non-numeric {path}</span>
        ) : (
          '—'
        )}
      </div>
      <svg viewBox="0 0 100 32" preserveAspectRatio="none" className="flex-1 w-full">
        <polyline className="graph-line" points={pts} vectorEffect="non-scaling-stroke" />
      </svg>
    </div>
  )
}
