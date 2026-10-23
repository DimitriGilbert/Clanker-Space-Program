// graph — a rolling sparkline of fields[0] over the last
// `window_s` seconds of frames (default 60 s of samples).
import { useRef, useEffect } from 'react'
import type { WidgetProps } from './registry'
import type { TelemetryFrame } from '../ws'

function num(v: unknown): number | null {
  return typeof v === 'number' && Number.isFinite(v) ? v : null
}

export function GraphWidget({ def, frame }: WidgetProps) {
  const path = def.fields[0]
  const windowS = num(def.options.window_s) ?? 60
  const rate = 16 // samples/s assumed for window sizing
  const cap = Math.max(16, Math.floor(windowS * rate))
  const buf = useRef<number[]>([])

  useEffect(() => {
    const v = num(frame?.payload[path])
    if (v !== null) {
      buf.current.push(v)
      if (buf.current.length > cap) buf.current.splice(0, buf.current.length - cap)
    }
  }, [frame, path, cap])

  const data = buf.current
  const lo = data.length ? Math.min(...data) : 0
  const hi = data.length ? Math.max(...data) : 1
  const span = hi - lo || 1
  const pts = data
    .map((v, i) => `${(i / Math.max(1, data.length - 1)) * 100},${30 - ((v - lo) / span) * 28}`)
    .join(' ')

  return (
    <div className="flex-1 flex flex-col">
      <div className="text-sm" style={{ color: 'var(--muted)' }}>
        {data.length ? data[data.length - 1].toFixed(2) : '—'}
      </div>
      <svg viewBox="0 0 100 32" preserveAspectRatio="none" className="flex-1 w-full">
        <polyline className="graph-line" points={pts} vectorEffect="non-scaling-stroke" />
      </svg>
    </div>
  )
}
