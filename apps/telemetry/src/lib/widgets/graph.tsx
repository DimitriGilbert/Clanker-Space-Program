// graph — a rolling sparkline of fields[0] over the last
// `window_s` seconds of frames (default 60 s of samples).
import { useRef, useEffect } from 'react'
import type { WidgetProps } from './registry'
import { num } from '../utils'

/** The rolling buffer's hard bound (C21-F1). `window_s` is free-form
 *  dashboard JSON — `window_s: 3600` at 64 Hz asks for 230,400
 *  samples, far more than a ~100 px sparkline can draw and enough
 *  for the per-render O(cap) min/max + polyline-string passes to
 *  dominate the frame. The cap clamps the *buffer*, never the
 *  option: the widget still honours the window up to this many
 *  samples. */
const MAX_POINTS = 2048

export function GraphWidget({ def, frame, rateHz }: WidgetProps) {
  const path = def.fields[0]
  const windowS = num(def.options.window_s) ?? 60
  // The server's negotiated rate — the `subscribed` ack's rate_hz,
  // plumbed through WidgetProps — sizes the rolling window.
  const rate = rateHz > 0 ? rateHz : 16
  const cap = Math.min(MAX_POINTS, Math.max(16, Math.floor(windowS * rate)))
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
  // Loop-based min/max (C21-F1): `Math.min(...data)` spreads the
  // whole buffer onto the call stack and throws `RangeError` past
  // ~125k elements on V8 — a stored `window_s` could push the buffer
  // over that. One pass is O(n) work with O(1) stack at any cap.
  let lo = 0
  let hi = 1
  if (data.length) {
    lo = data[0]
    hi = data[0]
    for (let i = 1; i < data.length; i++) {
      const v = data[i]
      if (v < lo) lo = v
      else if (v > hi) hi = v
    }
  }
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
