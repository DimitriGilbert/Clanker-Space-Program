// dashboard.test.ts — the field-path surface and the loadDashboard
// fallback classification: which dashboard the widgets paint depends
// on WHICH notice category a fetch outcome maps to, so each branch is
// pinned. `fetch` is stubbed — no server.
import { afterEach, describe, expect, it, vi } from 'vitest'
import {
  listDashboards,
  loadDashboard,
  subscriptionFields,
  type Dashboard,
} from './dashboard'

function dash(fields: string[]): Dashboard {
  return {
    schema_version: 1,
    name: 'x',
    title: 'X',
    rate_hz: 16,
    widgets: [
      {
        id: 'w',
        kind: 'table',
        title: 'W',
        fields,
        rect: { x: 0, y: 0, w: 4, h: 2 },
        options: {},
      },
    ],
  }
}

function stubFetch(handler: (path: string) => Promise<Response>) {
  vi.stubGlobal(
    'fetch',
    vi.fn((input: RequestInfo | URL) => handler(String(input))),
  )
}

afterEach(() => {
  vi.unstubAllGlobals()
})

describe('subscriptionFields', () => {
  it('is the sorted, deduped field-path union', () => {
    const d = dash(['focus.altitude_m', 'tick'])
    d.widgets.push({
      id: 'w2',
      kind: 'gauge',
      title: 'W2',
      fields: ['focus.altitude_m', 'warp_level'],
      rect: { x: 0, y: 0, w: 2, h: 1 },
      options: {},
    })
    expect(subscriptionFields(d)).toEqual([
      'focus.altitude_m',
      'tick',
      'warp_level',
    ])
  })
})

describe('listDashboards', () => {
  it('returns the stored names', async () => {
    stubFetch(async () =>
      Response.json({ dashboards: ['default', 'program', 'night-shift'] }),
    )
    expect(await listDashboards()).toEqual([
      'default',
      'program',
      'night-shift',
    ])
  })

  it('degrades to [] on refusal, unreachable store, and bad bodies', async () => {
    stubFetch(async () => new Response('nope', { status: 503 }))
    expect(await listDashboards()).toEqual([])
    stubFetch(async () => {
      throw new Error('offline')
    })
    expect(await listDashboards()).toEqual([])
    stubFetch(async () => Response.json({ dashboards: 'oops' }))
    expect(await listDashboards()).toEqual([])
    stubFetch(async () => Response.json(['a-is-not-an-object']))
    expect(await listDashboards()).toEqual([])
  })
})

describe('loadDashboard', () => {
  it('serves the stored document when the schema speaks', async () => {
    const stored = dash(['tick'])
    stubFetch(async () => Response.json(stored))
    const loaded = await loadDashboard('x')
    expect(loaded.source).toBe('server')
    expect(loaded.dashboard).toEqual(stored)
    expect(loaded.notice).toBeUndefined()
  })

  it('falls back per failure class with a named notice', async () => {
    const cases: Array<[() => Promise<Response>, RegExp]> = [
      // first run — nothing stored under the name
      [async () => new Response('x', { status: 404 }), /no 'x' dashboard stored/],
      // auth — the token is wrong/expired
      [async () => new Response('x', { status: 401 }), /unauthorized/],
      // store not configured on the server
      [async () => new Response('x', { status: 503 }), /not configured/],
      // server reachable but the body isn't a dashboard
      [async () => new Response('<html>', { status: 200 }), /malformed/],
      // a dashboard from a different schema epoch
      [
        async () => Response.json({ ...dash(['tick']), schema_version: 99 }),
        /schema_version 99/,
      ],
      // anything else
      [async () => new Response('x', { status: 500 }), /HTTP 500/],
    ]
    for (const [res, notice] of cases) {
      stubFetch(res)
      const loaded = await loadDashboard('x')
      expect(loaded.source).toBe('bundled')
      expect(loaded.notice).toMatch(notice)
    }
    // And the genuinely-unreachable shape — fetch rejecting.
    stubFetch(async () => {
      throw new Error('offline')
    })
    const loaded = await loadDashboard('x')
    expect(loaded.source).toBe('bundled')
    expect(loaded.notice).toMatch(/unreachable/)
  })

  it('bundled default keys focus.*/program.*, never a vessel index (C28-F1/F2)', async () => {
    // A 404 forces the bundled document — assert its declared fields
    // ride the re-keying indirection rather than a monotonic id, and
    // that the program layer is actually on the default dashboard.
    stubFetch(async () => new Response('x', { status: 404 }))
    const { dashboard } = await loadDashboard('default')
    const fields = subscriptionFields(dashboard)
    expect(fields.some((f) => f.startsWith('focus.'))).toBe(true)
    expect(fields.every((f) => !/^vessels\.\d+\./.test(f))).toBe(true)
    expect(fields.some((f) => f.startsWith('program.'))).toBe(true)
  })
})
