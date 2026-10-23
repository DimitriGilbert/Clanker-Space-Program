// M9d §2.12.2 — the sidecar dashboard document. The shape mirrors
// csp-server's dashboard.rs (schema v1); documents are stored by the
// server under $config_dir/csp/dashboards/*.json and fetched through
// /api/dashboards. A dashboard is non-authoritative: it selects and
// lays out fields, never mutates the sim.

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

import { cspToken } from './ws'

/** Load the named dashboard — falls back to the bundled default
 *  when the store has none (first run) or the fetch fails. */
export async function loadDashboard(name: string): Promise<Dashboard> {
  try {
    const res = await apiFetch(`/api/dashboards/${name}`)
    if (res.ok) {
      const dash = (await res.json()) as Dashboard
      if (dash?.schema_version === DASHBOARD_SCHEMA_VERSION) return dash
    }
  } catch {
    /* fall through to the bundled default */
  }
  const def = (await import('../../dashboards/default.json')).default as Dashboard
  return def
}

/** Persist a dashboard via the sidecar API (PUT /api/dashboards/name). */
export async function saveDashboard(dash: Dashboard): Promise<boolean> {
  try {
    const res = await apiFetch(`/api/dashboards/${dash.name}`, {
      method: 'PUT',
      body: JSON.stringify(dash),
    })
    return res.ok
  } catch {
    return false
  }
}
