// M9d §2.12.2 — the sidecar dashboard document. The shape mirrors
// csp-server's dashboard.rs (schema v1); documents are stored by the
// server under $config_dir/csp/dashboards/*.json and fetched through
// /api/dashboards. A dashboard is non-authoritative: it selects and
// lays out fields, never mutates the sim.

import { cspToken } from './ws'

export const DASHBOARD_SCHEMA_VERSION = 1

export interface WidgetRect {
  x: number
  y: number
  w: number
  h: number
}

export type WidgetKind = 'gauge' | 'graph' | 'table' | 'orbit-map'

export interface WidgetDef {
  id: string
  kind: WidgetKind
  title: string
  /** §2.12.2 subscription paths the widget reads. */
  fields: string[]
  rect: WidgetRect
  options: Record<string, unknown>
}

export interface Dashboard {
  schema_version: number
  name: string
  title: string
  rate_hz: number
  widgets: WidgetDef[]
}

/** The field-path union — what the WS `subscribe` sends. */
export function subscriptionFields(d: Dashboard): string[] {
  return [...new Set(d.widgets.flatMap((w) => w.fields))].sort()
}

async function apiFetch(path: string, init?: RequestInit): Promise<Response> {
  return fetch(path, {
    ...init,
    headers: {
      Authorization: `Bearer ${cspToken()}`,
      'Content-Type': 'application/json',
      ...(init?.headers ?? {}),
    },
  })
}

export type DashboardSource = 'server' | 'bundled'

export interface LoadedDashboard {
  dashboard: Dashboard
  /** Where the layout came from — the server's store or the bundled
   *  default fallback. */
  source: DashboardSource
  /** Why the bundled default is in use, when it is — surfaced in the
   *  page header rather than silently swallowed. */
  notice?: string
}

async function bundledDefault(): Promise<Dashboard> {
  return (await import('../../dashboards/default.json')).default as Dashboard
}

/** Load the named dashboard. Falls back to the bundled default for a
 *  first run (404 / unconfigured store), and reports *why* — the
 *  distinction between "nothing stored", "store not configured",
 *  "auth failed", and "server unreachable" matters to the user. */
export async function loadDashboard(name: string): Promise<LoadedDashboard> {
  let res: Response
  try {
    res = await apiFetch(`/api/dashboards/${name}`)
  } catch {
    return {
      dashboard: await bundledDefault(),
      source: 'bundled',
      notice: 'dashboard store unreachable — showing the bundled default',
    }
  }
  if (res.ok) {
    // A 200 with a non-JSON body must not reject loadDashboard — an
    // unhandled parse failure leaves the dashboard permanently blank
    // with no notice. Surface it as a schema/parse mismatch instead.
    let dash: Dashboard
    try {
      dash = (await res.json()) as Dashboard
    } catch {
      return {
        dashboard: await bundledDefault(),
        source: 'bundled',
        notice: `dashboard '${name}' returned a malformed (non-JSON) body — showing the bundled default`,
      }
    }
    if (dash?.schema_version === DASHBOARD_SCHEMA_VERSION) {
      return { dashboard: dash, source: 'server' }
    }
    return {
      dashboard: await bundledDefault(),
      source: 'bundled',
      notice: `dashboard '${name}' is schema_version ${String(
        dash?.schema_version,
      )} — this client speaks ${DASHBOARD_SCHEMA_VERSION}; showing the bundled default`,
    }
  }
  const notice =
    res.status === 401 || res.status === 403
      ? `dashboard fetch unauthorized (HTTP ${res.status}) — check the access token; showing the bundled default`
      : res.status === 404
        ? `no '${name}' dashboard stored (first run) — showing the bundled default`
        : res.status === 503
          ? 'dashboard store not configured on the server — showing the bundled default'
          : `dashboard fetch failed (HTTP ${res.status}) — showing the bundled default`
  return { dashboard: await bundledDefault(), source: 'bundled', notice }
}
