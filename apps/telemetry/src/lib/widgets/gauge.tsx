// gauge — a single-field numeric readout with unit + optional
// min/max bar. fields[0] is the value.
import type { WidgetProps } from './registry'
import { num } from '../utils'

export function GaugeWidget({ def, frame }: WidgetProps) {
  const path = def.fields[0]
  const raw = frame?.payload[path]
  const value = num(raw)
  const unit = String(def.options.unit ?? '')
  const min = num(def.options.min) ?? 0
  const max = num(def.options.max)
  const frac = value !== null && max !== null && max > min
    ? Math.min(1, Math.max(0, (value - min) / (max - min)))
    : null
  const missing = frame?.unknown_fields.includes(path)
  return (
    <div className="flex-1 flex flex-col justify-center">
      {value === null ? (
        missing ? (
          <div className="widget-unknown">unknown field {path}</div>
        ) : raw !== null && raw !== undefined ? (
          // The path resolved but isn't a number — a type mismatch,
          // not "no data".
          <div className="widget-unknown">non-numeric {path}</div>
        ) : (
          <div className="widget-value" style={{ color: 'var(--muted)' }}>—</div>
        )
      ) : (
        <>
          <div>
            <span className="widget-value">{value.toFixed(1)}</span>
            <span className="widget-unit">{unit}</span>
          </div>
          {frac !== null && (
            <div className="mt-2 h-1.5 rounded" style={{ background: 'var(--border)' }}>
              <div
                className="h-1.5 rounded"
                style={{ width: `${frac * 100}%`, background: 'var(--accent)' }}
              />
            </div>
          )}
        </>
      )}
    </div>
  )
}
