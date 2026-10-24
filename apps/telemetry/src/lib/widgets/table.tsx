// table — every declared field as a label/value row.
import type { WidgetProps } from './registry'

function render(v: unknown): string {
  if (v === null || v === undefined) return '—'
  if (typeof v === 'number') return Number.isInteger(v) ? String(v) : v.toFixed(3)
  if (typeof v === 'object') return JSON.stringify(v)
  return String(v)
}

export function TableWidget({ def, frame }: WidgetProps) {
  return (
    <div className="flex-1 overflow-auto">
      <table className="w-full text-sm" style={{ fontVariantNumeric: 'tabular-nums' }}>
        <tbody>
          {def.fields.map((f) => {
            const missing = frame?.unknown_fields.includes(f) ?? false
            return (
              <tr key={f}>
                <td className="py-0.5 pr-2" style={{ color: 'var(--muted)' }}>
                  {f.split('.').pop()}
                </td>
                <td className="py-0.5 text-right">
                  {missing ? (
                    <span className="widget-unknown">unknown field</span>
                  ) : frame ? (
                    render(frame.payload[f])
                  ) : (
                    '—'
                  )}
                </td>
              </tr>
            )
          })}
        </tbody>
      </table>
    </div>
  )
}
