// orbit-map — a schematic SVG orbit: the parent's body disc, the
// conic ellipse from `elements`, and the vessel marker from pos_m.
// Schematic, not authoritative — f32-rendered analog of the sim's
// f64 truth (it reads the same serialized telemetry the wire ships).
import type { WidgetProps } from './registry'

function num(v: unknown): number | null {
  return typeof v === 'number' && Number.isFinite(v) ? v : null
}

interface Elements {
  semi_major_axis_m?: number
  eccentricity?: number
  arg_periapsis_rad?: number
}

export function OrbitMapWidget({ def, frame }: WidgetProps) {
  const elements = (frame?.payload[def.fields[0]] ?? null) as Elements | null
  const pos = (frame?.payload[def.fields[1]] ?? null) as
    | { x?: number; y?: number; z?: number }
    | null
  const a = num(elements?.semi_major_axis_m) ?? 0
  const e = num(elements?.eccentricity) ?? 0
  const omega = num(elements?.arg_periapsis_rad) ?? 0
  if (a <= 0) {
    return <div className="widget-unknown flex-1">no conic — vessel not on rails</div>
  }
  // Project the conic into the SVG frame: semi-major along the
  // periapsis direction, scaled so apoapsis fits the viewbox.
  const b = a * Math.sqrt(Math.max(0, 1 - e * e))
  const ra = a * (1 + e)
  const c = a * e
  const scale = 88 / ra
  const cos = Math.cos(omega)
  const sin = Math.sin(omega)
  // Ellipse centre offset from focus by c toward -periapsis.
  const cx = 100 - c * cos * scale
  const cy = 80 + c * sin * scale
  // Vessel marker: in-plane projection (x→right, z→down screen).
  let vx = 100
  let vy = 80
  if (pos) {
    const px = num(pos.x) ?? 0
    const pz = num(pos.z) ?? 0
    vx = 100 + px * scale
    vy = 80 + pz * scale
  }
  return (
    <svg viewBox="0 0 200 160" className="flex-1 w-full">
      <circle cx="100" cy="80" r="12" style={{ fill: 'var(--accent-warm)' }} />
      <ellipse
        cx={cx}
        cy={cy}
        rx={a * scale}
        ry={b * scale}
        transform={`rotate(${(-omega * 180) / Math.PI} ${cx} ${cy})`}
        style={{ fill: 'none', stroke: 'var(--accent)', strokeWidth: 1.2 }}
      />
      <circle cx={vx} cy={vy} r="3" style={{ fill: 'var(--good)' }} />
    </svg>
  )
}
