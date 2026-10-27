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
import { asRecord, num } from '../utils'

interface Elements {
  semi_major_axis_m?: number
  eccentricity?: number
  inclination_rad?: number
  ascending_node_rad?: number
  arg_periapsis_rad?: number
}

interface Vec3 {
  x?: number
  y?: number
  z?: number
}

/** The wire-value → `Elements` decode: a record's numeric fields,
 *  anything else is `null` (the "no conic" branch). Replaces the old
 *  `as` cast — the payload off the wire is `unknown`, not typed. */
function asElements(v: unknown): Elements | null {
  const r = asRecord(v)
  if (r === null) return null
  return {
    semi_major_axis_m: num(r.semi_major_axis_m) ?? undefined,
    eccentricity: num(r.eccentricity) ?? undefined,
    inclination_rad: num(r.inclination_rad) ?? undefined,
    ascending_node_rad: num(r.ascending_node_rad) ?? undefined,
    arg_periapsis_rad: num(r.arg_periapsis_rad) ?? undefined,
  }
}

/** Same decode for `pos_m` — `{x,y,z}` numbers or `null`. */
function asVec3(v: unknown): Vec3 | null {
  const r = asRecord(v)
  if (r === null) return null
  return {
    x: num(r.x) ?? undefined,
    y: num(r.y) ?? undefined,
    z: num(r.z) ?? undefined,
  }
}

export function OrbitMapWidget({ def, frame }: WidgetProps) {
  const elementsPath: string | undefined = def.fields[0]
  const posPath: string | undefined = def.fields[1]
  const unknown = frame?.unknown_fields ?? []
  const elements =
    elementsPath !== undefined ? asElements(frame?.payload[elementsPath]) : null
  const pos = posPath !== undefined ? asVec3(frame?.payload[posPath]) : null

  if (elementsPath === undefined) {
    // Same explanatory shape the fields[1] branch below uses — a
    // dashboard that declares no elements path is a config gap, not
    // a resolved-but-unknown field.
    return (
      <div className="widget-unknown flex-1">
        elements: no fields[0] configured — dashboard gap
      </div>
    )
  }
  if (unknown.includes(elementsPath)) {
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
  // periapsis direction, scaled so apoapsis — `ra`, the ellipse's
  // farthest point from the focus — fits BOTH viewbox axes with the
  // same 12 px margin (focus sits at (100, 80) in a 200×160 box:
  // x-budget 88, y-budget 68). The old x-only scale clipped
  // near-circular orbits top/bottom by up to 8 px — a parking orbit
  // rendered as a cut ring (C28-F5).
  const b = a * Math.sqrt(Math.max(0, 1 - e * e))
  const ra = a * (1 + e)
  const c = a * e
  const scale = Math.min(88, 68) / ra
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
    const x = pos.x ?? null
    const y = pos.y ?? null
    const z = pos.z ?? null
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
