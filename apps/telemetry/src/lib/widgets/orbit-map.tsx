// orbit-map — a schematic SVG orbit: the parent's body disc, the
// conic ellipse from `elements`, and the vessel marker from pos_m.
// Schematic, not authoritative — f32-rendered analog of the sim's
// f64 truth (it reads the same serialized telemetry the wire ships).
//
// Projection convention (verified against `cartesian_to_elements`'s
// `R3(Ω)·R1(i)·R3(ω)` perifocal→inertial rotation in
// csp-sim's orbit.rs): the ellipse is drawn **in-plane** — the conic's
// perifocal frame mapped flat, semi-major toward periapsis. The vessel
// marker is therefore projected the same way: pos_m is rotated by the
// *inverse* elements rotation into perifocal `(x_p, y_p)` — for a real
// conic `z_p ≈ 0` identically, so the marker rides the drawn ellipse
// for any inclination. Limitation: the drawing carries no sense of
// tilt — an inclined orbit looks like an equatorial one here (the
// marker's out-of-plane component is dropped, which is exact while
// the vessel is on its conic).
import type { WidgetProps } from './registry'

function num(v: unknown): number | null {
  return typeof v === 'number' && Number.isFinite(v) ? v : null
}

interface Elements {
  semi_major_axis_m?: number
  eccentricity?: number
  inclination_rad?: number
  ascending_node_rad?: number
  arg_periapsis_rad?: number
}

export function OrbitMapWidget({ def, frame }: WidgetProps) {
  const elementsPath: string | undefined = def.fields[0]
  const posPath: string | undefined = def.fields[1]
  const unknown = frame?.unknown_fields ?? []
  const elements = (
    elementsPath !== undefined ? frame?.payload[elementsPath] : null
  ) as Elements | null | undefined
  const pos = (
    posPath !== undefined ? frame?.payload[posPath] : null
  ) as { x?: number; y?: number; z?: number } | null | undefined

  if (elementsPath === undefined || unknown.includes(elementsPath)) {
    return (
      <div className="widget-unknown flex-1">
        unknown field {elementsPath}
      </div>
    )
  }
  const a = num(elements?.semi_major_axis_m) ?? 0
  const e = num(elements?.eccentricity) ?? 0
  const omega = num(elements?.arg_periapsis_rad) ?? 0
  const incl = num(elements?.inclination_rad) ?? 0
  const node = num(elements?.ascending_node_rad) ?? 0
  if (frame === null) {
    // The stream went stale — no live frame — which is not the same as
    // the vessel being off its conic.
    return <div className="widget-unknown flex-1">stale — no live frame</div>
  }
  if (elements === null || elements === undefined) {
    return <div className="widget-unknown flex-1">no conic — vessel not on rails</div>
  }
  if (a <= 0) {
    // `a < 0` is the hyperbolic/parabolic encoding — a valid conic,
    // just not an ellipse this schematic draws.
    return <div className="widget-unknown flex-1">open conic — not drawn</div>
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
  // Vessel marker: rotate pos_m into perifocal (undo R3(Ω)·R1(i)·R3(ω)
  // — i.e. apply R3(−ω)·R1(−i)·R3(−Ω)), keep the in-plane components,
  // and map perifocal → screen with the ellipse's convention:
  // screen = R3(−ω) applied to (x_p, −y_p). For i = Ω = 0 this reduces
  // to (x, −y) — the reference-plane projection.
  let vx: number | null = null
  let vy: number | null = null
  const posUnknown = posPath === undefined || unknown.includes(posPath)
  if (pos && !posUnknown) {
    const x = num(pos.x)
    const y = num(pos.y)
    const z = num(pos.z)
    if (x !== null && y !== null && z !== null) {
      const co = Math.cos(node)
      const so = Math.sin(node)
      const ci = Math.cos(incl)
      const si = Math.sin(incl)
      // undo R3(Ω)
      const x1 = co * x + so * y
      const y1 = -so * x + co * y
      // undo R1(i)
      const x2 = x1
      const y2 = ci * y1 + si * z
      // undo R3(ω) → perifocal (xp toward periapsis, yp along motion)
      const xp = cos * x2 + sin * y2
      const yp = -sin * x2 + cos * y2
      vx = 100 + (xp * cos - yp * sin) * scale
      vy = 80 + (-xp * sin - yp * cos) * scale
    }
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
      {vx !== null && vy !== null && (
        <circle cx={vx} cy={vy} r="3" style={{ fill: 'var(--good)' }} />
      )}
      {posUnknown && (
        <text
          x="100"
          y="156"
          textAnchor="middle"
          style={{ fill: 'var(--bad)', fontSize: 8 }}
        >
          {posPath === undefined
            ? 'position: no fields[1] configured — dashboard gap'
            : `position: unknown field ${posPath}`}
        </text>
      )}
    </svg>
  )
}
